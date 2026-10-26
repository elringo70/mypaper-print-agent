//! Sending rendered pages to a printer.
//!
//! This is deliberately raster: the pages are what the analyzer measured, so
//! what the customer receives is what they were charged for. It also removes
//! the last dependency on an external interpreter to print — the previous
//! implementation shelled out to Ghostscript's `mswinpr2` device, which is
//! AGPL and therefore not something this product can ship.

use image::RgbaImage;

#[derive(Debug, thiserror::Error)]
pub enum PrintError {
    #[error("printer dc: {0}")]
    DeviceContext(String),
    #[error("start document on {printer}: {code}")]
    StartDoc { printer: String, code: i32 },
    #[error("print failed: {0}")]
    Page(String),
    #[error("no pages to print")]
    Empty,
    #[error("printing needs Windows (set MPA_MOCK_PRINT_DIR for tests)")]
    UnsupportedPlatform,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Print `pages` to `printer` (the exact spooler name).
///
/// `dpi` is the resolution the pages were rendered at; the printer's own DPI
/// is used to scale them onto the sheet.
pub fn print_images(printer: &str, pages: &[RgbaImage], dpi: u32) -> Result<(), PrintError> {
    if pages.is_empty() {
        return Err(PrintError::Empty);
    }
    platform_print(printer, pages, dpi)
}

#[cfg(windows)]
fn platform_print(printer: &str, pages: &[RgbaImage], dpi: u32) -> Result<(), PrintError> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::Xps::{EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let printer_w = wide(printer);
    // SAFETY: the string is NUL-terminated and outlives the call.
    let hdc = unsafe {
        CreateDCW(
            PCWSTR::null(),
            PCWSTR(printer_w.as_ptr()),
            PCWSTR::null(),
            None,
        )
    };
    if hdc.is_invalid() {
        return Err(PrintError::DeviceContext(printer.to_string()));
    }

    // SAFETY: GDI owns the DC until DeleteDC below; every early return goes
    // through this guard.
    struct DcGuard(HDC);
    impl Drop for DcGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }
    let dc = DcGuard(hdc);

    let doc_name = wide("MyPaper");
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR(doc_name.as_ptr()),
        ..Default::default()
    };
    // SAFETY: standard StartDoc/EndDoc bracket around one document.
    let started = unsafe { StartDocW(dc.0, &info) };
    if started <= 0 {
        return Err(PrintError::StartDoc {
            printer: printer.to_string(),
            code: started,
        });
    }

    let result = (|| -> Result<(), PrintError> {
        // SAFETY: GetDeviceCaps is valid on a printer DC.
        let printer_dpi = unsafe { GetDeviceCaps(Some(dc.0), LOGPIXELSX) }.max(1) as f64;
        for page in pages {
            let (w, h) = (page.width() as i32, page.height() as i32);
            if w == 0 || h == 0 {
                continue;
            }
            let target_w = (w as f64 * printer_dpi / dpi as f64).round() as i32;
            let target_h = (h as f64 * printer_dpi / dpi as f64).round() as i32;

            // 32bpp BITMAPINFO is BGRA; the alpha byte is padding here.
            let mut bgra = page.as_raw().clone();
            for px in bgra.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
                px[3] = 255;
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down rows, as the image crate stores them
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };

            // SAFETY: valid DC, valid DIB, dimensions come from the image.
            unsafe {
                if StartPage(dc.0) <= 0 {
                    return Err(PrintError::Page("StartPage".into()));
                }
                let drawn = StretchDIBits(
                    dc.0,
                    0,
                    0,
                    target_w,
                    target_h,
                    0,
                    0,
                    w,
                    h,
                    Some(bgra.as_ptr() as *const core::ffi::c_void),
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                let ended = EndPage(dc.0);
                if drawn == 0 || ended <= 0 {
                    return Err(PrintError::Page("StretchDIBits/EndPage".into()));
                }
            }
        }
        Ok(())
    })();

    // SAFETY: closes the document opened above.
    unsafe {
        let _ = EndDoc(dc.0);
    }
    result
}

/// Off Windows there is no GDI printer. Tests point `MPA_MOCK_PRINT_DIR` at a
/// directory and get one PNG per page, which is enough to assert that the
/// reprint path delivered the right pages.
#[cfg(not(windows))]
fn platform_print(printer: &str, pages: &[RgbaImage], _dpi: u32) -> Result<(), PrintError> {
    let dir = std::env::var("MPA_MOCK_PRINT_DIR").map_err(|_| PrintError::UnsupportedPlatform)?;
    let safe: String = printer
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    for (index, page) in pages.iter().enumerate() {
        page.save(dir.join(format!("{safe}-page{}.png", index + 1)))
            .map_err(|e| PrintError::Page(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    fn page() -> RgbaImage {
        RgbaImage::from_pixel(8, 6, image::Rgba([10, 20, 30, 255]))
    }

    #[test]
    fn empty_batch_is_rejected() {
        assert!(matches!(
            print_images("X", &[], 150),
            Err(PrintError::Empty)
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn mock_print_writes_one_file_per_page() {
        let dir = std::env::temp_dir().join(format!("mpa-print-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("MPA_MOCK_PRINT_DIR", &dir);
        print_images("My Printer", &[page(), page()], 150).unwrap();
        let files = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(files, 2);
        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("MPA_MOCK_PRINT_DIR");
    }
}
