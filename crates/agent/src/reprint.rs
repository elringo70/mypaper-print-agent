use crate::core::OutboxEntry;
#[cfg(windows)]
use mpa_analyzer::print_pdf_to_winprinter;
use mpa_analyzer::{convert_to_pdf, extract_pages_pdf, Ghostscript, SpoolFormat};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReprintError {
    #[error("no cached spool file")]
    NoSpoolFile,
    #[error("subset reprint unsupported for {0:?} (PDF, PS, XPS, PCL, EMF only)")]
    UnsupportedFormat(SpoolFormat),
    #[error("normalize to PDF: {0}")]
    Convert(String),
    #[error("ghostscript unavailable: {0}")]
    Ghostscript(String),
    #[error("extract failed: {0}")]
    Extract(String),
    #[error("print failed: {0}")]
    Print(String),
    #[error("reprint only supported on Windows (set MPA_MOCK_PRINT_DIR for tests)")]
    UnsupportedPlatform,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// On-disk cache for spool bytes of analyzed jobs. The spooler deletes its
/// own copy once the held job is cancelled, so the agent keeps one to be
/// able to reprint a subset later.
pub fn spool_cache_dir() -> PathBuf {
    std::env::temp_dir().join("mpa-spool-cache")
}

pub fn cache_spool_bytes(
    printer: &str,
    job_id: u32,
    version: u64,
    data: &[u8],
) -> std::io::Result<PathBuf> {
    let dir = spool_cache_dir();
    std::fs::create_dir_all(&dir)?;
    let safe: String = printer
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = dir.join(format!("{safe}-{job_id}-{version}.bin"));
    std::fs::write(&path, data)?;
    Ok(path)
}

pub fn cleanup_spool_file(path: Option<&Path>) {
    if let Some(p) = path {
        let _ = std::fs::remove_file(p);
    }
}

/// Build the subset PDF with only the included pages of an entry.
pub fn build_subset_pdf(entry: &OutboxEntry) -> Result<Vec<u8>, ReprintError> {
    // Every supported spool format normalizes to PDF first, so subsetting
    // and delivery stay format-agnostic. Only Unknown has no path at all.
    match entry.format {
        Some(
            SpoolFormat::Pdf
            | SpoolFormat::PostScript
            | SpoolFormat::Xps
            | SpoolFormat::Emf
            | SpoolFormat::Pcl,
        ) => {}
        Some(f) => return Err(ReprintError::UnsupportedFormat(f)),
        None => return Err(ReprintError::NoSpoolFile),
    }
    let path = entry.spool_path.as_ref().ok_or(ReprintError::NoSpoolFile)?;
    let bytes = std::fs::read(path).map_err(|_| ReprintError::NoSpoolFile)?;
    let wanted: Vec<u32> = entry
        .pages
        .iter()
        .filter(|p| p.included)
        .map(|p| p.page_no)
        .collect();
    if wanted.is_empty() {
        return Err(ReprintError::NoSpoolFile);
    }
    let gs = Ghostscript::discover().map_err(|e| ReprintError::Ghostscript(e.to_string()))?;
    let format = entry.format.unwrap_or(SpoolFormat::Unknown);
    let pdf =
        convert_to_pdf(&bytes, format, &gs).map_err(|e| ReprintError::Convert(e.to_string()))?;
    extract_pages_pdf(&pdf, &wanted, &gs).map_err(|e| ReprintError::Extract(e.to_string()))
}

/// Deliver an already-built subset PDF to the original printer.
pub fn deliver_pdf(pdf: &[u8], entry: &OutboxEntry) -> Result<(), ReprintError> {
    #[cfg(windows)]
    {
        let gs = Ghostscript::discover().map_err(|e| ReprintError::Ghostscript(e.to_string()))?;
        let dir = spool_cache_dir();
        let file = dir.join(format!("{}-reprint.pdf", entry.idempotency_key));
        std::fs::write(&file, pdf)?;
        // Deliver to the role's printer when one is assigned (Fase 3), else to
        // the printer that received the original job. Print options (Fase 4)
        // travel with the entry so a queued job keeps its settings.
        let target = entry.deliver_printer.as_deref().unwrap_or(&entry.printer);
        let result = print_pdf_to_winprinter(&file, target, &gs, entry.options.as_ref())
            .map_err(|e| ReprintError::Print(e.to_string()));
        let _ = std::fs::remove_file(&file);
        result
    }

    #[cfg(not(windows))]
    {
        let dir =
            std::env::var("MPA_MOCK_PRINT_DIR").map_err(|_| ReprintError::UnsupportedPlatform)?;
        let file = PathBuf::from(dir).join(format!("{}-reprint.pdf", entry.idempotency_key));
        std::fs::write(&file, pdf).map_err(|e| ReprintError::Print(e.to_string()))?;
        Ok(())
    }
}

/// Outcome of one lock-free reprint sweep step (see `sweep_one_reprint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOutcome {
    Idle,
    Reprinted { key: String },
    Failed { key: String, error: String },
}

/// Attempt a single pending reprint WITHOUT holding the core lock during
/// Ghostscript/printer IO: the entry is cloned under a brief lock, printed
/// unlocked, then marked under a second brief lock. Safe to call from the
/// watcher loop, the UI poll or the drain — the UI thread never waits on a
/// subprocess this way.
pub fn sweep_one_reprint(
    core: &std::sync::Arc<std::sync::Mutex<crate::core::AgentCore>>,
) -> SweepOutcome {
    let Some(entry) = core
        .lock()
        .map(|core| core.claim_reprint_entry())
        .unwrap_or(None)
    else {
        return SweepOutcome::Idle;
    };
    let key = entry.idempotency_key.clone();
    match attempt_reprint(&entry) {
        Ok(()) => {
            if let Ok(mut core) = core.lock() {
                core.mark_reprinted(&key);
            }
            SweepOutcome::Reprinted { key }
        }
        Err(e) => SweepOutcome::Failed {
            key,
            error: e.to_string(),
        },
    }
}

/// Reprint the included pages of an outbox entry: extract the subset PDF,
/// then send it to the original printer. Returns an error (leaving the
/// entry pending) when anything is missing — never half-prints.
pub fn attempt_reprint(entry: &OutboxEntry) -> Result<(), ReprintError> {
    let subset = build_subset_pdf(entry)?;
    deliver_pdf(&subset, entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with_pages() -> OutboxEntry {
        OutboxEntry {
            idempotency_key: "test-1".into(),
            printer: "Mock-LaserJet".into(),
            job_id: 1,
            target: crate::core::TargetSelection::NewDraft,
            pages: vec![],
            format: Some(SpoolFormat::Pdf),
            spool_path: None,
            mode: mpa_domain::PrintMode::Bw,
            deliver_printer: None,
            options: None,
            applied_settings: None,
            reprint_pending: true,
            synced: false,
            remote_id: None,
            materialized: false,
        }
    }

    #[test]
    fn rejects_unsupported_format_before_touching_disk() {
        let mut entry = entry_with_pages();
        entry.format = Some(SpoolFormat::Unknown);
        assert!(matches!(
            attempt_reprint(&entry),
            Err(ReprintError::UnsupportedFormat(SpoolFormat::Unknown))
        ));
    }

    #[test]
    fn missing_cache_file_is_an_error_not_a_partial_print() {
        let entry = entry_with_pages();
        assert!(matches!(
            attempt_reprint(&entry),
            Err(ReprintError::NoSpoolFile)
        ));
    }

    #[test]
    fn cache_roundtrip_and_cleanup() {
        let path = cache_spool_bytes("HP LaserJet  Pro", 7, 3, b"%PDF-1.4 fake").unwrap();
        assert!(path.is_file());
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("HP_LaserJet__Pro-7-3"));
        cleanup_spool_file(Some(&path));
        assert!(!path.exists());
        // Missing file is tolerated.
        cleanup_spool_file(Some(&path));
        cleanup_spool_file(None);
    }

    #[cfg(not(windows))]
    #[test]
    fn mock_deliver_drops_pdf_without_ghostscript() {
        let dir = std::env::temp_dir().join("mpa-mock-print-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("MPA_MOCK_PRINT_DIR", &dir);
        let entry = entry_with_pages();
        deliver_pdf(b"%PDF-1.4 subset", &entry).unwrap();
        let dropped = dir.join("test-1-reprint.pdf");
        assert_eq!(std::fs::read(&dropped).unwrap(), b"%PDF-1.4 subset");
        std::env::remove_var("MPA_MOCK_PRINT_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
