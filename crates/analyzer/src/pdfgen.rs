use thiserror::Error;

#[derive(Debug, Error)]
pub enum PdfGenError {
    #[error("no pages to write")]
    Empty,
}

/// One full-bleed raster page: JPEG bytes plus its physical size. The writer
/// embeds each image at exact size (no scaling surprises on reprint).
#[derive(Debug)]
pub struct PdfRasterPage {
    pub width_mm: f32,
    pub height_mm: f32,
    pub width_px: u32,
    pub height_px: u32,
    pub jpeg: Vec<u8>,
}

/// Minimal PDF writer: catalog + pages + one DCTDecode image per page.
/// Hand-rolled on purpose: zero new dependencies, deterministic output, and
/// the xref table is unit-verified below (same pattern as the pipeline's
/// `minimal_pdf` test fixture).
pub fn images_to_pdf(pages: &[PdfRasterPage]) -> Result<Vec<u8>, PdfGenError> {
    if pages.is_empty() {
        return Err(PdfGenError::Empty);
    }
    // Object numbering: 1 = catalog, 2 = pages, then triples per page.
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());

    let kids: Vec<String> = (0..pages.len())
        .map(|i| {
            let page_obj = 3 + i * 3;
            format!("{page_obj} 0 R")
        })
        .collect();
    objects.push(
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        )
        .into_bytes(),
    );

    for (i, page) in pages.iter().enumerate() {
        let page_obj = 3 + i * 3;
        let content_obj = page_obj + 1;
        let image_obj = page_obj + 2;
        let (w_pt, h_pt) = (mm_to_pt(page.width_mm), mm_to_pt(page.height_mm));
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w_pt:.2} {h_pt:.2}] \
                 /Resources << /XObject << /Im{i} {image_obj} 0 R >> >> /Contents {content_obj} 0 R >>"
            )
            .into_bytes(),
        );
        let content = format!("q {w_pt:.2} 0 0 {h_pt:.2} 0 0 cm /Im{i} Do Q");
        objects.push(content.into_bytes());
        let mut img = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            page.width_px,
            page.height_px,
            page.jpeg.len()
        )
        .into_bytes();
        img.extend_from_slice(&page.jpeg);
        img.extend_from_slice(b"\nendstream");
        objects.push(img);
    }

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF",
            objects.len() + 1
        )
        .as_bytes(),
    );
    Ok(pdf)
}

fn mm_to_pt(mm: f32) -> f32 {
    mm * 72.0 / 25.4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> PdfRasterPage {
        // Minimal valid JPEG: SOI + EOI. Parsers accept it as 0x0 image;
        // the writer only needs Length, never decodes.
        PdfRasterPage {
            width_mm: 215.9,
            height_mm: 279.4,
            width_px: IMAGE_W,
            height_px: IMAGE_H,
            jpeg: vec![0xFF, 0xD8, 0xFF, 0xD9],
        }
    }

    const IMAGE_W: u32 = 100;
    const IMAGE_H: u32 = 100;

    #[test]
    fn rejects_empty_input() {
        assert!(matches!(images_to_pdf(&[]), Err(PdfGenError::Empty)));
    }

    /// Byte-oriented line splitter: the file embeds binary JPEG, and
    /// `from_utf8_lossy` would replace invalid sequences (shifting every
    /// later byte offset), so offset verification must stay on raw bytes.
    fn pdf_lines(pdf: &[u8]) -> Vec<&[u8]> {
        pdf.split(|b| *b == b'\n').collect()
    }

    #[test]
    fn xref_offsets_point_at_their_objects() {
        let pdf = images_to_pdf(&[page(), page()]).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.ends_with(b"%%EOF"));

        let marker = b"startxref\n";
        let sx = pdf
            .windows(marker.len())
            .rposition(|w| w == marker)
            .expect("startxref present")
            + marker.len();
        let eol = pdf[sx..].iter().position(|b| *b == b'\n').unwrap();
        let xref_at: usize = std::str::from_utf8(&pdf[sx..sx + eol])
            .unwrap()
            .parse()
            .unwrap();

        let section = pdf_lines(&pdf[xref_at..]);
        assert_eq!(section[0], b"xref");
        let counts: Vec<usize> = section[1]
            .split(|b| *b == b' ')
            .map(|n| std::str::from_utf8(n).unwrap().parse().unwrap())
            .collect();
        // 2 catalog objects + 2 pages × 3 objects each + free head.
        assert_eq!(counts, vec![0, 9]);
        for (obj_no, entry) in section.iter().skip(2).take(counts[1]).enumerate() {
            if obj_no == 0 {
                assert!(entry.ends_with(b"f "));
                continue;
            }
            let offset: usize = std::str::from_utf8(&entry[..10]).unwrap().parse().unwrap();
            let marker = format!("{obj_no} 0 obj");
            assert!(
                pdf[offset..].starts_with(marker.as_bytes()),
                "object {obj_no} misaddressed"
            );
        }
    }

    #[test]
    fn media_box_matches_physical_size() {
        let pdf = images_to_pdf(&[page()]).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        // 215.9mm × 279.4mm carta → 612.00 × 792.00 pt.
        assert!(text.contains("/MediaBox [0 0 612.00 792.00]"));
        assert!(text.contains("/Width 100 /Height 100"));
        assert!(text.contains("/Filter /DCTDecode"));
    }
}
