//! Minimal PDF writer for comics.
//!
//! One page per (already screen-sized) JPEG, embedded as-is with `DCTDecode`
//! (no recompression), plus an outline entry per chapter so readers like
//! `KOReader` show a chapter table of contents.

use crate::comic::Rendered;
use anyhow::{Result, anyhow};
use std::fmt::Write as _;

/// Width, height and colour components of a baseline/progressive JPEG.
fn jpeg_info(d: &[u8]) -> Option<(u32, u32, u8)> {
    let mut i = 2;
    while i + 9 < d.len() {
        if d[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = d[i + 1];
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let h = u32::from(u16::from_be_bytes([d[i + 5], d[i + 6]]));
            let w = u32::from(u16::from_be_bytes([d[i + 7], d[i + 8]]));
            return Some((w, h, d[i + 9]));
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([*d.get(i + 2)?, *d.get(i + 3)?]));
        i += 2 + len;
    }
    None
}

/// PDF text string as UTF-16BE hex with BOM (safe for any title).
fn text_string(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        let _ = write!(out, "{u:04X}");
    }
    out.push('>');
    out
}

struct Writer {
    buf: Vec<u8>,
    offsets: Vec<usize>, // index = object number - 1
}

impl Writer {
    fn obj(&mut self, num: usize, body: &[u8]) {
        if self.offsets.len() < num {
            self.offsets.resize(num, 0);
        }
        self.offsets[num - 1] = self.buf.len();
        self.buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        self.buf.extend_from_slice(body);
        self.buf.extend_from_slice(b"\nendobj\n");
    }

    fn stream(&mut self, num: usize, dict: &str, data: &[u8]) {
        let mut body = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.obj(num, &body);
    }
}

/// Build the PDF.
///
/// # Errors
/// Fails if a page is not a JPEG or there are no pages.
pub fn to_pdf(r: &Rendered) -> Result<Vec<u8>> {
    let pages: Vec<(usize, &[u8])> =
        r.chapters.iter().enumerate().flat_map(|(ci, c)| c.pages.iter().map(move |p| (ci, p.as_slice()))).collect();
    if pages.is_empty() {
        return Err(anyhow!("nenhuma página"));
    }
    // object layout: 1 catalog, 2 pages, 3 outlines, 4 info,
    // then per page: page, content, image; then one per chapter outline item
    let page_obj = |i: usize| 5 + 3 * i;
    let first_outline = 5 + 3 * pages.len();
    let total_estimate: usize = pages.iter().map(|p| p.1.len() + 300).sum();
    let mut w = Writer { buf: Vec::with_capacity(total_estimate + 4096), offsets: Vec::new() };
    w.buf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let mut first_page_of_chapter: Vec<Option<usize>> = vec![None; r.chapters.len()];
    for (i, (ci, jpeg)) in pages.iter().enumerate() {
        let (pw, ph, comps) = jpeg_info(jpeg).ok_or_else(|| anyhow!("página {i} não é JPEG"))?;
        let cs = if comps == 1 { "/DeviceGray" } else { "/DeviceRGB" };
        let (po, co, io) = (page_obj(i), page_obj(i) + 1, page_obj(i) + 2);
        first_page_of_chapter[*ci].get_or_insert(po);
        w.obj(
            po,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /XObject << /Im {io} 0 R >> >> /Contents {co} 0 R >>"
            )
            .as_bytes(),
        );
        w.stream(co, "", format!("q {pw} 0 0 {ph} 0 0 cm /Im Do Q").as_bytes());
        w.stream(
            io,
            &format!("/Type /XObject /Subtype /Image /Width {pw} /Height {ph} /ColorSpace {cs} /BitsPerComponent 8 /Filter /DCTDecode"),
            jpeg,
        );
    }

    // outline: one entry per non-empty chapter
    let chapters: Vec<(&str, usize)> =
        r.chapters.iter().zip(&first_page_of_chapter).filter_map(|(c, p)| p.map(|p| (c.title.as_str(), p))).collect();
    let n = chapters.len();
    for (k, (title, page)) in chapters.iter().enumerate() {
        let mut d = format!("<< /Title {} /Parent 3 0 R /Dest [{page} 0 R /Fit]", text_string(title));
        if k > 0 {
            let _ = write!(d, " /Prev {} 0 R", first_outline + k - 1);
        }
        if k + 1 < n {
            let _ = write!(d, " /Next {} 0 R", first_outline + k + 1);
        }
        d.push_str(" >>");
        w.obj(first_outline + k, d.as_bytes());
    }
    let outlines = if n == 0 {
        "<< /Type /Outlines /Count 0 >>".to_owned()
    } else {
        format!("<< /Type /Outlines /First {first_outline} 0 R /Last {} 0 R /Count {n} >>", first_outline + n - 1)
    };
    w.obj(3, outlines.as_bytes());
    let kids: String = (0..pages.len()).map(|i| format!("{} 0 R", page_obj(i))).collect::<Vec<_>>().join(" ");
    w.obj(2, format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).as_bytes());
    w.obj(1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R /PageMode /UseOutlines >>");
    w.obj(4, format!("<< /Title {} /Producer (kindle-turbo) >>", text_string(&r.title)).as_bytes());

    let xref_at = w.buf.len();
    let count = w.offsets.len() + 1;
    let mut xref = format!("xref\n0 {count}\n0000000000 65535 f \n");
    for off in &w.offsets {
        let _ = writeln!(xref, "{off:010} 00000 n ");
    }
    let _ = write!(xref, "trailer\n<< /Size {count} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref_at}\n%%EOF\n");
    w.buf.extend_from_slice(xref.as_bytes());
    Ok(w.buf)
}
