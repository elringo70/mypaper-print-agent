//! Turning a captured IPP job into the draft MyPaper already knows how to price.
//!
//! The spool watcher and the IPP endpoint must produce the same shape, or the
//! same document would be billed differently depending on how it arrived. This
//! is the single place that maps a measurement onto an [`OutboxEntry`].

use crate::core::{OutboxEntry, OutboxPage, PageArea, TargetSelection};
use mpa_analyzer::JobAnalysis;
use mpa_domain::PrintMode;

/// What the client told us about the job, minus the document itself.
#[derive(Debug, Clone)]
pub struct CapturedJob<'a> {
    /// Queue the job arrived on, as it appears in the spooler.
    pub printer: &'a str,
    /// IPP `print-color-mode`: `monochrome` or `color`.
    pub color_mode: Option<&'a str>,
    /// Stable per document, so resending the same print job never bills twice.
    pub idempotency_key: String,
    /// Printer the pages must come out of, when it differs from the capture one.
    pub deliver_printer: Option<String>,
}

/// Draft for MyPaper. Target stays unassigned: the operator picks the ticket in
/// the POS, which is where the price becomes a charge.
pub fn draft_for(captured: &CapturedJob<'_>, analysis: &JobAnalysis) -> OutboxEntry {
    let pages = analysis
        .pages
        .iter()
        .map(|page| OutboxPage {
            page_no: page.page_no,
            coverage: page.coverage,
            tier_id: None,
            included: true,
            // `AREA` ladders bill on this, so it has to survive to the server.
            area: page.metrics.map(|m| PageArea {
                area_pct: m.area_pct() as f32,
                class: Some(m.class),
            }),
        })
        .collect();

    OutboxEntry {
        idempotency_key: captured.idempotency_key.clone(),
        printer: captured.printer.to_string(),
        // The IPP endpoint has no spooler job id of its own; the idempotency
        // key is what makes a retry safe.
        job_id: 0,
        target: TargetSelection::Unassigned,
        pages,
        format: Some(analysis.format),
        spool_path: None,
        mode: mode_from(captured.color_mode),
        deliver_printer: captured.deliver_printer.clone(),
        options: None,
        applied_settings: None,
        reprint_pending: false,
        reprint_attempts: 0,
        document_sha256: None,
        media: None,
        synced: false,
        remote_id: None,
        materialized: false,
    }
}

/// Idempotency key for a captured document: the content hash itself.
///
/// Printing the same file twice produces the same key, and MyPaper is idempotent
/// on it, so a resend after a network failure cannot bill twice — while a second
/// genuine print of the same PDF is the same document and the operator decides
/// in the POS whether to charge it again.
pub fn idempotency_for(document: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(document);
    format!("ipp-{}", hex::encode(hasher.finalize()))
}

/// Default to the colour rule set when the client says nothing: it is the one
/// the operator expects from a colour queue, and the POS shows the draft before
/// anything is charged.
fn mode_from(color_mode: Option<&str>) -> PrintMode {
    match color_mode {
        Some(mode) if mode.eq_ignore_ascii_case("monochrome") => PrintMode::Bw,
        _ => PrintMode::Color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mpa_analyzer::PageAnalysis;

    fn analysis_with(pixels: &[u8], w: u32, h: u32) -> JobAnalysis {
        let metrics = mpa_analyzer::analyze_rgba(pixels, w, h).expect("metrics");
        JobAnalysis {
            format: mpa_analyzer::SpoolFormat::Pdf,
            pages: vec![PageAnalysis {
                page_no: 1,
                coverage: metrics.coverage(),
                thumbnail_png: Vec::new(),
                metrics: Some(metrics),
            }],
        }
    }

    fn captured<'a>(color_mode: Option<&'a str>) -> CapturedJob<'a> {
        CapturedJob {
            printer: "MyPaper",
            color_mode,
            idempotency_key: "abc123".into(),
            deliver_printer: Some("KONICA".into()),
        }
    }

    #[test]
    fn the_idempotency_key_is_the_document_itself() {
        let a = idempotency_for(b"%PDF-1.7 one");
        assert_eq!(a, idempotency_for(b"%PDF-1.7 one"), "same bytes, same key");
        assert_ne!(a, idempotency_for(b"%PDF-1.7 two"));
        assert!(a.starts_with("ipp-"));
    }

    #[test]
    fn monochrome_from_the_client_picks_the_black_rule_set() {
        let entry = draft_for(
            &captured(Some("monochrome")),
            &analysis_with(&[0, 0, 0, 255], 1, 1),
        );
        assert_eq!(entry.mode, PrintMode::Bw);
    }

    #[test]
    fn colour_is_the_default_when_the_client_says_nothing() {
        assert_eq!(
            draft_for(
                &captured(Some("color")),
                &analysis_with(&[0, 0, 0, 255], 1, 1)
            )
            .mode,
            PrintMode::Color
        );
        assert_eq!(
            draft_for(&captured(None), &analysis_with(&[0, 0, 0, 255], 1, 1)).mode,
            PrintMode::Color
        );
    }

    #[test]
    fn the_area_measurement_survives_to_the_draft() {
        // Half the sheet inked: AREA ladders bill on exactly this number.
        let mut pixels = vec![255u8; 2 * 2 * 4];
        for px in pixels.as_chunks_mut::<4>().0.iter_mut().take(2) {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        }
        let entry = draft_for(&captured(Some("color")), &analysis_with(&pixels, 2, 2));
        let area = entry.pages[0].area.expect("area carried");
        assert!((area.area_pct - 50.0).abs() < 0.01, "got {}", area.area_pct);
    }

    #[test]
    fn the_draft_points_at_the_queue_and_the_delivery_printer() {
        let entry = draft_for(
            &captured(Some("color")),
            &analysis_with(&[0, 0, 0, 255], 1, 1),
        );
        assert_eq!(entry.printer, "MyPaper");
        assert_eq!(entry.deliver_printer.as_deref(), Some("KONICA"));
        assert_eq!(entry.idempotency_key, "abc123");
        assert!(matches!(entry.target, TargetSelection::Unassigned));
        // No spool file: the document lives on disk under the inbox directory.
        assert!(entry.spool_path.is_none());
    }
}
