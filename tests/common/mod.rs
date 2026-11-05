//! Shared fixtures: real EPUB / image files built on the fly (no mocks).
#![allow(dead_code, unreachable_pub, clippy::unwrap_used, clippy::missing_panics_doc)]

use image::{GrayImage, ImageFormat, Luma, Rgb, RgbImage};
use std::fmt::Write as _;
use std::io::{Cursor, Write};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

/// A chapter of a synthetic book.
pub struct Chapter {
    pub file: String,
    pub title: String,
    pub body: String,
}

/// (title, href, children as (title, href))
pub type TocEntry = (String, String, Vec<(String, String)>);

pub struct EpubSpec {
    pub title: String,
    pub author: String,
    pub language: String,
    pub uuid: Option<String>,
    pub chapters: Vec<Chapter>,
    /// (zip path under OEBPS/, bytes, media type)
    pub images: Vec<(String, Vec<u8>, String)>,
    pub cover: Option<String>,
    pub css: Option<String>,
    /// nested TOC
    pub toc: Vec<TocEntry>,
    pub epub3_nav: bool,
}

impl Default for EpubSpec {
    fn default() -> Self {
        Self {
            title: "Livro de Teste".into(),
            author: "Autora Teste".into(),
            language: "pt-BR".into(),
            uuid: Some("123e4567-e89b-12d3-a456-426614174000".into()),
            chapters: Vec::new(),
            images: Vec::new(),
            cover: None,
            css: Some("p { text-indent: 1em; } .x { color: red; }".into()),
            toc: Vec::new(),
            epub3_nav: false,
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn build_epub(spec: &EpubSpec) -> Vec<u8> {
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("mimetype", stored).unwrap();
    z.write_all(b"application/epub+zip").unwrap();
    z.start_file("META-INF/container.xml", stored).unwrap();
    z.write_all(br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();

    let mut manifest = String::new();
    let mut spine = String::new();
    for (i, c) in spec.chapters.iter().enumerate() {
        write!(manifest, r#"<item id="c{i}" href="{}" media-type="application/xhtml+xml"/>"#, c.file).unwrap();
        write!(spine, r#"<itemref idref="c{i}"/>"#).unwrap();
        z.start_file(format!("OEBPS/{}", c.file), stored).unwrap();
        let css = if spec.css.is_some() { r#"<link rel="stylesheet" type="text/css" href="style.css"/>"# } else { "" };
        write!(
            z,
            r#"<?xml version="1.0" encoding="utf-8"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>{}</title>{css}</head><body>{}</body></html>"#,
            esc(&c.title),
            c.body
        )
        .unwrap();
    }
    for (i, (path, data, mt)) in spec.images.iter().enumerate() {
        let id = if spec.cover.as_deref() == Some(path.as_str()) { "cover-img".to_owned() } else { format!("img{i}") };
        write!(manifest, r#"<item id="{id}" href="{path}" media-type="{mt}"/>"#).unwrap();
        z.start_file(format!("OEBPS/{path}"), stored).unwrap();
        z.write_all(data).unwrap();
    }
    if let Some(css) = &spec.css {
        manifest.push_str(r#"<item id="css" href="style.css" media-type="text/css"/>"#);
        z.start_file("OEBPS/style.css", stored).unwrap();
        z.write_all(css.as_bytes()).unwrap();
    }
    // TOC
    if spec.epub3_nav {
        manifest.push_str(r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#);
        let mut ol = String::from("<ol>");
        for (t, h, kids) in &spec.toc {
            write!(ol, r#"<li><a href="{h}">{}</a>"#, esc(t)).unwrap();
            if !kids.is_empty() {
                ol.push_str("<ol>");
                for (kt, kh) in kids {
                    write!(ol, r#"<li><a href="{kh}">{}</a></li>"#, esc(kt)).unwrap();
                }
                ol.push_str("</ol>");
            }
            ol.push_str("</li>");
        }
        ol.push_str("</ol>");
        z.start_file("OEBPS/nav.xhtml", stored).unwrap();
        write!(z, r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc">{ol}</nav></body></html>"#).unwrap();
    } else {
        manifest.push_str(r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#);
        let mut pts = String::new();
        let mut n = 0;
        for (t, h, kids) in &spec.toc {
            n += 1;
            write!(pts, r#"<navPoint id="n{n}" playOrder="{n}"><navLabel><text>{}</text></navLabel><content src="{h}"/>"#, esc(t)).unwrap();
            for (kt, kh) in kids {
                n += 1;
                write!(
                    pts,
                    r#"<navPoint id="n{n}" playOrder="{n}"><navLabel><text>{}</text></navLabel><content src="{kh}"/></navPoint>"#,
                    esc(kt)
                )
                .unwrap();
            }
            pts.push_str("</navPoint>");
        }
        z.start_file("OEBPS/toc.ncx", stored).unwrap();
        write!(
            z,
            r#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap>{pts}</navMap></ncx>"#
        )
        .unwrap();
    }
    let uuid = spec.uuid.as_ref().map(|u| format!(r#"<dc:identifier id="uid">urn:uuid:{u}</dc:identifier>"#)).unwrap_or_default();
    let cover_meta = spec.cover.as_ref().map(|_| r#"<meta name="cover" content="cover-img"/>"#).unwrap_or_default();
    z.start_file("OEBPS/content.opf", stored).unwrap();
    write!(
        z,
        r#"<?xml version="1.0" encoding="utf-8"?><package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf"><dc:title>{}</dc:title><dc:creator>{}</dc:creator><dc:language>{}</dc:language><dc:date>2026-01-02</dc:date>{uuid}{cover_meta}</metadata><manifest>{manifest}</manifest><spine toc="ncx">{spine}</spine><guide><reference type="text" title="Start" href="{}"/></guide></package>"#,
        esc(&spec.title),
        esc(&spec.author),
        spec.language,
        spec.chapters.first().map_or("", |c| c.file.as_str())
    )
    .unwrap();
    z.finish().unwrap().into_inner()
}

/// Low byte of a pixel value.
fn byte(v: u32) -> u8 {
    u8::try_from(v % 256).unwrap()
}

pub fn encode(img: &image::DynamicImage, fmt: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, fmt).unwrap();
    out.into_inner()
}

/// A colourful test picture.
pub fn picture(w: u32, h: u32, seed: u8) -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| {
        Rgb([byte(x * 7).wrapping_add(seed), byte(y * 3).wrapping_add(seed), byte((x + y) * 5)])
    }))
}

/// A webtoon strip: white background with dark "panels" separated by white gutters.
pub fn webtoon_strip(w: u32, h: u32, panels: &[(u32, u32)]) -> image::DynamicImage {
    let mut g = GrayImage::from_pixel(w, h, Luma([255]));
    for &(top, bottom) in panels {
        for y in top..bottom.min(h) {
            for x in 20..w - 20 {
                g.put_pixel(x, y, Luma([byte((x / 3 + y / 5) % 120 + 20)]));
            }
        }
    }
    image::DynamicImage::ImageLuma8(g)
}

/// Long chapter body with unique, checkable sentences.
pub fn prose(tag: &str, paragraphs: usize) -> String {
    let mut out = String::new();
    for i in 0..paragraphs {
        writeln!(out, "<p id=\"{tag}-p{i}\">Frase única {tag} número {i}: Klein abriu os olhos &amp; viu o nevoeiro — ação, coração, 诡秘之主, 괴담.</p>").unwrap();
    }
    out
}
