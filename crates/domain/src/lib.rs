pub mod coverage;
mod options;
pub mod policy;
mod pricing;
pub mod release;
pub mod state;

pub use coverage::{
    classify_coverage, has_color_ink, metric_coverage, metric_coverage_with_area, Coverage,
    MetricsByMode, PrintMetric, PrintMode, Tier,
};
pub use options::{PrintOptions, PrintPreset};
pub use policy::{PolicyMode, PrinterPolicy, PrinterRoles};
pub use pricing::{
    dearest_tier, is_grey_ink, priced_coverage, PrintPricing, VolumeDiscount, GREY_TOLERANCE,
};
pub use release::{ManifestError, ReleaseManifest, UpdateStatus};
pub use state::{transition, JobEvent, JobState, TransitionError};
