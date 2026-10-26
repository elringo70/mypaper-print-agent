//! End-to-end PDF measurement through pdfium against the study's ground truth.
//!
//! This is the test that proves the PDF path no longer needs Ghostscript: the
//! same 30 documents the study rendered with PdfCraft are rendered here with
//! pdfium at the same resolution and measured with the same classifier.
//!
//! Two different rasterizers will never agree pixel for pixel (anti-aliasing,
//! font hinting), so per-channel numbers are held to a documented tolerance
//! while the page class — what actually decides the price — must match exactly.
//!
//! Skipped, not failed, when libpdfium is not provisioned: set `MPA_PDFIUM` to
//! the shared library path to run it.

#![cfg(feature = "pdfium")]

use mpa_analyzer::{analyze_rgba, pdf, PageClass};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Tolerance for the covered-area fraction across two different rasterizers.
///
/// The measured maximum on this corpus is 5.0 pp (CP-023, a 20% halftone
/// screen: the two renderers sample the screen at different phases), and 2.4 pp
/// on dense 8 pt text due to font hinting. Everything else stays under 1 pp.
/// This is rasterizer variance, not measurement variance: the pure-parity test
/// shares pixels and holds 0.05 pp.
const AREA_TOL: f64 = 0.06;

/// Per-channel tolerance. Measured maximum is 0.52 pp (CP-003 dense text), and
/// the channels are what the tier ladder bills, so this one stays tight.
const CHANNEL_TOL: f64 = 0.01;

#[derive(Debug, Deserialize)]
struct Truth {
    pages: Vec<TruthPage>,
}

#[derive(Debug, Deserialize)]
struct TruthPage {
    id: String,
    page: u32,
    file: String,
    coverage_nonwhite: f64,
    coverage_c: f64,
    coverage_m: f64,
    coverage_y: f64,
    coverage_k: f64,
    clase_propuesta: String,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rea")
}

fn pdf_path(id: &str) -> Option<PathBuf> {
    let dir = fixtures().join("pdfs");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(id))
        })
}

/// The corpus' `page` field is unreliable: every page of a multi-page case is
/// recorded as page 1. The page number survives only in the PNG file name
/// (`..._p3_150dpi.png`), which is what the generator actually used.
fn page_of(p: &TruthPage) -> usize {
    let re_ish = p.file.split("_p").nth(1);
    match re_ish.and_then(|rest| rest.split('_').next()) {
        Some(n) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => {
            n.parse().unwrap_or(p.page as usize)
        }
        _ => p.page as usize,
    }
}

fn class_of(name: &str) -> PageClass {
    match name {
        "blanco" => PageClass::Blanco,
        "color" => PageClass::Color,
        "gris" => PageClass::Gris,
        "mono" => PageClass::Mono,
        other => panic!("unknown reference class: {other}"),
    }
}

#[test]
fn pdfium_reproduces_the_reference_classes() {
    if !pdf::available() {
        eprintln!("pdfium not available (set MPA_PDFIUM); skipping");
        return;
    }

    let raw = std::fs::read_to_string(fixtures().join("render_truth.json")).unwrap();
    let truth: Truth = serde_json::from_str(&raw).unwrap();

    let mut class_failures = Vec::new();
    let mut drift_failures = Vec::new();
    let mut pages = 0;

    for p in &truth.pages {
        let Some(path) = pdf_path(&p.id) else {
            class_failures.push(format!("{}: fixture PDF missing", p.id));
            continue;
        };
        let bytes = std::fs::read(&path).unwrap();

        let page_no = page_of(p);
        let count = pdf::page_count(&bytes).expect("page count");
        if page_no > count {
            class_failures.push(format!("{}: page {page_no} of {count}", p.id));
            continue;
        }

        let img = pdf::render_page(&bytes, page_no - 1, 150).expect("render");
        let got = analyze_rgba(img.as_raw(), img.width(), img.height()).expect("metrics");
        pages += 1;

        let expected = class_of(&p.clase_propuesta);
        if got.class != expected {
            class_failures.push(format!(
                "{} p{} ({}): ours={:?} truth={expected:?} | area={:.4} cmy=({:.4},{:.4},{:.4})",
                p.id,
                page_no,
                p.file,
                got.class,
                got.coverage_nonwhite,
                got.coverage_c,
                got.coverage_m,
                got.coverage_y
            ));
        }

        for (name, ours, theirs, tol) in [
            (
                "nonwhite",
                got.coverage_nonwhite,
                p.coverage_nonwhite,
                AREA_TOL,
            ),
            ("c", got.coverage_c, p.coverage_c, CHANNEL_TOL),
            ("m", got.coverage_m, p.coverage_m, CHANNEL_TOL),
            ("y", got.coverage_y, p.coverage_y, CHANNEL_TOL),
            ("k", got.coverage_k, p.coverage_k, CHANNEL_TOL),
        ] {
            if (ours - theirs).abs() > tol {
                drift_failures.push(format!(
                    "{} p{} {name}: pdfium={ours:.4} pdfcraft={theirs:.4} (Δ {:.4})",
                    p.id,
                    page_no,
                    (ours - theirs).abs()
                ));
            }
        }
    }

    assert_eq!(
        pages, 35,
        "all 35 reference pages must render through pdfium"
    );
    assert!(
        class_failures.is_empty(),
        "class divergence:\n{}",
        class_failures.join("\n")
    );
    assert!(
        drift_failures.is_empty(),
        "coverage drift beyond tolerance:\n{}",
        drift_failures.join("\n")
    );
}
