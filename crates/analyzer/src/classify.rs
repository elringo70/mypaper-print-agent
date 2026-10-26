//! Pixel-level coverage metrics and page classification.
//!
//! This is the single measurement path of the analyzer. It mirrors, term for
//! term, the reference implementation used to build the CyberPlanet study's
//! ground truth (`tools/render_truth.js`), so a page measured here and the same
//! page measured by the study produce the same numbers on the same pixels.
//!
//! Nothing here renders. The caller supplies decoded RGB(A) pixels, whatever
//! produced them (GDI playback of an EMF spool file, pdfium for a PDF, the
//! `image` crate for a raster payload). Keeping rendering out means one
//! estimator for every input format, which is the whole point: before this
//! module the agent measured PDFs with a device-based tool and images with a
//! naive formula, so the same photo priced differently depending on how the
//! customer sent it.

use mpa_domain::Coverage;

/// Fixed thresholds. They are part of the engine identity: every analysis
/// carries them, and changing any value changes `ENGINE_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Analysis resolution. The reference corpus is 150 dpi.
    pub dpi: u32,
    /// Per-channel value (0-255) above which a pixel carries no ink.
    pub noblanco_canal: u8,
    /// Chroma (max-min, normalized) above which a pixel is "color".
    pub croma_pixel: f64,
    /// Chroma tolerance under which a pixel is neutral (grey).
    pub gris_tol_croma: f64,
    /// A color channel coverage above this makes the page a color page.
    pub class_color_ink_min: f64,
    /// At or below this non-white coverage the page is blank.
    pub class_blanco_max: f64,
    /// Share of inked pixels that must be dark for the page to be mono.
    pub class_mono_dark_ratio: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            dpi: 150,
            noblanco_canal: 248,
            croma_pixel: 0.04,
            gris_tol_croma: 0.008,
            class_color_ink_min: 0.002,
            class_blanco_max: 0.004,
            class_mono_dark_ratio: 0.5,
        }
    }
}

/// Engine identity, stamped on every analysis so a price can never be
/// attributed to a different measurement.
pub const ENGINE_VERSION: &str = "2.0.0";

/// What the sheet is, as far as billing is concerned.
///
/// `Blank` is free. `Grey` is billed on the color ladder: greys count as solid
/// color, which is the agreed rule and the historical behaviour. Only `Mono`
/// reaches the black-and-white tier, and it requires the ink to be pure black,
/// not merely neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageClass {
    Blanco,
    Color,
    Gris,
    Mono,
}

impl PageClass {
    /// Stable wire name, matching the `serde` representation.
    pub fn as_str(self) -> &'static str {
        match self {
            PageClass::Blanco => "blanco",
            PageClass::Color => "color",
            PageClass::Gris => "gris",
            PageClass::Mono => "mono",
        }
    }

    /// Whether the page is charged at all.
    pub fn is_billable(self) -> bool {
        !matches!(self, PageClass::Blanco)
    }

    /// Whether the page is billed on the color ladder.
    pub fn uses_color_ladder(self) -> bool {
        matches!(self, PageClass::Color | PageClass::Gris)
    }
}

/// Per-page measurements, all normalized to 0-1 except `page_area_cm2`.
///
/// Field names follow the reference implementation so the two can be compared
/// directly, and so an audit trail can point at the same nouns.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PageMetrics {
    pub width: u32,
    pub height: u32,
    pub page_area_cm2: f64,
    /// Fraction of pixels carrying any ink.
    pub coverage_nonwhite: f64,
    pub coverage_c: f64,
    pub coverage_m: f64,
    pub coverage_y: f64,
    pub coverage_k: f64,
    /// Mean ink assuming monochrome: `1 - luminance`.
    pub coverage_lum: f64,
    pub dark_fraction: f64,
    pub color_pixel_frac: f64,
    pub gris_pixel_frac: f64,
    /// Among inked pixels, the share that is pure black (K >= 95%, no chroma).
    /// This is the only thing separating "black text" from "grey".
    pub pure_black_frac: f64,
    pub mean_chroma: f64,
    pub class: PageClass,
}

impl PageMetrics {
    /// Coverage of the sheet, per channel, in percent (0-100).
    pub fn coverage(&self) -> Coverage {
        Coverage::new(
            (self.coverage_c * 100.0) as f32,
            (self.coverage_m * 100.0) as f32,
            (self.coverage_y * 100.0) as f32,
            (self.coverage_k * 100.0) as f32,
        )
    }

    /// The scalar the area-based tier ladder bills on, in percent (0-100).
    /// "Area" means the share of the sheet that carries any ink.
    pub fn area_pct(&self) -> f64 {
        (self.coverage_nonwhite * 100.0).max(0.0)
    }
}

/// Analyze decoded 8-bit RGBA pixels.
///
/// `rgba` is row-major, 4 bytes per pixel; alpha is ignored (the reference
/// implementation composes over paper-white before measuring). Returns `None`
/// for a zero-sized bitmap.
pub fn analyze_rgba(rgba: &[u8], width: u32, height: u32) -> Option<PageMetrics> {
    analyze_rgba_with(rgba, width, height, Thresholds::default())
}

/// [`analyze_rgba`] with explicit thresholds (tests and calibration).
pub fn analyze_rgba_with(
    rgba: &[u8],
    width: u32,
    height: u32,
    th: Thresholds,
) -> Option<PageMetrics> {
    let total_px = (width as u64).checked_mul(height as u64)?;
    if total_px == 0 || rgba.len() < (total_px as usize) * 4 {
        return None;
    }
    let n = total_px as f64;
    let cut = th.noblanco_canal as f64 / 255.0;

    let mut inked: u64 = 0;
    let mut dark: u64 = 0;
    let mut color_px: u64 = 0;
    let mut gris_px: u64 = 0;
    let mut pure_black: u64 = 0;
    let mut s_k = 0.0f64;
    let mut s_c = 0.0f64;
    let mut s_m = 0.0f64;
    let mut s_y = 0.0f64;
    let mut s_lum = 0.0f64;
    let mut s_chroma = 0.0f64;

    for px in rgba.as_chunks::<4>().0 {
        let r = px[0] as f64 / 255.0;
        let g = px[1] as f64 / 255.0;
        let b = px[2] as f64 / 255.0;
        let mx = r.max(g).max(b);
        let mn = r.min(g).min(b);
        let chroma = mx - mn;
        // Ink decomposition: the common part of the three absorptions is black.
        let k = (1.0 - mx).min(1.0 - r).min(1.0 - g).min(1.0 - b).max(0.0);
        let c = (1.0 - r - k).max(0.0);
        let m = (1.0 - g - k).max(0.0);
        let y = (1.0 - b - k).max(0.0);
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;

        s_k += k;
        s_c += c;
        s_m += m;
        s_y += y;
        s_lum += 1.0 - lum;
        s_chroma += chroma;

        let is_inked = r < cut || g < cut || b < cut;
        if is_inked {
            inked += 1;
            if k >= 0.95 && chroma <= th.gris_tol_croma {
                pure_black += 1;
            }
            if lum < 0.5 {
                dark += 1;
            } else if chroma <= th.gris_tol_croma {
                gris_px += 1;
            }
            if chroma > th.croma_pixel {
                color_px += 1;
            }
        }
    }

    let r4 = |x: f64| (x * 10_000.0).round() / 10_000.0;
    let coverage_c = r4(s_c / n);
    let coverage_m = r4(s_m / n);
    let coverage_y = r4(s_y / n);
    let coverage_nonwhite = r4(inked as f64 / n);

    // Classification uses raw counts, not the rounded fractions: rounding first
    // would flip borderline pages.
    let chroma_ink = coverage_c.max(coverage_m).max(coverage_y);
    let class = if chroma_ink > th.class_color_ink_min {
        PageClass::Color
    } else if coverage_nonwhite <= th.class_blanco_max {
        PageClass::Blanco
    } else if (dark as f64) >= (inked as f64) * th.class_mono_dark_ratio {
        PageClass::Mono
    } else {
        PageClass::Gris
    };

    let mm_px = 25.4 / th.dpi as f64;
    let page_area_cm2 = r4((width as f64 * mm_px / 10.0) * (height as f64 * mm_px / 10.0));

    Some(PageMetrics {
        width,
        height,
        page_area_cm2,
        coverage_nonwhite,
        coverage_c,
        coverage_m,
        coverage_y,
        coverage_k: r4(s_k / n),
        coverage_lum: r4(s_lum / n),
        dark_fraction: r4(dark as f64 / n),
        color_pixel_frac: r4(color_px as f64 / n),
        gris_pixel_frac: r4(gris_px as f64 / n),
        pure_black_frac: r4(if inked == 0 {
            0.0
        } else {
            pure_black as f64 / inked as f64
        }),
        mean_chroma: r4(s_chroma / n),
        class,
    })
}

/// Convenience wrapper for an already decoded image.
pub fn analyze_image(img: &image::DynamicImage) -> Option<PageMetrics> {
    let rgba = img.to_rgba8();
    analyze_rgba(rgba.as_raw(), rgba.width(), rgba.height())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        RgbaImage::from_pixel(w, h, Rgba(px)).into_raw()
    }

    fn half_black(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbaImage::from_pixel(w, h, Rgba([255, 255, 255, 255]));
        for y in 0..h {
            for x in 0..w / 2 {
                img.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        img.into_raw()
    }

    #[test]
    fn white_page_is_blank_and_free() {
        let m = analyze_rgba(&solid(64, 64, [255, 255, 255, 255]), 64, 64).unwrap();
        assert_eq!(m.coverage_nonwhite, 0.0);
        assert_eq!(m.class, PageClass::Blanco);
        assert!(!m.class.is_billable());
        assert_eq!(m.area_pct(), 0.0);
    }

    #[test]
    fn black_text_page_is_mono() {
        let m = analyze_rgba(&half_black(64, 64), 64, 64).unwrap();
        assert_eq!(m.class, PageClass::Mono);
        assert!(m.class.is_billable());
        assert!(!m.class.uses_color_ladder());
        assert!((m.coverage_nonwhite - 0.5).abs() < 1e-6);
        assert!((m.coverage_k - 0.5).abs() < 1e-6);
        assert!((m.pure_black_frac - 1.0).abs() < 1e-9);
        assert_eq!(m.area_pct(), 50.0);
    }

    /// The rule the owner asked for: a neutral grey sheet is solid color, not
    /// black and white. With the naive K-only separation it used to bill ByN.
    #[test]
    fn grey_page_bills_on_the_color_ladder() {
        let m = analyze_rgba(&solid(32, 32, [128, 128, 128, 255]), 32, 32).unwrap();
        assert_eq!(m.class, PageClass::Gris);
        assert!(m.class.uses_color_ladder());
        assert!((m.coverage_k - 0.498).abs() < 1e-3);
    }

    #[test]
    fn saturated_page_is_color() {
        let m = analyze_rgba(&solid(32, 32, [255, 0, 0, 255]), 32, 32).unwrap();
        assert_eq!(m.class, PageClass::Color);
        assert!((m.coverage_m - 1.0).abs() < 1e-9);
        assert!((m.coverage_y - 1.0).abs() < 1e-9);
        assert_eq!(m.coverage_c, 0.0);
    }

    #[test]
    fn near_white_pixels_do_not_count_as_ink() {
        // 250/255 is above the 248 cut: no ink, so the page is blank.
        let m = analyze_rgba(&solid(16, 16, [250, 250, 250, 255]), 16, 16).unwrap();
        assert_eq!(m.coverage_nonwhite, 0.0);
        assert_eq!(m.class, PageClass::Blanco);
    }

    /// Below the color threshold the page is genuinely blank: a stray pixel is
    /// not ink. Documented behaviour of the reference rule, not an accident.
    #[test]
    fn a_stray_pixel_stays_blank() {
        let mut img = RgbaImage::from_pixel(100, 100, Rgba([255, 255, 255, 255]));
        img.put_pixel(10, 10, Rgba([0, 128, 255, 255]));
        let m = analyze_rgba(img.as_raw(), 100, 100).unwrap();
        assert_eq!(m.class, PageClass::Blanco);
        assert!(m.coverage_nonwhite > 0.0);
    }

    /// Once a colored area passes 0.2% of a channel the page bills as color.
    #[test]
    fn a_visible_color_block_is_color() {
        let mut img = RgbaImage::from_pixel(100, 100, Rgba([255, 255, 255, 255]));
        for y in 0..20 {
            for x in 0..20 {
                img.put_pixel(x, y, Rgba([0, 128, 255, 255]));
            }
        }
        let m = analyze_rgba(img.as_raw(), 100, 100).unwrap();
        assert_eq!(m.class, PageClass::Color);
        assert!(m.coverage_m > 0.002);
    }

    #[test]
    fn area_matches_paper_size() {
        // 150 dpi A4 is 1240 x 1754 px -> ~623.65 cm².
        let m = analyze_rgba(&solid(1240, 1754, [255, 255, 255, 255]), 1240, 1754).unwrap();
        assert!(
            (m.page_area_cm2 - 623.65).abs() < 0.1,
            "{}",
            m.page_area_cm2
        );
    }

    #[test]
    fn truncated_buffer_is_rejected() {
        assert!(analyze_rgba(&[0, 0, 0, 0], 10, 10).is_none());
        assert!(analyze_rgba(&[], 0, 0).is_none());
    }
}
