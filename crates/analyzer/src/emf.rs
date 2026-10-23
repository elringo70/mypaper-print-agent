use image::RgbaImage;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmfError {
    #[error("not an EMF spool file (no ENHMETAHEADER found)")]
    NotEmf,
    #[error("rendering EMF needs Windows GDI")]
    NeedsWindows,
    #[error("GDI playback failed: {0}")]
    Gdi(String),
    #[error("image encode: {0}")]
    Image(#[from] image::ImageError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// ENHMETAHEADER layout (all u32/i32 little-endian, 88 bytes).
const HEADER_SIZE: usize = 88;
/// iType == EMR_HEADER.
const EMR_HEADER: u32 = 1;
/// dSignature == "FME " as u32 LE.
const ENHMETA_SIGNATURE: u32 = 0x464D4520;

/// One EMF page: byte range plus its physical size in millimeters, decoded
/// from rclFrame (0.01mm units). Letter fallback when the frame is empty.
#[derive(Debug, Clone)]
pub struct EmfPage {
    pub bytes: Vec<u8>,
    pub width_mm: f32,
    pub height_mm: f32,
}

#[derive(Debug, Clone, Copy)]
struct EmfHeader {
    total_bytes: u32,
    width_mm: f32,
    height_mm: f32,
}

fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    data.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_i32_le(data: &[u8], off: usize) -> Option<i32> {
    data.get(off..off + 4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Parse an ENHMETAHEADER at `off`. Returns `None` when the bytes do not look
/// like a header (caller keeps scanning).
fn parse_header_at(data: &[u8], off: usize) -> Option<EmfHeader> {
    if read_u32_le(data, off)? != EMR_HEADER {
        return None;
    }
    if data.len() - off < HEADER_SIZE {
        return None;
    }
    if read_u32_le(data, off + 40)? != ENHMETA_SIGNATURE {
        return None;
    }
    let total_bytes = read_u32_le(data, off + 48)?;
    if total_bytes < HEADER_SIZE as u32 {
        return None;
    }
    let (left, top, right, bottom) = (
        read_i32_le(data, off + 24)?,
        read_i32_le(data, off + 28)?,
        read_i32_le(data, off + 32)?,
        read_i32_le(data, off + 36)?,
    );
    let (mut width_mm, mut height_mm) =
        ((right - left) as f32 / 100.0, (bottom - top) as f32 / 100.0);
    if width_mm <= 0.0 || height_mm <= 0.0 {
        // Carta fallback: the Mexican office standard when the driver
        // leaves rclFrame empty.
        width_mm = 215.9;
        height_mm = 279.4;
    }
    Some(EmfHeader {
        total_bytes,
        width_mm,
        height_mm,
    })
}

/// Split a spool file into EMF pages. Windows concatenates one ENHMETAHEADER
/// per page; each header's nBytes delimits its page when sane, otherwise the
/// next header (or EOF) does. Trailing padding is ignored.
pub fn split_pages(data: &[u8]) -> Result<Vec<EmfPage>, EmfError> {
    // Header offsets first: records are u32-aligned.
    let mut headers: Vec<(usize, EmfHeader)> = Vec::new();
    let mut off = 0;
    while off + HEADER_SIZE <= data.len() {
        if let Some(header) = parse_header_at(data, off) {
            headers.push((off, header));
            // Skip ahead past this header to avoid matching its own innards;
            // page bodies cannot contain a valid header before nBytes ends,
            // but scanning from there is still correct, just slower.
            off += 4;
        } else {
            off += 4;
        }
    }
    if headers.is_empty() {
        return Err(EmfError::NotEmf);
    }
    let mut pages = Vec::with_capacity(headers.len());
    for (i, (start, header)) in headers.iter().enumerate() {
        let next_header = headers
            .get(i + 1)
            .map(|(off, _)| *off)
            .unwrap_or(data.len());
        let claimed_end = start
            .saturating_add(header.total_bytes as usize)
            .min(data.len());
        // Trust nBytes only when it lands on (or past) the next header;
        // otherwise the next header wins and padding stays out.
        let end = if claimed_end >= next_header {
            next_header
        } else if claimed_end > start + HEADER_SIZE {
            claimed_end
        } else {
            next_header
        };
        pages.push(EmfPage {
            bytes: data[*start..end].to_vec(),
            width_mm: header.width_mm,
            height_mm: header.height_mm,
        });
    }
    Ok(pages)
}

/// Rasterize one EMF page through GDI playback (Windows only). Dimensions
/// come from the header the splitter already validated — no re-parsing.
#[cfg(windows)]
pub fn rasterize_page(
    emf: &[u8],
    width_mm: f32,
    height_mm: f32,
    dpi: u32,
) -> Result<RgbaImage, EmfError> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::*;

    // SAFETY: the slice form copies the source into a GDI-managed handle.
    let hemf = unsafe { SetEnhMetaFileBits(emf) };
    if hemf.is_invalid() {
        return Err(EmfError::Gdi("SetEnhMetaFileBits rejected the page".into()));
    }

    struct Guards {
        hemf: HENHMETAFILE,
        hdc: HDC,
        hbmp: HBITMAP,
    }
    impl Drop for Guards {
        fn drop(&mut self) {
            // SAFETY: handles came from successful GDI calls above.
            unsafe {
                let _ = DeleteObject(self.hbmp.into());
                let _ = DeleteDC(self.hdc);
                let _ = DeleteEnhMetaFile(Some(self.hemf));
            }
        }
    }

    let (w_px, h_px) = (
        (width_mm / 25.4 * dpi as f32).round().max(1.0) as u32,
        (height_mm / 25.4 * dpi as f32).round().max(1.0) as u32,
    );

    // SAFETY: every call below follows the documented GDI memory-DC pattern;
    // pointers stay valid through DeleteDC, errors abort before use.
    unsafe {
        let hdc = CreateCompatibleDC(None);
        if hdc.is_invalid() {
            let _ = DeleteEnhMetaFile(Some(hemf));
            return Err(EmfError::Gdi("CreateCompatibleDC failed".into()));
        }
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w_px as i32;
        // Negative height = top-down DIB, no row flip needed later.
        bmi.bmiHeader.biHeight = -(h_px as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbmp = match CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(hbmp) if !hbmp.is_invalid() && !bits.is_null() => hbmp,
            _ => {
                let _ = DeleteDC(hdc);
                let _ = DeleteEnhMetaFile(Some(hemf));
                return Err(EmfError::Gdi("CreateDIBSection failed".into()));
            }
        };
        // Paint paper-white first so transparent regions stay blank, not black.
        // Named with underscore: the binding must live until scope end so its
        // Drop releases the handles; only the name is unused.
        let _guards = Guards { hemf, hdc, hbmp };
        SelectObject(hdc, hbmp.into());
        let _ = PatBlt(hdc, 0, 0, w_px as i32, h_px as i32, WHITENESS);
        let rect = RECT {
            left: 0,
            top: 0,
            right: w_px as i32,
            bottom: h_px as i32,
        };
        if !PlayEnhMetaFile(hdc, hemf, &rect).as_bool() {
            return Err(EmfError::Gdi("PlayEnhMetaFile failed".into()));
        }
        // BGRA → RGBA straight copy from the top-down DIB.
        let raw = std::slice::from_raw_parts(bits as *const u8, (w_px * h_px * 4) as usize);
        let mut rgba = Vec::with_capacity(raw.len());
        // `as_chunks` keeps the same semantics as `chunks_exact(4)`: the tail
        // (a partial pixel) is dropped.
        for px in raw.as_chunks::<4>().0 {
            rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        RgbaImage::from_raw(w_px, h_px, rgba)
            .ok_or_else(|| EmfError::Gdi("zero-size render".into()))
    }
}

/// Rasterize one EMF page. Outside Windows there is no GDI: callers get a
/// clear error instead of a silent blank page.
#[cfg(not(windows))]
pub fn rasterize_page(
    _emf: &[u8],
    _width_mm: f32,
    _height_mm: f32,
    _dpi: u32,
) -> Result<RgbaImage, EmfError> {
    Err(EmfError::NeedsWindows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal synthetic spool: two ENHMETAHEADERs with consistent nBytes.
    fn two_page_spool() -> Vec<u8> {
        let mut spool = Vec::new();
        for (w01, h01) in [(21590i32, 27940i32), (21000i32, 29700i32)] {
            let body = vec![0xAAu8; 64];
            let total = (HEADER_SIZE + body.len()) as u32;
            let mut hdr = vec![0u8; HEADER_SIZE];
            hdr[0..4].copy_from_slice(&1u32.to_le_bytes());
            hdr[4..8].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
            hdr[24..28].copy_from_slice(&0i32.to_le_bytes());
            hdr[28..32].copy_from_slice(&0i32.to_le_bytes());
            hdr[32..36].copy_from_slice(&w01.to_le_bytes());
            hdr[36..40].copy_from_slice(&h01.to_le_bytes());
            hdr[40..44].copy_from_slice(&ENHMETA_SIGNATURE.to_le_bytes());
            hdr[44..48].copy_from_slice(&1u32.to_le_bytes());
            hdr[48..52].copy_from_slice(&total.to_le_bytes());
            hdr[52..56].copy_from_slice(&2u32.to_le_bytes());
            spool.extend_from_slice(&hdr);
            spool.extend_from_slice(&body);
        }
        spool
    }

    #[test]
    fn splits_pages_with_sizes() {
        let pages = split_pages(&two_page_spool()).unwrap();
        assert_eq!(pages.len(), 2);
        assert!((pages[0].width_mm - 215.9).abs() < 0.01);
        assert!((pages[0].height_mm - 279.4).abs() < 0.01);
        assert!((pages[1].width_mm - 210.0).abs() < 0.01);
        assert_eq!(pages[0].bytes.len(), HEADER_SIZE + 64);
    }

    #[test]
    fn empty_frame_falls_back_to_letter() {
        let mut spool = two_page_spool();
        // Zero rclFrame on the first page.
        spool[24..40].fill(0);
        let pages = split_pages(&spool).unwrap();
        assert!((pages[0].width_mm - 215.9).abs() < 0.01);
        assert!((pages[0].height_mm - 279.4).abs() < 0.01);
    }

    #[test]
    fn rejects_non_emf() {
        assert!(matches!(split_pages(b"%PDF-1.4"), Err(EmfError::NotEmf)));
        assert!(matches!(split_pages(&[]), Err(EmfError::NotEmf)));
    }

    #[test]
    fn trailing_padding_does_not_glue_pages() {
        let mut spool = two_page_spool();
        spool.extend_from_slice(&[0u8; 128]);
        let pages = split_pages(&spool).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1].bytes.len(), HEADER_SIZE + 64);
    }
}
