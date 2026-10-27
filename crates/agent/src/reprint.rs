use crate::core::OutboxEntry;
use mpa_analyzer::{print_images, render_pages, SpoolFormat};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Reprints are rendered at 300 dpi: the customer already accepted the sheet,
/// so the reprint should look at least as good as the original.
const REPRINT_DPI: u32 = 300;

#[derive(Debug, Error)]
pub enum ReprintError {
    #[error("no cached spool file")]
    NoSpoolFile,
    #[error("no pages selected")]
    EmptySelection,
    #[error("subset reprint unsupported for {0:?}")]
    UnsupportedFormat(SpoolFormat),
    #[error("render failed: {0}")]
    Render(String),
    #[error("print failed: {0}")]
    Print(String),
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

/// Outcome of one lock-free reprint sweep step (see `sweep_one_reprint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOutcome {
    Idle,
    Reprinted { key: String },
    Failed { key: String, error: String },
}

/// Attempt a single pending reprint WITHOUT holding the core lock during
/// rendering or printer IO: the entry is cloned under a brief lock, printed
/// unlocked, then marked under a second brief lock. Safe to call from the
/// watcher loop, the UI poll or the drain — the UI thread never waits on a
/// printer this way.
pub fn sweep_one_reprint(
    core: &std::sync::Arc<std::sync::Mutex<crate::core::AgentCore>>,
) -> SweepOutcome {
    let Some(entry) = core
        .lock()
        .map(|mut core| core.claim_reprint_entry())
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
        Err(e) => {
            if let Ok(mut core) = core.lock() {
                core.mark_reprint_failed(&key);
            }
            SweepOutcome::Failed {
                key,
                error: e.to_string(),
            }
        }
    }
}

/// Pages that actually go on paper.
///
/// Included and not blank: a blank page is not printed and not charged, because
/// the sheet the customer left empty is not something the shop produces. The
/// following page moves up — that is what "no se imprime" means on paper.
pub fn printable_pages(entry: &OutboxEntry) -> Vec<u32> {
    entry
        .pages
        .iter()
        .filter(|p| p.included)
        .filter(|p| {
            !matches!(
                p.area.and_then(|a| a.class),
                Some(mpa_analyzer::PageClass::Blanco)
            )
        })
        .map(|p| p.page_no)
        .collect()
}

/// Reprint the included pages of an outbox entry.
///
/// Pages are rendered to pixels and printed as raster: that is the same view
/// the analyzer priced, and it needs no external interpreter. Print options
/// (paper size, duplex) are not applied on this path yet — the printer keeps
/// its own defaults and the entry still records what was asked for, so the
/// difference stays auditable.
pub fn attempt_reprint(entry: &OutboxEntry) -> Result<(), ReprintError> {
    let format = match entry.format {
        Some(f @ (SpoolFormat::Pdf | SpoolFormat::Emf | SpoolFormat::Image)) => f,
        Some(f) => return Err(ReprintError::UnsupportedFormat(f)),
        None => return Err(ReprintError::NoSpoolFile),
    };
    let path = entry.spool_path.as_ref().ok_or(ReprintError::NoSpoolFile)?;
    let bytes = std::fs::read(path).map_err(|_| ReprintError::NoSpoolFile)?;

    let wanted = printable_pages(entry);
    if wanted.is_empty() {
        return Err(ReprintError::EmptySelection);
    }

    let pages = render_pages(&bytes, format, &wanted, REPRINT_DPI)
        .map_err(|e| ReprintError::Render(e.to_string()))?;
    // Deliver to the role's printer when one is assigned (Fase 3), else to the
    // printer that received the original job.
    let target = entry.deliver_printer.as_deref().unwrap_or(&entry.printer);
    print_images(target, &pages, REPRINT_DPI).map_err(|e| ReprintError::Print(e.to_string()))
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
            reprint_attempts: 0,
            synced: false,
            remote_id: None,
            materialized: false,
        }
    }

    #[test]
    fn blank_pages_are_not_printed() {
        use crate::core::{OutboxPage, PageArea};
        let mut entry = entry_with_pages();
        entry.pages = vec![
            OutboxPage {
                page_no: 1,
                coverage: mpa_domain::Coverage::new(0.0, 0.0, 0.0, 10.0),
                tier_id: None,
                included: true,
                area: Some(PageArea {
                    area_pct: 5.0,
                    class: Some(mpa_analyzer::PageClass::Mono),
                }),
            },
            OutboxPage {
                page_no: 2,
                coverage: mpa_domain::Coverage::new(0.0, 0.0, 0.0, 0.0),
                tier_id: None,
                included: true,
                area: Some(PageArea {
                    area_pct: 0.0,
                    class: Some(mpa_analyzer::PageClass::Blanco),
                }),
            },
            OutboxPage {
                page_no: 3,
                coverage: mpa_domain::Coverage::new(0.0, 0.0, 0.0, 4.0),
                tier_id: None,
                included: false,
                area: Some(PageArea {
                    area_pct: 2.0,
                    class: Some(mpa_analyzer::PageClass::Mono),
                }),
            },
        ];
        assert_eq!(
            printable_pages(&entry),
            vec![1],
            "blank and unchecked stay out"
        );
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

    /// A one-pixel PGM image is a valid spool payload, which keeps this test
    /// free of PDF fixtures while still exercising render and delivery.
    #[cfg(not(windows))]
    #[test]
    fn reprint_renders_selected_pages_and_delivers_them() {
        let dir = std::env::temp_dir().join(format!("mpa-reprint-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("MPA_MOCK_PRINT_DIR", &dir);

        let spool = dir.join("spool.bin");
        std::fs::write(&spool, b"P5\n2 2\n255\n\x00\x00\x00\x00").unwrap();

        let mut entry = entry_with_pages();
        entry.format = Some(SpoolFormat::Image);
        entry.spool_path = Some(spool);
        entry.pages = vec![crate::core::OutboxPage {
            page_no: 1,
            coverage: mpa_domain::Coverage::new(0.0, 0.0, 0.0, 100.0),
            tier_id: None,
            included: true,
            area: None,
        }];

        attempt_reprint(&entry).expect("reprint");
        let delivered = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".png"))
            .count();
        assert_eq!(delivered, 1, "one page, one delivered sheet");

        std::env::remove_var("MPA_MOCK_PRINT_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
