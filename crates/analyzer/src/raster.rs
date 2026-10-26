use image::DynamicImage;

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
    // Same semantics as `chunks_exact(4)`: a trailing partial pixel is dropped.
    for px in rgba.as_chunks::<4>().0 {
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
    fn thumbnail_is_valid_png() {
        let img = solid(400, 300, Rgb([10, 200, 30]));
        let png = thumbnail_png(&img, 64);
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        let decoded: DynamicImage = image::load_from_memory(&png).unwrap();
        assert!(decoded.width() <= 64 && decoded.height() <= 64);
    }
}
