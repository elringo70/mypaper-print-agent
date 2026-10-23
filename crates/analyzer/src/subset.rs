use crate::gs::{Ghostscript, GsError};
use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SubsetError {
    #[error(transparent)]
    Ghostscript(#[from] GsError),
    #[error("no pages selected")]
    Empty,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Group sorted page numbers into contiguous ranges: [1,2,3,5,7,8] →
/// [(1,3),(5,5),(7,8)]. Pure and unit-tested (also reused by the UI hint).
pub fn group_contiguous(mut pages: Vec<u32>) -> Vec<(u32, u32)> {
    pages.sort_unstable();
    pages.dedup();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for p in pages {
        match ranges.last_mut() {
            Some(last) if p == last.1 + 1 => last.1 = p,
            _ => ranges.push((p, p)),
        }
    }
    ranges
}

/// Rebuild a PDF with only `pages` (1-based), preserving order.
/// Implemented as range extraction + merge through `pdfwrite`, so it works
/// for any PDF Ghostscript reads (no page-tree surgery here).
pub fn extract_pages_pdf(
    pdf: &[u8],
    pages: &[u32],
    gs: &Ghostscript,
) -> Result<Vec<u8>, SubsetError> {
    let ranges = group_contiguous(pages.to_vec());
    if ranges.is_empty() {
        return Err(SubsetError::Empty);
    }
    let dir = unique_temp_dir()?;
    let input = dir.join("in.pdf");
    std::fs::write(&input, pdf)?;

    let mut parts: Vec<std::path::PathBuf> = Vec::new();
    for (i, (first, last)) in ranges.iter().enumerate() {
        let out = dir.join(format!("part-{i}.pdf"));
        // Single-page ranges still go through pdfwrite for uniformity.
        let status = Command::new(gs.program())
            .args([
                "-dBATCH",
                "-dNOPAUSE",
                "-dQUIET",
                &format!("-dFirstPage={first}"),
                &format!("-dLastPage={last}"),
                "-sDEVICE=pdfwrite",
                &format!("-o{}", out.display()),
            ])
            .arg(&input)
            .status()
            .map_err(|e| GsError::Failed(format!("spawn: {e}")))?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(GsError::Failed(format!("extract pages {first}-{last}")).into());
        }
        parts.push(out);
    }

    let merged = dir.join("merged.pdf");
    if parts.len() == 1 {
        std::fs::rename(&parts[0], &merged)?;
    } else {
        let status = Command::new(gs.program())
            .args(["-dBATCH", "-dNOPAUSE", "-dQUIET", "-sDEVICE=pdfwrite"])
            .arg(format!("-o{}", merged.display()))
            .args(&parts)
            .status()
            .map_err(|e| GsError::Failed(format!("spawn: {e}")))?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(GsError::Failed("merge parts".into()).into());
        }
    }
    let bytes = std::fs::read(&merged)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(bytes)
}

fn unique_temp_dir() -> Result<std::path::PathBuf, std::io::Error> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mpa-subset-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Send a PDF file to a Windows printer through Ghostscript's `mswinpr2`
/// device (printer must be the exact spooler name).
pub fn print_pdf_to_winprinter(
    pdf_path: &Path,
    printer: &str,
    gs: &Ghostscript,
    options: Option<&mpa_domain::PrintOptions>,
) -> Result<(), GsError> {
    let mut command = Command::new(gs.program());
    command.args([
        "-dBATCH",
        "-dNOPAUSE",
        "-dQUIET",
        "-sDEVICE=mswinpr2",
        &format!("-sOutputFile=%printer%{printer}"),
    ]);
    // Paper size / duplex: the only print characteristics mswinpr2 honours
    // (resolution and media type are not settable through this device).
    if let Some(options) = options {
        command.args(options.gs_args());
    }
    let status = command
        .arg(pdf_path)
        .status()
        .map_err(|e| GsError::Failed(format!("spawn: {e}")))?;
    if !status.success() {
        return Err(GsError::Failed(format!("mswinpr2 print on {printer}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_contiguous_ranges() {
        assert_eq!(
            group_contiguous(vec![1, 2, 3, 5, 7, 8]),
            vec![(1, 3), (5, 5), (7, 8)]
        );
        assert_eq!(group_contiguous(vec![3, 1, 2, 2]), vec![(1, 3)]);
        assert!(group_contiguous(vec![]).is_empty());
    }

    #[test]
    fn rejects_empty_selection() {
        let gs = Ghostscript::discover().unwrap_or(Ghostscript::for_tests());
        assert!(matches!(
            extract_pages_pdf(b"%PDF-1.4 stub", &[], &gs),
            Err(SubsetError::Empty)
        ));
    }
}
