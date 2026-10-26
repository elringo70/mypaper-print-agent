//! Turns a spool payload into per-page measurements.
//!
//! One path for every input: decode or rasterize to pixels, then measure with
//! [`crate::classify`]. There is deliberately no second estimator and no
//! external process, so the same document always yields the same numbers.
//!
//! Supported payloads are PDF (pdfium), EMF (GDI playback on Windows) and
//! raster images. PostScript, XPS and PCL are rejected explicitly: they would
//! need an AGPL renderer, and a silent "unanalyzable" job is worse than a clear
//! error the operator can act on.

use crate::classify::{analyze_image, PageMetrics, Thresholds};
use crate::format::{detect_format, image_format_from_extension, SpoolFormat};
use crate::raster::thumbnail_png;
use mpa_domain::Coverage;
use thiserror::Error;

/// Server cap mirror (`printJobCreateSchema`, max 500 pages): analyzing more
/// is pointless because the upload would be rejected. Split the document.
pub const MAX_ANALYZE_PAGES: usize = 500;
/// Previews are a convenience, not the product: past this many pages the UI
/// shows a placeholder instead of holding hundreds of PNGs in memory.
pub const MAX_THUMBNAIL_PAGES: usize = 100;

#[derive(Debug, Error)]
pub enum AnalyzeError {
    #[error("unsupported spool format: {0:?}")]
    UnsupportedFormat(SpoolFormat),
    #[error("too many pages: {pages} (limit {MAX_ANALYZE_PAGES}); split the document")]
    TooManyPages { pages: usize },
    #[error("PDF analysis requires the pdfium engine, which is not available")]
    NoPdfEngine,
    #[error("emf: {0}")]
    Emf(#[from] crate::emf::EmfError),
    #[cfg(feature = "pdfium")]
    #[error("pdf: {0}")]
    Pdf(#[from] crate::pdf::RenderError),
    #[error("image decode: {0}")]
    Image(#[from] image::ImageError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct PageAnalysis {
    pub page_no: u32,
    pub coverage: Coverage,
    pub thumbnail_png: Vec<u8>,
    /// Full pixel metrics: page class, area coverage and the channel fractions
    /// the tier ladder bills on. `None` only if the bitmap could not be
    /// measured, which the caller must not read as "blank".
    pub metrics: Option<PageMetrics>,
}

#[derive(Debug, Clone)]
pub struct JobAnalysis {
    pub pages: Vec<PageAnalysis>,
    pub format: SpoolFormat,
}

/// Analyze a spool payload.
pub fn analyze_bytes(data: &[u8]) -> Result<JobAnalysis, AnalyzeError> {
    let format = detect_format(data);
    let pages = match format {
        SpoolFormat::Image => vec![image_page(data)?],
        SpoolFormat::Pdf => analyze_pdf(data)?,
        SpoolFormat::Emf => analyze_emf(data)?,
        other => return Err(AnalyzeError::UnsupportedFormat(other)),
    };
    Ok(JobAnalysis { pages, format })
}

/// Analyze a file on disk. Extension decides images; content decides the rest.
pub fn analyze_file(path: &std::path::Path) -> Result<JobAnalysis, AnalyzeError> {
    if image_format_from_extension(path) {
        let img = image::open(path)?;
        let metrics = analyze_image(&img);
        let coverage = metrics
            .map(|m| m.coverage())
            .unwrap_or(Coverage::new(0.0, 0.0, 0.0, 0.0));
        return Ok(JobAnalysis {
            pages: vec![PageAnalysis {
                page_no: 1,
                coverage,
                thumbnail_png: thumbnail_png(&img, 256),
                metrics,
            }],
            format: SpoolFormat::Image,
        });
    }
    let data = std::fs::read(path)?;
    analyze_bytes(&data)
}

/// Render selected pages of a payload to pixels. Used by the reprint path,
/// which must show the printer the same pages the analyzer priced.
pub fn render_pages(
    data: &[u8],
    format: SpoolFormat,
    pages: &[u32],
    dpi: u32,
) -> Result<Vec<image::RgbaImage>, AnalyzeError> {
    if pages.is_empty() {
        return Ok(Vec::new());
    }
    match format {
        SpoolFormat::Pdf => render_pdf_pages(data, pages, dpi),
        SpoolFormat::Emf => render_emf_pages(data, pages, dpi),
        SpoolFormat::Image => {
            let img = image::load_from_memory(data)?;
            Ok(vec![img.to_rgba8()])
        }
        other => Err(AnalyzeError::UnsupportedFormat(other)),
    }
}

fn image_page(data: &[u8]) -> Result<PageAnalysis, AnalyzeError> {
    let img = image::load_from_memory(data)?;
    let metrics = analyze_image(&img);
    let coverage = metrics
        .map(|m| m.coverage())
        .unwrap_or(Coverage::new(0.0, 0.0, 0.0, 0.0));
    Ok(PageAnalysis {
        page_no: 1,
        coverage,
        thumbnail_png: thumbnail_png(&img, 256),
        metrics,
    })
}

/// Previews past the budget render as placeholders (the UI already does).
fn wants_thumbnail(page_no: u32) -> bool {
    (page_no as usize) <= MAX_THUMBNAIL_PAGES
}

/// One measured page: pixel metrics, the coverage derived from them, and a
/// preview when the page is inside the thumbnail budget.
fn measured_page(page_no: u32, img: image::RgbaImage) -> PageAnalysis {
    let img = image::DynamicImage::ImageRgba8(img);
    let metrics = analyze_image(&img);
    let coverage = metrics
        .map(|m| m.coverage())
        .unwrap_or(Coverage::new(0.0, 0.0, 0.0, 0.0));
    let thumbnail_png = if wants_thumbnail(page_no) {
        thumbnail_png(&img, 256)
    } else {
        Vec::new()
    };
    PageAnalysis {
        page_no,
        coverage,
        thumbnail_png,
        metrics,
    }
}

#[cfg(feature = "pdfium")]
fn analyze_pdf(data: &[u8]) -> Result<Vec<PageAnalysis>, AnalyzeError> {
    let count = crate::pdf::page_count(data)?;
    check_page_cap(count)?;
    let dpi = Thresholds::default().dpi;
    let mut pages = Vec::with_capacity(count);
    for index in 0..count {
        let img = crate::pdf::render_page(data, index, dpi)?;
        pages.push(measured_page((index + 1) as u32, img));
    }
    Ok(pages)
}

#[cfg(not(feature = "pdfium"))]
fn analyze_pdf(_data: &[u8]) -> Result<Vec<PageAnalysis>, AnalyzeError> {
    Err(AnalyzeError::NoPdfEngine)
}

#[cfg(feature = "pdfium")]
fn render_pdf_pages(
    data: &[u8],
    pages: &[u32],
    dpi: u32,
) -> Result<Vec<image::RgbaImage>, AnalyzeError> {
    let mut out = Vec::with_capacity(pages.len());
    for page_no in pages {
        out.push(crate::pdf::render_page(data, *page_no as usize - 1, dpi)?);
    }
    Ok(out)
}

#[cfg(not(feature = "pdfium"))]
fn render_pdf_pages(
    _data: &[u8],
    _pages: &[u32],
    _dpi: u32,
) -> Result<Vec<image::RgbaImage>, AnalyzeError> {
    Err(AnalyzeError::NoPdfEngine)
}

#[cfg(windows)]
fn analyze_emf(data: &[u8]) -> Result<Vec<PageAnalysis>, AnalyzeError> {
    let split = crate::emf::split_pages(data)?;
    check_page_cap(split.len())?;
    let dpi = Thresholds::default().dpi;
    let mut pages = Vec::with_capacity(split.len());
    for (index, page) in split.iter().enumerate() {
        let img = crate::emf::rasterize_page(&page.bytes, page.width_mm, page.height_mm, dpi)?;
        pages.push(measured_page((index + 1) as u32, img));
    }
    Ok(pages)
}

/// EMF needs GDI playback; elsewhere the payload cannot be measured and the
/// caller is told so instead of getting a silent blank page.
#[cfg(not(windows))]
fn analyze_emf(_data: &[u8]) -> Result<Vec<PageAnalysis>, AnalyzeError> {
    Err(AnalyzeError::Emf(crate::emf::EmfError::NeedsWindows))
}

#[cfg(windows)]
fn render_emf_pages(
    data: &[u8],
    pages: &[u32],
    dpi: u32,
) -> Result<Vec<image::RgbaImage>, AnalyzeError> {
    let split = crate::emf::split_pages(data)?;
    let mut out = Vec::with_capacity(pages.len());
    for page_no in pages {
        let page = *page_no as usize;
        let emf = split
            .get(page.saturating_sub(1))
            .ok_or(AnalyzeError::TooManyPages { pages: page })?;
        out.push(crate::emf::rasterize_page(
            &emf.bytes,
            emf.width_mm,
            emf.height_mm,
            dpi,
        )?);
    }
    Ok(out)
}

#[cfg(not(windows))]
fn render_emf_pages(
    _data: &[u8],
    _pages: &[u32],
    _dpi: u32,
) -> Result<Vec<image::RgbaImage>, AnalyzeError> {
    Err(AnalyzeError::Emf(crate::emf::EmfError::NeedsWindows))
}

/// Rejects oversized documents before the expensive render.
fn check_page_cap(pages: usize) -> Result<(), AnalyzeError> {
    if pages > MAX_ANALYZE_PAGES {
        return Err(AnalyzeError::TooManyPages { pages });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_bytes_analyze_end_to_end() {
        // 2x1 PPM: one white pixel, one black pixel -> K = 50%.
        let ppm: Vec<u8> = b"P6\n2 1\n255\n\xFF\xFF\xFF\x00\x00\x00".to_vec();
        let analysis = analyze_bytes(&ppm).unwrap();
        assert_eq!(analysis.pages.len(), 1);
        assert!((analysis.pages[0].coverage.k - 50.0).abs() < 1e-3);
        assert!(!analysis.pages[0].thumbnail_png.is_empty());
        let metrics = analysis.pages[0].metrics.expect("image path measures");
        assert_eq!(metrics.class, crate::classify::PageClass::Mono);
    }

    #[test]
    fn page_cap_rejects_oversized_documents() {
        assert!(check_page_cap(MAX_ANALYZE_PAGES).is_ok());
        assert!(matches!(
            check_page_cap(MAX_ANALYZE_PAGES + 1),
            Err(AnalyzeError::TooManyPages { pages: 501 })
        ));
    }

    #[test]
    fn thumbnail_budget_keeps_first_hundred_pages() {
        assert!(wants_thumbnail(1));
        assert!(wants_thumbnail(MAX_THUMBNAIL_PAGES as u32));
        assert!(!wants_thumbnail(MAX_THUMBNAIL_PAGES as u32 + 1));
    }

    #[test]
    fn unknown_payload_fails_explicitly() {
        assert!(matches!(
            analyze_bytes(b"whatever"),
            Err(AnalyzeError::UnsupportedFormat(SpoolFormat::Unknown))
        ));
    }

    /// PostScript, XPS and PCL are out of scope by decision: they need an AGPL
    /// renderer. The error names the format instead of pretending it worked.
    #[test]
    fn non_free_formats_are_rejected_by_name() {
        for (payload, format) in [
            (b"%!PS-Adobe-3.0\n".to_vec(), SpoolFormat::PostScript),
            (vec![0x50, 0x4B, 0x03, 0x04, 0], SpoolFormat::Xps),
            (vec![0x1Bu8, b'E', 0x1B, b'&', b'l'], SpoolFormat::Pcl),
        ] {
            assert!(
                matches!(
                    analyze_bytes(&payload),
                    Err(AnalyzeError::UnsupportedFormat(f)) if f == format
                ),
                "{format:?} must be rejected by name"
            );
        }
    }

    #[cfg(not(feature = "pdfium"))]
    #[test]
    fn pdf_without_pdfium_says_so() {
        assert!(matches!(
            analyze_bytes(&minimal_pdf()),
            Err(AnalyzeError::NoPdfEngine)
        ));
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn blank_pdf_measures_as_blank() {
        if !crate::pdf::available() {
            return;
        }
        let analysis = analyze_bytes(&minimal_pdf()).expect("pdfium measures");
        assert_eq!(analysis.format, SpoolFormat::Pdf);
        assert_eq!(analysis.pages.len(), 1);
        let metrics = analysis.pages[0].metrics.expect("pdfium path measures");
        assert_eq!(metrics.class, crate::classify::PageClass::Blanco);
        assert!(!metrics.class.is_billable());
    }

    #[test]
    fn render_pages_rejects_unsupported_formats() {
        assert!(matches!(
            render_pages(b"\x1B E", SpoolFormat::Pcl, &[1], 150),
            Err(AnalyzeError::UnsupportedFormat(SpoolFormat::Pcl))
        ));
    }

    #[test]
    fn render_pages_of_nothing_is_empty() {
        assert!(render_pages(b"%PDF-1.4", SpoolFormat::Pdf, &[], 150)
            .unwrap()
            .is_empty());
    }

    /// Builds a minimal one-page blank PDF with a correct xref table.
    fn minimal_pdf() -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        let objects: &[&[u8]] = &[
            b"<< /Type /Catalog /Pages 2 0 R >>",
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        ];
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }
}
