//! What the UI receives. Plain data, no agent internals: the window is a view
//! of the core, not a second copy of it.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    /// Bumped by the core on every change; the UI polls this to know when to
    /// re-read (cheap, no events to leak).
    pub version: u64,
    pub mode: String,
    /// Set when this device may not bill (no entitlement, expired, clock
    /// moved backwards). The UI shows it as a banner, not as an error dialog.
    pub blocked: Option<String>,
    pub jobs: Vec<JobDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDto {
    pub printer: String,
    pub job_id: u32,
    pub document: String,
    pub state: String,
    pub pages: Vec<PageDto>,
    pub analysis_error: Option<String>,
    pub warnings: Vec<String>,
    pub format: Option<String>,
    pub price: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDto {
    pub page_no: u32,
    pub c: f32,
    pub m: f32,
    pub y: f32,
    pub k: f32,
    /// Share of the sheet carrying ink, when it was measured.
    pub area_pct: Option<f32>,
    pub class: Option<String>,
    pub tier_id: Option<String>,
    pub included: bool,
    /// `data:image/png;base64,…`, empty when the page has no preview.
    pub thumbnail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckDto {
    pub name: String,
    /// "ok" | "warn" | "fail"
    pub health: String,
    pub detail: String,
}