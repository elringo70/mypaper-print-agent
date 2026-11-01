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
    /// Open tickets to attach a draft to. Empty when offline or unpaired.
    pub tickets: Vec<TicketDto>,
    /// Print presets from `config.toml`, by name.
    pub presets: Vec<String>,
}

/// Price of one rule set for the selected pages.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeQuoteDto {
    pub total: f64,
    /// Per-page line total, so the list shows a cost next to each page.
    pub pages: Vec<PageQuoteDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageQuoteDto {
    pub page_no: u32,
    pub unit_price: f64,
}

/// Both rule sets at once: the operator flips between them and the total must
/// not flicker through a round trip each time.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteDto {
    pub bw: ModeQuoteDto,
    pub color: ModeQuoteDto,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketDto {
    pub id: String,
    pub number: i32,
    pub cashier: String,
    /// Customer of the sale, when it has one.
    pub customer: Option<String>,
    pub total: f64,
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
    /// "unassigned" | "newDraft" | "ticket"
    pub target: String,
    pub target_ticket_id: Option<String>,
    pub preset: Option<String>,
    /// "Bw" | "Color"
    pub print_mode: String,
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
    // Page previews are fetched separately (`page_thumbnail`): shipping every
    // thumbnail here turned each poll into megabytes of base64.
    pub included: bool,
    /// Sheet carries no ink: not charged and not printed.
    pub blank: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckDto {
    pub name: String,
    /// "ok" | "warn" | "fail"
    pub health: String,
    pub detail: String,
}
