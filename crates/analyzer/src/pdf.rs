//! PDF rasterization through pdfium.
//!
//! Replaces the Ghostscript dependency that made the analyzer's numbers depend
//! on whatever version the customer happened to have installed — and whose
//! AGPL licence is incompatible with shipping this product closed-source.
//! pdfium is BSD-3-Clause and its binaries are redistributable.
//!
//! pdfium is not thread-safe, so the instance is process-wide and every use is
//! serialized behind a mutex. That is also why the accessor never hands a
//! reference out of the lock.

use image::RgbaImage;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Where the shared library is expected when the environment does not say.
const ENV_VAR: &str = "MPA_PDFIUM";

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("pdfium is not available: {0}")]
    Unavailable(String),
    #[error("pdfium cannot open the document: {0}")]
    Open(String),
    #[error("document has no page {page} (it has {count})")]
    NoSuchPage { page: usize, count: usize },
    #[error("pdfium cannot render page {page}: {message}")]
    Render { page: usize, message: String },
}

struct Serialized(pdfium_render::prelude::Pdfium);

// SAFETY: every access goes through the process-wide mutex below. pdfium is
// documented as not thread-safe, so serialization is precisely what makes
// sharing the single instance sound; no reference ever leaves the lock.
unsafe impl Send for Serialized {}

static PDFIUM: OnceLock<Result<Mutex<Serialized>, String>> = OnceLock::new();

/// Candidate library locations, most specific first.
fn candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(explicit) = std::env::var(ENV_VAR) {
        if !explicit.trim().is_empty() {
            found.push(PathBuf::from(explicit));
        }
    }
    // Next to the executable: the installer drops it beside mpa.exe.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            found.push(pdfium_render::prelude::Pdfium::pdfium_platform_library_name_at_path(dir));
        }
    }
    // System search path.
    found.push(PathBuf::from(
        pdfium_render::prelude::Pdfium::pdfium_platform_library_name(),
    ));
    found
}

fn instance() -> Result<&'static Mutex<Serialized>, RenderError> {
    let slot = PDFIUM.get_or_init(|| {
        use pdfium_render::prelude::Pdfium;
        let mut errors = Vec::new();
        for path in candidates() {
            match Pdfium::bind_to_library(&path) {
                Ok(bindings) => return Ok(Mutex::new(Serialized(Pdfium::new(bindings)))),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        Err(errors.join("; "))
    });
    match slot {
        Ok(mutex) => Ok(mutex),
        Err(message) => Err(RenderError::Unavailable(message.clone())),
    }
}

/// Whether a usable pdfium is present. Cheaper than rendering, and lets the
/// pipeline choose a fallback instead of failing a job.
pub fn available() -> bool {
    instance().is_ok()
}

/// Number of pages in a PDF payload.
pub fn page_count(bytes: &[u8]) -> Result<usize, RenderError> {
    with_document(bytes, |doc| Ok(doc.pages().len() as usize))
}

/// Render one page (0-based) at `dpi` into RGBA pixels.
pub fn render_page(bytes: &[u8], page: usize, dpi: u32) -> Result<RgbaImage, RenderError> {
    with_document(bytes, |doc| {
        let count = doc.pages().len() as usize;
        let pdf_page = doc
            .pages()
            .get(page as i32)
            .map_err(|_| RenderError::NoSuchPage { page, count })?;
        let width_pt = pdf_page.width().value;
        let height_pt = pdf_page.height().value;
        let target_w = ((width_pt / 72.0) * dpi as f32).round().max(1.0) as i32;
        let target_h = ((height_pt / 72.0) * dpi as f32).round().max(1.0) as i32;
        let config = pdfium_render::prelude::PdfRenderConfig::new()
            .set_target_width(target_w)
            .set_target_height(target_h);
        let bitmap = pdf_page
            .render_with_config(&config)
            .map_err(|e| RenderError::Render {
                page,
                message: e.to_string(),
            })?;
        RgbaImage::from_raw(
            bitmap.width() as u32,
            bitmap.height() as u32,
            bitmap.as_rgba_bytes(),
        )
        .ok_or_else(|| RenderError::Render {
            page,
            message: "bitmap has an unexpected size".into(),
        })
    })
}

fn with_document<T>(
    bytes: &[u8],
    f: impl FnOnce(&pdfium_render::prelude::PdfDocument<'_>) -> Result<T, RenderError>,
) -> Result<T, RenderError> {
    let mutex = instance()?;
    // A poisoned lock only means a previous call panicked mid-render; the
    // library state is still valid for the next document.
    let guard = mutex.lock().unwrap_or_else(|e| e.into_inner());
    let doc = guard
        .0
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| RenderError::Open(e.to_string()))?;
    f(&doc)
}
