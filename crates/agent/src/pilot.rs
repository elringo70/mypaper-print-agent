//! Shadow-pilot journal: what the agent measured, without charging anything.
//!
//! The pilot runs for a week in a real copy shop. The agent prints and measures
//! as usual, but nothing reaches MyPaper as a charge; instead every job leaves a
//! line here. The point is to compare, page by page, what this engine says
//! against what the shop is used to paying — and against what CyberPlanet
//! reports for the same sheets.
//!
//! Deliberately a plain append-only JSONL file: readable with `mpa pilot report`,
//! greppable, and impossible to corrupt into losing a day of measurements.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One measured page as the pilot records it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PilotPage {
    pub page_no: u32,
    pub class: Option<String>,
    pub area_pct: Option<f32>,
    pub c: f32,
    pub m: f32,
    pub y: f32,
    pub k: f32,
}

/// One captured job, with what it would have cost under each rule set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PilotEntry {
    /// ISO-8601 UTC, written when the job was captured.
    pub at: String,
    pub document: String,
    pub pages: Vec<PilotPage>,
    /// `None` when the server could not be asked (unpaired or offline). The
    /// measurement is still valid; only the price is unknown.
    #[serde(default)]
    pub bw_total: Option<f64>,
    #[serde(default)]
    pub color_total: Option<f64>,
}

/// Aggregated view of the journal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PilotReport {
    pub jobs: usize,
    pub pages: usize,
    pub blank_pages: usize,
    pub color_pages: usize,
    pub mono_pages: usize,
    pub grey_pages: usize,
    pub bw_total: f64,
    pub color_total: f64,
    /// Jobs captured while the price could not be fetched.
    pub unpriced_jobs: usize,
}

pub fn journal_path(dir: &Path) -> PathBuf {
    dir.join("pilot.jsonl")
}

/// Append one entry. Best effort: a pilot that cannot write must not stop the
/// printer the customer is waiting for.
pub fn append(dir: &Path, entry: &PilotEntry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path(dir))?;
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Read the journal, skipping lines that cannot be parsed: a truncated last line
/// from a power cut must not throw away the week.
pub fn read(dir: &Path) -> Vec<PilotEntry> {
    let Ok(text) = std::fs::read_to_string(journal_path(dir)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<PilotEntry>(line).ok())
        .collect()
}

pub fn summarize(entries: &[PilotEntry]) -> PilotReport {
    let mut report = PilotReport {
        jobs: entries.len(),
        ..Default::default()
    };
    for entry in entries {
        report.pages += entry.pages.len();
        for page in &entry.pages {
            match page.class.as_deref() {
                Some("blanco") => report.blank_pages += 1,
                Some("color") => report.color_pages += 1,
                Some("gris") => report.grey_pages += 1,
                Some("mono") => report.mono_pages += 1,
                _ => {}
            }
        }
        match (entry.bw_total, entry.color_total) {
            (Some(bw), Some(color)) => {
                report.bw_total += bw;
                report.color_total += color;
            }
            _ => report.unpriced_jobs += 1,
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mpa-pilot-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn page(no: u32, class: &str, area: f32) -> PilotPage {
        PilotPage {
            page_no: no,
            class: Some(class.into()),
            area_pct: Some(area),
            c: 0.0,
            m: 0.0,
            y: 0.0,
            k: area,
        }
    }

    fn entry(document: &str, pages: Vec<PilotPage>, price: Option<(f64, f64)>) -> PilotEntry {
        PilotEntry {
            at: "2026-10-26T10:00:00Z".into(),
            document: document.into(),
            pages,
            bw_total: price.map(|p| p.0),
            color_total: price.map(|p| p.1),
        }
    }

    #[test]
    fn a_journal_round_trips() {
        let dir = workdir("roundtrip");
        let original = entry(
            "cotizacion.pdf",
            vec![page(1, "mono", 8.0), page(2, "color", 20.0)],
            Some((9.0, 24.0)),
        );
        append(&dir, &original).expect("append");
        let read_back = read(&dir);
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].document, "cotizacion.pdf");
        assert_eq!(read_back[0].pages.len(), 2);
        assert_eq!(read_back[0].color_total, Some(24.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_line_does_not_lose_the_journal() {
        let dir = workdir("broken");
        append(
            &dir,
            &entry("uno.pdf", vec![page(1, "mono", 5.0)], Some((3.0, 3.0))),
        )
        .unwrap();
        // Simulate a power cut mid-write.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal_path(&dir))
            .unwrap();
        writeln!(file, "{{\"at\": \"2026-10-26T11:00:00Z\", \"docu").unwrap();
        append(
            &dir,
            &entry("dos.pdf", vec![page(1, "color", 9.0)], Some((0.0, 13.0))),
        )
        .unwrap();

        let entries = read(&dir);
        assert_eq!(entries.len(), 2, "the good lines survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_summary_counts_pages_by_class_and_totals_both_rule_sets() {
        let entries = vec![
            entry(
                "a.pdf",
                vec![page(1, "mono", 8.0), page(2, "blanco", 0.0)],
                Some((9.0, 9.0)),
            ),
            entry(
                "b.pdf",
                vec![
                    page(1, "color", 30.0),
                    page(2, "gris", 12.0),
                    page(3, "mono", 4.0),
                ],
                Some((12.0, 31.5)),
            ),
            // Captured while the server was unreachable: measured, not priced.
            entry("c.pdf", vec![page(1, "mono", 6.0)], None),
        ];
        let report = summarize(&entries);
        assert_eq!(report.jobs, 3);
        assert_eq!(report.pages, 6);
        assert_eq!(report.blank_pages, 1);
        assert_eq!(report.color_pages, 1);
        assert_eq!(report.grey_pages, 1);
        assert_eq!(report.mono_pages, 3);
        assert_eq!(report.unpriced_jobs, 1);
        assert!((report.bw_total - 21.0).abs() < 0.001);
        assert!((report.color_total - 40.5).abs() < 0.001);
    }

    #[test]
    fn an_empty_journal_is_an_empty_report_not_an_error() {
        let dir = workdir("empty");
        let entries = read(&dir);
        assert!(entries.is_empty());
        let report = summarize(&entries);
        assert_eq!(report.jobs, 0);
        assert_eq!(report.pages, 0);
    }
}
