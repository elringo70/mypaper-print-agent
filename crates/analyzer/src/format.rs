/// Spool payload format, sniffed from magic bytes. The Windows spooler stores
/// whatever the driver emitted: PDF/PS for print-to-file style drivers, XPS
/// or EMF for most GDI apps, PCL for laser printers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SpoolFormat {
    Pdf,
    PostScript,
    Xps,
    Emf,
    Pcl,
    Image,
    Unknown,
}

/// Image formats the raster analyzer reads directly (extension hint).
pub fn image_format_from_extension(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "bmp" | "ppm" | "pgm" | "tif" | "tiff" | "webp")
    )
}

pub fn detect_format(data: &[u8]) -> SpoolFormat {
    if data.starts_with(b"%PDF") {
        return SpoolFormat::Pdf;
    }
    if data.starts_with(b"%!") {
        return SpoolFormat::PostScript;
    }
    // ZIP local-file header: XPS packages are OPC zips.
    if data.starts_with(&[0x50, 0x4B, 0x03, 0x04]) {
        return SpoolFormat::Xps;
    }
    // ENHMETAHEADER: iType == 1 (EMR_HEADER), header size == 88.
    if data.len() >= 8 && data[0..4] == [1, 0, 0, 0] && data[4..8] == [88, 0, 0, 0] {
        return SpoolFormat::Emf;
    }
    // PCL escape sequences.
    if data.starts_with(&[0x1B]) {
        return SpoolFormat::Pcl;
    }
    // PNG / JPEG / BMP / PPM magic.
    if data.starts_with(&[0x89, b'P', b'N', b'G'])
        || data.starts_with(&[0xFF, 0xD8, 0xFF])
        || data.starts_with(b"BM")
        || data.starts_with(b"P6")
        || data.starts_with(b"P5")
    {
        return SpoolFormat::Image;
    }
    SpoolFormat::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_known_magics() {
        assert_eq!(detect_format(b"%PDF-1.7...."), SpoolFormat::Pdf);
        assert_eq!(detect_format(b"%!PS-Adobe-3.0"), SpoolFormat::PostScript);
        assert_eq!(
            detect_format(&[0x50, 0x4B, 0x03, 0x04, 0]),
            SpoolFormat::Xps
        );
        assert_eq!(
            detect_format(&[1, 0, 0, 0, 88, 0, 0, 0, 0, 0]),
            SpoolFormat::Emf
        );
        assert_eq!(detect_format(&[0x1B, b'E']), SpoolFormat::Pcl);
        assert_eq!(detect_format(&[0x89, b'P', b'N', b'G']), SpoolFormat::Image);
        assert_eq!(detect_format(b"whatever"), SpoolFormat::Unknown);
    }
}
