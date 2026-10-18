//! End-to-end demo of the Phase-3 reprint pipeline foundations (Linux-safe):
/// synthetic EMF spool → page split → raster → JPEG → assembled PDF with a
/// verified xref table. GDI playback itself only runs on Windows; here the
/// raster step uses synthetic pixels through the same `encode + assemble`
/// path `emf_convert` uses after `rasterize_page`.
use image::{Rgba, RgbaImage};
use mpa_analyzer::raster::encode_jpeg_rgba;
use mpa_analyzer::{images_to_pdf, split_pages, PdfRasterPage};

/// Minimal one-page EMF: valid ENHMETAHEADER + filler body.
fn one_page_emf(width_01mm: i32, height_01mm: i32) -> Vec<u8> {
    let body = vec![0xAAu8; 64];
    let total = (88 + body.len()) as u32;
    let mut hdr = vec![0u8; 88];
    hdr[0..4].copy_from_slice(&1u32.to_le_bytes());
    hdr[4..8].copy_from_slice(&88u32.to_le_bytes());
    hdr[32..36].copy_from_slice(&width_01mm.to_le_bytes());
    hdr[36..40].copy_from_slice(&height_01mm.to_le_bytes());
    hdr[40..44].copy_from_slice(&0x464D4520u32.to_le_bytes());
    hdr[48..52].copy_from_slice(&total.to_le_bytes());
    hdr[52..56].copy_from_slice(&2u32.to_le_bytes());
    hdr.extend_from_slice(&body);
    hdr
}

fn main() {
    // 1. Two-page spool: carta + A4.
    let mut spool = one_page_emf(21590, 27940);
    spool.extend_from_slice(&one_page_emf(21000, 29700));
    let pages = split_pages(&spool).expect("synthetic spool splits");
    println!("split: {} pages", pages.len());

    // 2. Same encode + assemble path as the reprint pipeline (150 DPI).
    let dpi = 150u32;
    let mut raster = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let (w, h) = (
            (page.width_mm / 25.4 * dpi as f32).round() as u32,
            (page.height_mm / 25.4 * dpi as f32).round() as u32,
        );
        let color = if i % 2 == 0 {
            Rgba([200, 30, 30, 255])
        } else {
            Rgba([30, 60, 200, 255])
        };
        let img = RgbaImage::from_pixel(w, h, color);
        let jpeg = encode_jpeg_rgba(&img.into_raw(), w, h, 85).expect("jpeg encodes");
        raster.push(PdfRasterPage {
            width_mm: page.width_mm,
            height_mm: page.height_mm,
            width_px: w,
            height_px: h,
            jpeg,
        });
        println!("page {}: {w}x{h}px @ {dpi}dpi", i + 1);
    }
    let pdf = images_to_pdf(&raster).expect("pdf assembles");
    let out = std::env::temp_dir().join("emf-demo.pdf");
    std::fs::write(&out, &pdf).expect("pdf writes");
    println!("wrote {} ({} bytes)", out.display(), pdf.len());
}
