pub mod classify;
pub mod emf;
pub mod format;
#[cfg(feature = "pdfium")]
pub mod pdf;
pub mod pdfgen;
pub mod pipeline;
pub mod print;
pub mod raster;
pub mod spool_capture;
pub mod subset;

pub use classify::{
    analyze_image, analyze_rgba, analyze_rgba_with, PageClass, PageMetrics, Thresholds,
    ENGINE_VERSION,
};
pub use emf::{split_pages, EmfError, EmfPage};
pub use format::{detect_format, SpoolFormat};
pub use pdfgen::{images_to_pdf, PdfGenError, PdfRasterPage};
pub use pipeline::{
    analyze_bytes, analyze_file, render_pages, AnalyzeError, JobAnalysis, PageAnalysis,
};
pub use print::{print_images, PrintError};
pub use spool_capture::{capture_spool_file, CaptureError, SpoolFile};
pub use subset::{group_contiguous, SubsetError};
