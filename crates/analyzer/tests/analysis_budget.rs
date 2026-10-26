//! Time budget for the per-pixel analysis.
//!
//! The engine reads every pixel of every page; a regression there is invisible
//! in a correctness test and obvious to the operator whose print dialog hangs.
//! This walks A4 pages at the real 150 dpi through the same entry point the
//! agent uses and fails if the cost per page drifts by an order of magnitude.
//!
//! The budget is deliberately loose (CI is slower and shared): it catches
//! accidental O(n²) work, not a 10 % slowdown.

use std::time::Instant;

/// A4 at 150 dpi.
const WIDTH: u32 = 1240;
const HEIGHT: u32 = 1754;
const PAGES: usize = 4;
/// Generous: ~14 ms/page on a laptop, so this tolerates a 10× slower machine.
const BUDGET_MS: u128 = 2_000;

/// A page that looks like a document: white paper, black text, some colour.
fn page(seed: u32) -> Vec<u8> {
    let mut rgba = vec![255u8; (WIDTH * HEIGHT * 4) as usize];
    let mut state = seed | 1;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let roll = (state >> 24) as u8;
        if roll < 30 {
            // Text: near-black ink.
            pixel[0] = 20;
            pixel[1] = 20;
            pixel[2] = 20;
        } else if roll < 34 {
            // A logo in colour.
            pixel[0] = 200;
            pixel[3] = 255;
        }
    }
    rgba
}

#[test]
fn analysing_a_page_stays_within_the_budget() {
    // Warm the path once so the measurement is not the first-touch cost.
    let _ = mpa_analyzer::analyze_rgba(&page(0), WIDTH, HEIGHT).expect("first page");

    let started = Instant::now();
    for seed in 1..=PAGES as u32 {
        let analysis = mpa_analyzer::analyze_rgba(&page(seed), WIDTH, HEIGHT).expect("page");
        assert_eq!(analysis.width, WIDTH);
    }
    let elapsed = started.elapsed().as_millis();

    assert!(
        elapsed < BUDGET_MS,
        "{PAGES} pages took {elapsed} ms, budget {BUDGET_MS} ms"
    );
    eprintln!(
        "{PAGES} pages in {elapsed} ms ({:.1} ms/page)",
        elapsed as f64 / PAGES as f64
    );
}
