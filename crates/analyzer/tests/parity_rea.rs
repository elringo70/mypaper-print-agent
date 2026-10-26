//! Parity with the CyberPlanet study's ground truth.
//!
//! The study rendered 30 deterministic PDFs to 150 dpi PNGs with an independent
//! rasterizer (PdfCraft) and measured them pixel by pixel. Those 35 pages are
//! the external reference for this analyzer: if MyPaper's numbers drift from
//! them, the drift is ours, not the fixture's.
//!
//! This test is the acceptance criterion of the measurement phase. It does not
//! render anything — it feeds the same pixels to both implementations, so it
//! isolates the measurement from the rasterizer.

use mpa_analyzer::{analyze_rgba, PageClass};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 0.05 percentage points, as a fraction: the tolerance the study committed to.
const TOL: f64 = 0.0005;

#[derive(Debug, Deserialize)]
struct Truth {
    pages: Vec<TruthPage>,
}

#[derive(Debug, Deserialize)]
struct TruthPage {
    id: String,
    page: u32,
    file: String,
    page_area_cm2: f64,
    coverage_nonwhite: f64,
    coverage_c: f64,
    coverage_m: f64,
    coverage_y: f64,
    coverage_k: f64,
    coverage_lum: f64,
    dark_fraction: f64,
    color_pixel_frac: f64,
    gris_pixel_frac: f64,
    pure_black_frac: f64,
    mean_chroma: f64,
    clase_propuesta: String,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rea")
}

fn truth() -> Truth {
    let raw = std::fs::read_to_string(fixtures().join("render_truth.json"))
        .expect("fixture render_truth.json is missing");
    serde_json::from_str(&raw).expect("render_truth.json does not parse")
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
fn every_reference_page_matches_the_ground_truth() {
    let truth = truth();
    assert_eq!(truth.pages.len(), 35, "the reference corpus has 35 pages");

    let mut failures = Vec::new();

    for p in &truth.pages {
        let path = fixtures().join("render").join(&p.file);
        let img = image::open(&path)
            .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()))
            .to_rgba8();
        let got = analyze_rgba(img.as_raw(), img.width(), img.height())
            .unwrap_or_else(|| panic!("{} produced no metrics", p.file));

        let mut diffs = Vec::new();
        let mut cmp = |name: &str, ours: f64, theirs: f64, tol: f64| {
            if (ours - theirs).abs() > tol {
                diffs.push(format!(
                    "{name}: ours={ours:.4} truth={theirs:.4} (Δ {:.4})",
                    (ours - theirs).abs()
                ));
            }
        };
        cmp(
            "coverage_nonwhite",
            got.coverage_nonwhite,
            p.coverage_nonwhite,
            TOL,
        );
        cmp("coverage_c", got.coverage_c, p.coverage_c, TOL);
        cmp("coverage_m", got.coverage_m, p.coverage_m, TOL);
        cmp("coverage_y", got.coverage_y, p.coverage_y, TOL);
        cmp("coverage_k", got.coverage_k, p.coverage_k, TOL);
        cmp("coverage_lum", got.coverage_lum, p.coverage_lum, TOL);
        cmp("dark_fraction", got.dark_fraction, p.dark_fraction, TOL);
        cmp(
            "color_pixel_frac",
            got.color_pixel_frac,
            p.color_pixel_frac,
            TOL,
        );
        cmp(
            "gris_pixel_frac",
            got.gris_pixel_frac,
            p.gris_pixel_frac,
            TOL,
        );
        cmp(
            "pure_black_frac",
            got.pure_black_frac,
            p.pure_black_frac,
            TOL,
        );
        cmp("mean_chroma", got.mean_chroma, p.mean_chroma, TOL);
        cmp("page_area_cm2", got.page_area_cm2, p.page_area_cm2, 0.05);

        let expected = class_of(&p.clase_propuesta);
        if got.class != expected {
            diffs.push(format!("class: ours={:?} truth={expected:?}", got.class));
        }

        if !diffs.is_empty() {
            failures.push(format!(
                "{} p{} ({}):\n    {}",
                p.id,
                p.page,
                p.file,
                diffs.join("\n    ")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} pages diverged from the reference:\n\n{}",
        failures.len(),
        truth.pages.len(),
        failures.join("\n\n")
    );
}

/// Guard the two thresholds that decide money on this corpus. If a future
/// change makes a blank page billable or a color page free, this fails even
/// when the per-channel numbers still look plausible.
#[test]
fn blank_pages_are_free_and_color_pages_are_not() {
    let truth = truth();
    let mut blanks = 0;
    let mut colors = 0;

    for p in &truth.pages {
        let path = fixtures().join("render").join(&p.file);
        let img = image::open(&path).unwrap().to_rgba8();
        let got = analyze_rgba(img.as_raw(), img.width(), img.height()).unwrap();

        match got.class {
            PageClass::Blanco => {
                blanks += 1;
                assert!(!got.class.is_billable(), "{} must be free", p.id);
                assert_eq!(got.area_pct(), 0.0);
            }
            PageClass::Color | PageClass::Gris => {
                colors += 1;
                assert!(
                    got.class.uses_color_ladder(),
                    "{} must bill on the color ladder",
                    p.id
                );
                assert!(got.area_pct() > 0.0);
            }
            PageClass::Mono => {
                assert!(!got.class.uses_color_ladder(), "{} must bill ByN", p.id);
            }
        }
    }

    assert!(
        blanks > 0,
        "the corpus must contain at least one blank page"
    );
    assert!(
        colors > 0,
        "the corpus must contain at least one color page"
    );
}
