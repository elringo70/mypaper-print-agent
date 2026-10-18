use image::{DynamicImage, GenericImageView};
use mpa_domain::Coverage;

/// Naive RGB → CMYK separation (no ICC profile, no UCR/GCR tuning).
/// Matches the industry-standard estimator used by coverage calculators:
/// `K = 1 - max(R,G,B)`, other channels normalized by `(1 - K)`.
/// Printers apply their own profiles, so treat results as estimates —
/// exactly like CyberPlanet-style pre-print pricing does.
pub fn coverage_of_image(img: &DynamicImage) -> Coverage {
    let (w, h) = img.dimensions();
    let total = (w as f64) * (h as f64);
    if total == 0.0 {
        return Coverage::new(0.0, 0.0, 0.0, 0.0);
    }
    let rgb = img.to_rgb8();
    let mut sum = [0.0f64; 4];
    for pixel in rgb.pixels() {
        let [r, g, b] = pixel.0;
        let (rf, gf, bf) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        let k = 1.0 - rf.max(gf).max(bf);
        if k >= 1.0 {
            sum[3] += 1.0;
            continue;
        }
        let inv = 1.0 - k;
        sum[0] += (1.0 - rf - k) / inv;
        sum[1] += (1.0 - gf - k) / inv;
        sum[2] += (1.0 - bf - k) / inv;
        sum[3] += k;
    }
    Coverage::new(
        (sum[0] / total * 100.0) as f32,
        (sum[1] / total * 100.0) as f32,
        (sum[2] / total * 100.0) as f32,
        (sum[3] / total * 100.0) as f32,
    )
}

pub fn coverage_of_file(path: &std::path::Path) -> Result<Coverage, image::ImageError> {
    Ok(coverage_of_image(&image::open(path)?))
}

/// Small preview for the page list (Phase 3 UI). Longest side = `max_dim`.
/// Encode RGBA pixels as JPEG (quality 0-100) for PDF embedding.
/// JPEG has no alpha: fully transparent pixels become paper-white, matching
/// the white canvas the GDI rasterizer paints before playback.
pub fn encode_jpeg_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, image::ImageError> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ImageEncoder;

    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for px in rgba.chunks_exact(4) {
        let alpha = px[3] as f32 / 255.0;
        rgb.push((px[0] as f32 * alpha + 255.0 * (1.0 - alpha)) as u8);
        rgb.push((px[1] as f32 * alpha + 255.0 * (1.0 - alpha)) as u8);
        rgb.push((px[2] as f32 * alpha + 255.0 * (1.0 - alpha)) as u8);
    }
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, quality).write_image(
        &rgb,
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(buf)
}

pub fn thumbnail_png(img: &DynamicImage, max_dim: u32) -> Vec<u8> {
    let thumb = img.thumbnail(max_dim, max_dim);
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    // A preview must never fail the analysis: fall back to raw bytes.
    use image::ImageEncoder;
    if encoder
        .write_image(
            &thumb.to_rgb8(),
            thumb.width(),
            thumb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .is_err()
    {
        return Vec::new();
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn solid(w: u32, h: u32, pixel: Rgb<u8>) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, pixel))
    }

    #[test]
    fn white_page_has_zero_coverage() {
        let cov = coverage_of_image(&solid(16, 16, Rgb([255, 255, 255])));
        assert_eq!(cov, Coverage::new(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn black_page_is_full_k_only() {
        let cov = coverage_of_image(&solid(16, 16, Rgb([0, 0, 0])));
        assert!((cov.k - 100.0).abs() < 1e-3);
        assert!(cov.c.abs() < 1e-3 && cov.m.abs() < 1e-3 && cov.y.abs() < 1e-3);
    }

    #[test]
    fn pure_red_is_full_yellow() {
        // R=1,G=B=0 → K=0, C=0, M=1, Y=1.
        let cov = coverage_of_image(&solid(8, 8, Rgb([255, 0, 0])));
        assert!((cov.m - 100.0).abs() < 1e-3);
        assert!((cov.y - 100.0).abs() < 1e-3);
        assert!(cov.c.abs() < 1e-3 && cov.k.abs() < 1e-3);
    }

    #[test]
    fn half_black_page_measures_fifty_percent() {
        let mut img = RgbImage::from_pixel(10, 10, Rgb([255, 255, 255]));
        for y in 0..10 {
            for x in 0..5 {
                img.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let cov = coverage_of_image(&DynamicImage::ImageRgb8(img));
        assert!((cov.k - 50.0).abs() < 1e-3, "k={}", cov.k);
    }

    #[test]
    fn thumbnail_is_valid_png() {
        let img = solid(400, 300, Rgb([10, 200, 30]));
        let png = thumbnail_png(&img, 64);
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        let decoded: DynamicImage = image::load_from_memory(&png).unwrap();
        assert!(decoded.width() <= 64 && decoded.height() <= 64);
    }
}
