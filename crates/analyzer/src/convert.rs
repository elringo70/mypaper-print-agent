use crate::emf::{self, EmfError};
use crate::format::SpoolFormat;
use crate::gs::{Ghostscript, GsError};
use crate::pdfgen::{images_to_pdf, PdfGenError, PdfRasterPage};
use crate::raster::encode_jpeg_rgba;
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("cannot normalize {0:?} to PDF: {1}")]
    Unsupported(SpoolFormat, String),
    #[error(transparent)]
    Ghostscript(#[from] GsError),
    #[error(transparent)]
    Emf(#[from] EmfError),
    #[error(transparent)]
    Pdf(#[from] PdfGenError),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Raster DPI for EMF playback. 150 keeps a carta page under ~2MP while
/// staying sharp enough for office reprints.
const EMF_DPI: u32 = 150;
const JPEG_QUALITY: u8 = 85;

/// Normalize any supported spool payload to a PDF so the subset pipeline
/// (`extract_pages_pdf` + `mswinpr2`) stays format-agnostic.
pub fn convert_to_pdf(
    data: &[u8],
    format: SpoolFormat,
    gs: &Ghostscript,
) -> Result<Vec<u8>, ConvertError> {
    match format {
        SpoolFormat::Pdf => Ok(data.to_vec()),
        SpoolFormat::PostScript => gs_convert(data, "ps", gs),
        SpoolFormat::Xps => gs_convert(data, "xps", gs),
        SpoolFormat::Pcl => pcl_convert(data),
        SpoolFormat::Emf => emf_convert(data),
        SpoolFormat::Image | SpoolFormat::Unknown => Err(ConvertError::Unsupported(
            format,
            "only PDF, PS, XPS, PCL and EMF reprint as subsets".into(),
        )),
    }
}

/// Ghostscript conversion with an extension-hinted temp file so the right
/// interpreter (PS vs gxps) is picked without sniffing surprises.
fn gs_convert(data: &[u8], ext: &str, gs: &Ghostscript) -> Result<Vec<u8>, ConvertError> {
    let dir = unique_temp_dir()?;
    let input = dir.join(format!("in.{ext}"));
    let output = dir.join("out.pdf");
    std::fs::write(&input, data)?;
    let result = gs.convert_to_pdf(&input, &output);
    let bytes = result.and_then(|()| {
        std::fs::read(&output).map_err(|e| GsError::Failed(format!("read back: {e}")))
    });
    let _ = std::fs::remove_dir_all(&dir);
    Ok(bytes?)
}

/// PCL needs the separate GhostPCL interpreter (`gpcl6`), not bundled with
/// the standard Ghostscript install. Looked up next to Ghostscript first.
fn pcl_convert(data: &[u8]) -> Result<Vec<u8>, ConvertError> {
    let gpcl = find_pcl_interpreter().ok_or_else(|| {
        ConvertError::Unsupported(
            SpoolFormat::Pcl,
            "GhostPCL (gpcl6) not found next to Ghostscript or on PATH".into(),
        )
    })?;
    let dir = unique_temp_dir()?;
    let input = dir.join("in.pcl");
    let output = dir.join("out.pdf");
    std::fs::write(&input, data)?;
    let status = Command::new(&gpcl)
        .args([
            "-dBATCH",
            "-dNOPAUSE",
            "-dQUIET",
            "-sDEVICE=pdfwrite",
            &format!("-o{}", output.display()),
        ])
        .arg(&input)
        .status()
        .map_err(|e| GsError::Failed(format!("spawn gpcl6: {e}")))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(GsError::Failed("gpcl6 conversion failed".into()).into());
    }
    let bytes = std::fs::read(&output).map_err(ConvertError::Io)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(bytes)
}

/// Locate GhostPCL (`gpcl6`) for PCL→PDF conversion. Checked next to the
/// Ghostscript binary first, then `MPA_GPCL`, then PATH.
///
/// The PATH probe uses `-h`: GhostPCL rejects `--version` (exit 1), so probing
/// it the way Ghostscript is probed reports "gpcl6 ausente" on a machine where
/// it is installed and on PATH.
pub fn find_pcl_interpreter() -> Option<PathBuf> {
    const NAMES: &[&str] = &["gpcl6win64.exe", "gpcl6win32.exe", "gpcl6.exe", "gpcl6"];

    if let Ok(env) = std::env::var("MPA_GPCL") {
        if !env.trim().is_empty() {
            return Some(PathBuf::from(env));
        }
    }
    // Beside the Ghostscript binary: the two tools ship together. When
    // Ghostscript itself was found through PATH its program is a bare name with
    // no parent directory, so the well-known install roots are scanned too.
    if let Ok(gs) = Ghostscript::discover() {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(dir) = gs.program().parent() {
            if !dir.as_os_str().is_empty() {
                dirs.push(dir.to_path_buf());
            }
        }
        dirs.extend(ghostscript_install_dirs());
        for dir in dirs {
            for name in NAMES {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    for name in NAMES {
        if probe_pcl(name) {
            return Some(PathBuf::from(name));
        }
    }
    None
}

/// `...\gs\gsX.Y\bin` under the standard install roots.
#[cfg(windows)]
fn ghostscript_install_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for root in ["C:\\Program Files", "C:\\Program Files (x86)"] {
        if let Ok(entries) = std::fs::read_dir(PathBuf::from(root).join("gs")) {
            for entry in entries.flatten() {
                dirs.push(entry.path().join("bin"));
            }
        }
    }
    dirs
}

#[cfg(not(windows))]
fn ghostscript_install_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// GhostPCL answers `-h` with its version banner and exit 0.
fn probe_pcl(name: &str) -> bool {
    Command::new(name)
        .arg("-h")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// EMF pages → GDI raster → JPEG → one PDF. Deterministic and testable
/// without Windows: the splitter and the PDF assembly are pure.
fn emf_convert(data: &[u8]) -> Result<Vec<u8>, ConvertError> {
    let pages = emf::split_pages(data)?;
    let mut raster = Vec::with_capacity(pages.len());
    for page in &pages {
        let img = emf::rasterize_page(&page.bytes, page.width_mm, page.height_mm, EMF_DPI)?;
        let (width_px, height_px) = (img.width(), img.height());
        let jpeg = encode_jpeg_rgba(&img.into_raw(), width_px, height_px, JPEG_QUALITY)?;
        raster.push(PdfRasterPage {
            width_mm: page.width_mm,
            height_mm: page.height_mm,
            width_px,
            height_px,
            jpeg,
        });
    }
    Ok(images_to_pdf(&raster)?)
}

fn unique_temp_dir() -> Result<PathBuf, std::io::Error> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mpa-convert-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_passes_through_untouched() {
        let pdf = b"%PDF-1.4 fake".to_vec();
        let gs = Ghostscript::for_tests();
        assert_eq!(convert_to_pdf(&pdf, SpoolFormat::Pdf, &gs).unwrap(), pdf);
    }

    #[test]
    fn unknown_and_images_are_rejected_with_format() {
        let gs = Ghostscript::for_tests();
        assert!(matches!(
            convert_to_pdf(b"???", SpoolFormat::Unknown, &gs),
            Err(ConvertError::Unsupported(SpoolFormat::Unknown, _))
        ));
        assert!(matches!(
            convert_to_pdf(b"???", SpoolFormat::Image, &gs),
            Err(ConvertError::Unsupported(SpoolFormat::Image, _))
        ));
    }

    #[test]
    fn pcl_without_gpcl6_says_what_is_missing() {
        // Deterministic unless the dev box happens to ship GhostPCL.
        std::env::remove_var("MPA_GPCL");
        if find_pcl_interpreter().is_some() {
            return;
        }
        let gs = Ghostscript::for_tests();
        let err = convert_to_pdf(b"\x1BE pcl", SpoolFormat::Pcl, &gs).unwrap_err();
        assert!(matches!(
            err,
            ConvertError::Unsupported(SpoolFormat::Pcl, _)
        ));
    }
}
