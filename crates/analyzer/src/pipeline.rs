use crate::classify::{analyze_image, PageMetrics, Thresholds};
use crate::convert::{convert_to_pdf, ConvertError};
use crate::format::{detect_format, image_format_from_extension, SpoolFormat};
use crate::gs::{Ghostscript, GsError};
use crate::raster::thumbnail_png;
use mpa_domain::Coverage;
use std::path::{Path, PathBuf};
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
    #[error("ghostscript: {0}")]
    Ghostscript(#[from] GsError),
    #[error("normalize to PDF: {0}")]
    Convert(#[from] ConvertError),
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
    /// Full pixel metrics, when the payload was actually measured rather than
    /// estimated by a device. Carries the page class and the area coverage the
    /// tier ladder bills on. `None` on the legacy PDF path until the renderer
    /// lands; callers must not infer a class from `Option`.
    pub metrics: Option<PageMetrics>,
}

#[derive(Debug, Clone)]
pub struct JobAnalysis {
    pub pages: Vec<PageAnalysis>,
    pub format: SpoolFormat,
}

/// Analyze a spool payload. PDF/PS go through Ghostscript `inkcov`
/// (true channel coverage) plus a 72 DPI render for previews; images go
/// through the in-house raster estimator. EMF, XPS and PCL payloads are
/// normalized to PDF first (GDI playback, gxps, GhostPCL) and then take the
/// very same PDF path.
pub fn analyze_bytes(data: &[u8], gs: Option<&Ghostscript>) -> Result<JobAnalysis, AnalyzeError> {
    let format = detect_format(data);
    let mut analysis = match format {
        SpoolFormat::Image => {
            let img = image::load_from_memory(data)?;
            let metrics = analyze_image(&img);
            let coverage = metrics
                .map(|m| m.coverage())
                .unwrap_or(Coverage::new(0.0, 0.0, 0.0, 0.0));
            JobAnalysis {
                pages: vec![PageAnalysis {
                    page_no: 1,
                    coverage,
                    thumbnail_png: thumbnail_png(&img, 256),
                    metrics,
                }],
                format: SpoolFormat::Image,
            }
        }
        SpoolFormat::Pdf => analyze_pdf_best(data, gs)?,
        SpoolFormat::PostScript => {
            let gs = gs.ok_or(AnalyzeError::Ghostscript(crate::gs::GsError::NotFound))?;
            analyze_pdf_bytes(data, gs)?
        }
        // GDI applications spool EMF: measured directly on Windows, normalized
        // elsewhere. XPS and PCL are not measurable with free tooling and go
        // through the legacy conversion only when Ghostscript is present.
        SpoolFormat::Emf => analyze_emf_best(data, gs)?,
        SpoolFormat::Xps | SpoolFormat::Pcl => {
            let gs = gs.ok_or(AnalyzeError::Ghostscript(crate::gs::GsError::NotFound))?;
            let pdf = convert_to_pdf(data, format, gs)?;
            analyze_pdf_bytes(&pdf, gs)?
        }
        other => return Err(AnalyzeError::UnsupportedFormat(other)),
    };
    // The reported format is the SOURCE payload, never the intermediate PDF:
    // the outbox reprint reconverts the cached spool bytes with it.
    analysis.format = format;
    Ok(analysis)
}

/// Write the payload to a temp file and run the PDF analysis over it.
fn analyze_pdf_bytes(data: &[u8], gs: &Ghostscript) -> Result<JobAnalysis, AnalyzeError> {
    let dir = unique_temp_dir()?;
    let input = dir.join("job.bin");
    std::fs::write(&input, data)?;
    let result = analyze_pdf_file(&input, gs);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Analyze a file on disk. Extension decides images; content decides the rest.
/// Ghostscript is only required for PDF/PS payloads.
pub fn analyze_file(path: &Path, gs: Option<&Ghostscript>) -> Result<JobAnalysis, AnalyzeError> {
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
    analyze_bytes(&data, gs)
}

/// Rejects oversized documents before the expensive thumbnail render.
fn check_page_cap(pages: usize) -> Result<(), AnalyzeError> {
    if pages > MAX_ANALYZE_PAGES {
        return Err(AnalyzeError::TooManyPages { pages });
    }
    Ok(())
}

/// Previews past the budget render as placeholders (the UI already does).
fn wants_thumbnail(page_no: u32) -> bool {
    (page_no as usize) <= MAX_THUMBNAIL_PAGES
}

/// One measured page: pixel metrics, the coverage derived from them, and a
/// preview when the page is inside the thumbnail budget.
#[cfg(any(windows, feature = "pdfium"))]
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

/// PDF measurement: pdfium when it is provisioned, otherwise the legacy
/// Ghostscript path. Only the pdfium path yields a page class and area
/// coverage, which is what the tier ladder bills on.
fn analyze_pdf_best(data: &[u8], gs: Option<&Ghostscript>) -> Result<JobAnalysis, AnalyzeError> {
    #[cfg(feature = "pdfium")]
    if crate::pdf::available() {
        return analyze_pdf_with_pdfium(data);
    }
    let gs = gs.ok_or(AnalyzeError::Ghostscript(GsError::NotFound))?;
    analyze_pdf_bytes(data, gs)
}

#[cfg(feature = "pdfium")]
fn analyze_pdf_with_pdfium(data: &[u8]) -> Result<JobAnalysis, AnalyzeError> {
    let count = crate::pdf::page_count(data)?;
    check_page_cap(count)?;
    let dpi = Thresholds::default().dpi;
    let mut pages = Vec::with_capacity(count);
    for index in 0..count {
        let img = crate::pdf::render_page(data, index, dpi)?;
        pages.push(measured_page((index + 1) as u32, img));
    }
    Ok(JobAnalysis {
        pages,
        format: SpoolFormat::Pdf,
    })
}

/// EMF measurement: GDI playback on Windows, legacy normalization elsewhere.
fn analyze_emf_best(data: &[u8], gs: Option<&Ghostscript>) -> Result<JobAnalysis, AnalyzeError> {
    #[cfg(windows)]
    {
        // Ghostscript is only a fallback off Windows; nothing to convert here.
        let _ = gs;
        let split = crate::emf::split_pages(data)?;
        check_page_cap(split.len())?;
        let dpi = Thresholds::default().dpi;
        let mut pages = Vec::with_capacity(split.len());
        for (index, page) in split.iter().enumerate() {
            let img = crate::emf::rasterize_page(&page.bytes, page.width_mm, page.height_mm, dpi)?;
            pages.push(measured_page((index + 1) as u32, img));
        }
        Ok(JobAnalysis {
            pages,
            format: SpoolFormat::Emf,
        })
    }
    #[cfg(not(windows))]
    {
        let gs = gs.ok_or(AnalyzeError::Ghostscript(GsError::NotFound))?;
        let pdf = convert_to_pdf(data, SpoolFormat::Emf, gs)?;
        analyze_pdf_bytes(&pdf, gs)
    }
}

fn analyze_pdf_file(input: &Path, gs: &Ghostscript) -> Result<JobAnalysis, AnalyzeError> {
    let fractions = gs.inkcov(input)?;
    check_page_cap(fractions.len())?;
    let dir = unique_temp_dir()?;
    let pattern = dir.join("thumb-%d.png");
    gs.render_png_pages(input, 72, &pattern)?;
    let mut pages = Vec::with_capacity(fractions.len());
    for (i, f) in fractions.iter().enumerate() {
        let page_no = (i + 1) as u32;
        let thumbnail_png = if wants_thumbnail(page_no) {
            let thumb_path = dir.join(format!("thumb-{page_no}.png"));
            std::fs::read(&thumb_path).unwrap_or_default()
        } else {
            Vec::new()
        };
        pages.push(PageAnalysis {
            page_no,
            coverage: Coverage::new(f[0] * 100.0, f[1] * 100.0, f[2] * 100.0, f[3] * 100.0),
            thumbnail_png,
            metrics: None,
        });
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(JobAnalysis {
        pages,
        format: SpoolFormat::Pdf,
    })
}

fn unique_temp_dir() -> Result<PathBuf, std::io::Error> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mpa-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gs_or_skip() -> Option<Ghostscript> {
        Ghostscript::discover().ok()
    }

    #[test]
    fn image_bytes_analyze_end_to_end() {
        // 2x1 PPM: one white pixel, one black pixel → K = 50%.
        let ppm: Vec<u8> = b"P6\n2 1\n255\n\xFF\xFF\xFF\x00\x00\x00".to_vec();
        let analysis = analyze_bytes(&ppm, None).unwrap();
        assert_eq!(analysis.pages.len(), 1);
        assert!((analysis.pages[0].coverage.k - 50.0).abs() < 1e-3);
        assert!(!analysis.pages[0].thumbnail_png.is_empty());
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
            analyze_bytes(b"whatever", None),
            Err(AnalyzeError::UnsupportedFormat(SpoolFormat::Unknown))
        ));
    }

    /// EMF and PCL spools must reach the PDF normalization step (GDI /
    /// GhostPCL) instead of coming back as "unsupported spool format" — that
    /// rejection is what left every PCL laser printer unanalyzable.
    #[test]
    fn emf_and_pcl_route_to_normalization() {
        let emf = vec![1u8, 0, 0, 0, 88, 0, 0, 0, 0, 0];
        let pcl = vec![0x1Bu8, b'E', 0x1B, b'&', b'l', b'0', b'A'];
        for payload in [emf, pcl] {
            assert!(
                matches!(
                    analyze_bytes(&payload, None),
                    Err(AnalyzeError::Ghostscript(_))
                ),
                "must reach the conversion step, not UnsupportedFormat"
            );
        }
    }

    /// Same routing with Ghostscript available: whatever goes wrong now must
    /// go wrong *inside* the conversion (e.g. "GhostPCL (gpcl6) not found"),
    /// which the operator can act on.
    #[test]
    fn pcl_never_reports_unsupported_format_with_ghostscript() {
        let Some(gs) = gs_or_skip() else { return };
        let pcl = vec![0x1Bu8, b'E', 0x1B, b'&', b'l', b'0', b'A'];
        if let Err(e) = analyze_bytes(&pcl, Some(&gs)) {
            assert!(
                !matches!(e, AnalyzeError::UnsupportedFormat(_)),
                "routed nowhere: {e:?}"
            );
        }
    }

    /// End-to-end PCL: render a real PCL payload with Ghostscript's `ljet4`
    /// device, then analyze it through GhostPCL. Skipped where either tool is
    /// missing (CI Linux), exercised on the Windows build machine.
    #[test]
    fn pcl_roundtrip_survives_the_conversion() {
        use crate::convert::find_pcl_interpreter;
        let Some(gs) = gs_or_skip() else { return };
        if find_pcl_interpreter().is_none() {
            return;
        }
        let dir = unique_temp_dir().unwrap();
        let ps = dir.join("in.ps");
        std::fs::write(
            &ps,
            b"%!PS\n0 0 0 setrgbcolor 100 100 200 200 rectfill showpage\n",
        )
        .unwrap();
        let pcl_path = dir.join("out.pcl");
        let rendered = std::process::Command::new(gs.program())
            .arg("-q")
            .arg("-dBATCH")
            .arg("-dNOPAUSE")
            .arg("-sDEVICE=ljet4")
            .arg(format!("-o{}", pcl_path.display()))
            .arg(&ps)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !rendered {
            return; // Ghostscript built without the PCL-ish devices
        }
        let pcl = std::fs::read(&pcl_path).unwrap();
        assert_eq!(detect_format(&pcl), SpoolFormat::Pcl, "fixture is not PCL");

        let analysis = analyze_bytes(&pcl, Some(&gs)).expect("PCL must analyze");
        assert_eq!(
            analysis.format,
            SpoolFormat::Pcl,
            "the outbox reprint reconverts the cached spool bytes, so the \
             source format must survive the normalization"
        );
        assert_eq!(analysis.pages.len(), 1);
        assert!(
            analysis.pages[0].coverage.k > 0.0,
            "the filled rectangle must show up as ink"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(feature = "pdfium"))]
    #[test]
    fn pdf_without_ghostscript_fails_clearly() {
        let pdf = minimal_pdf();
        assert!(matches!(
            analyze_bytes(&pdf, None),
            Err(AnalyzeError::Ghostscript(_))
        ));
    }

    #[test]
    fn pdf_needs_ghostscript() {
        let Some(gs) = gs_or_skip() else { return };
        let dir = unique_temp_dir().unwrap();
        let input = dir.join("blank.pdf");
        // Single blank page PDF (valid xref computed at runtime).
        let pdf = minimal_pdf();
        std::fs::write(&input, pdf).unwrap();
        let analysis = analyze_pdf_file(&input, &gs).unwrap();
        assert_eq!(analysis.pages.len(), 1);
        assert!(analysis.pages[0].coverage.color_coverage() < 1.0);
        let _ = std::fs::remove_dir_all(&dir);
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
