pub mod classify;
pub mod convert;
pub mod emf;
pub mod format;
pub mod gs;
#[cfg(feature = "pdfium")]
pub mod pdf;
pub mod pdfgen;
pub mod pipeline;
pub mod raster;
pub mod spool_capture;
pub mod subset;

pub use classify::{
    analyze_image, analyze_rgba, analyze_rgba_with, PageClass, PageMetrics, Thresholds,
    ENGINE_VERSION,
};
pub use convert::{convert_to_pdf, find_pcl_interpreter, ConvertError};
pub use emf::{split_pages, EmfError, EmfPage};
pub use format::{detect_format, SpoolFormat};
pub use gs::{Ghostscript, GsError};
pub use pdfgen::{images_to_pdf, PdfGenError, PdfRasterPage};
pub use pipeline::{analyze_bytes, analyze_file, AnalyzeError, JobAnalysis, PageAnalysis};
pub use spool_capture::{capture_spool_file, CaptureError, SpoolFile};
pub use subset::{extract_pages_pdf, group_contiguous, print_pdf_to_winprinter, SubsetError};
