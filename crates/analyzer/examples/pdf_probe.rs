//! Manual probe: render one page of a PDF and print what came out.
//!
//! `cargo run --example pdf_probe --features pdfium -- <file.pdf> [page] [dpi]`
//! with `MPA_PDFIUM` pointing at the shared library.

use mpa_analyzer::pdf;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: pdf_probe <file.pdf> [page] [dpi]");
    let page: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let dpi: u32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(150);

    println!("pdfium available: {}", pdf::available());
    let bytes = std::fs::read(&path).expect("read pdf");
    println!("bytes: {}", bytes.len());
    println!("pages: {:?}", pdf::page_count(&bytes));

    let img = pdf::render_page(&bytes, page - 1, dpi).expect("render");
    println!("bitmap: {}x{}", img.width(), img.height());

    let raw = img.as_raw();
    let mut nonwhite = 0usize;
    let mut sum = [0u64; 3];
    for px in raw.as_chunks::<4>().0 {
        if px[0] < 248 || px[1] < 248 || px[2] < 248 {
            nonwhite += 1;
        }
        sum[0] += px[0] as u64;
        sum[1] += px[1] as u64;
        sum[2] += px[2] as u64;
    }
    let n = (img.width() as u64 * img.height() as u64).max(1);
    println!(
        "first px: {:?} | nonwhite {nonwhite}/{n} | mean rgb ({},{},{})",
        &raw[..4.min(raw.len())],
        sum[0] / n,
        sum[1] / n,
        sum[2] / n
    );
}
