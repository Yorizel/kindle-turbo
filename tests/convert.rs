//! End-to-end tests: real EPUBs -> AZW3 -> decoded again by the independent reader.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing, clippy::too_many_lines)]

mod common;

use common::{Chapter, EpubSpec, build_epub, encode, picture, prose};
use image::{ImageEncoder, ImageFormat};
use kindle_turbo::{epub, kf8, passthrough, reader};

fn convert(spec: &EpubSpec, compress: bool) -> (kf8::Output, reader::Azw3) {
    let bytes = build_epub(spec);
    let book = epub::open(&bytes).expect("epub opens");
    let out = kf8::convert(&book, &kf8::Options { cover: None, compress, fetch_cover: false }).expect("convert");
    let azw = reader::read(&out.data).expect("output decodes");
    (out, azw)
}

fn novel(chapters: usize, paragraphs: usize) -> EpubSpec {
    let mut spec = EpubSpec::default();
    for i in 0..chapters {
        spec.chapters.push(Chapter {
            file: format!("Text/ch{i:03}.xhtml"),
            title: format!("Capítulo {i}"),
            body: format!("<h1 id=\"top\">Capítulo {i}</h1>\n{}", prose(&format!("c{i}"), paragraphs)),
        });
        spec.toc.push((format!("Capítulo {i}"), format!("Text/ch{i:03}.xhtml"), vec![]));
    }
    spec
}

#[test]
fn text_survives_roundtrip_in_order() {
    let spec = novel(12, 40);
    for compress in [true, false] {
        let (_, azw) = convert(&spec, compress);
        assert_eq!(azw.files.len(), 12);
        let text = azw.plain_text();
        let mut last = 0;
        for c in 0..12 {
            for p in [0, 17, 39] {
                let needle = format!("Frase única c{c} número {p}:");
                let pos = text.find(&needle).unwrap_or_else(|| panic!("missing {needle}"));
                assert!(pos >= last, "out of order: {needle}");
                last = pos;
            }
        }
        assert!(text.contains("Klein abriu os olhos & viu o nevoeiro — ação, coração, 诡秘之主, 괴담."));
    }
}

#[test]
fn rebuilt_files_are_wellformed_xhtml() {
    let (_, azw) = convert(&novel(5, 120), true);
    for f in &azw.files {
        let s = String::from_utf8(f.clone()).expect("utf-8");
        assert!(s.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\""));
        assert!(s.trim_end().ends_with("</html>"));
        assert_eq!(s.matches("<body").count(), 1);
        assert_eq!(s.matches("</body>").count(), 1);
        // every opened <p> is closed
        assert_eq!(s.matches("<p ").count() + s.matches("<p>").count(), s.matches("</p>").count());
        // every aid-able element received an aid
        assert!(s.contains(" aid=\""));
    }
}

#[test]
fn compression_shrinks_text_and_matches_uncompressed() {
    let spec = novel(8, 80);
    let (big, raw) = convert(&spec, false);
    let (small, packed) = convert(&spec, true);
    assert_eq!(raw.text, packed.text, "decompressed text must be identical");
    assert!(small.data.len() * 10 < big.data.len() * 7, "palmdoc should save >30%: {} vs {}", small.data.len(), big.data.len());
}

#[test]
fn huge_paragraph_is_split_into_big_text_blocks() {
    let mut spec = EpubSpec::default();
    let long = "palavra ".repeat(6000); // ~48 KB of text in one <p>
    spec.chapters.push(Chapter { file: "a.xhtml".into(), title: "A".into(), body: format!("<div><p>{long}</p></div>") });
    spec.toc.push(("A".into(), "a.xhtml".into(), vec![]));
    let (_, azw) = convert(&spec, true);
    let s = String::from_utf8_lossy(&azw.files[0]);
    assert!(s.contains("AmznBigTextBlock"), "large text must be chunked");
    assert_eq!(azw.plain_text().matches("palavra").count(), 6000);
}

#[test]
fn nested_toc_depths_and_labels() {
    let mut spec = novel(4, 30);
    spec.toc = vec![
        (
            "Parte 1".into(),
            "Text/ch000.xhtml".into(),
            vec![("Cap 1".into(), "Text/ch000.xhtml#c0-p10".into()), ("Cap 2".into(), "Text/ch001.xhtml".into())],
        ),
        (
            "Parte 2".into(),
            "Text/ch002.xhtml".into(),
            vec![("Cap 3".into(), "Text/ch002.xhtml#c2-p5".into()), ("Cap 4".into(), "Text/ch003.xhtml".into())],
        ),
    ];
    let (_, azw) = convert(&spec, true);
    let labels: Vec<_> = azw.toc.iter().map(|(d, l, _)| (*d, l.as_str())).collect();
    assert_eq!(labels, vec![(0, "Parte 1"), (0, "Parte 2"), (1, "Cap 1"), (1, "Cap 2"), (1, "Cap 3"), (1, "Cap 4")]);
    // offsets point at the right paragraph
    let (_, _, off) = azw.toc.iter().find(|t| t.1 == "Cap 3").unwrap();
    let around = String::from_utf8_lossy(&azw.text[*off..*off + 200]).into_owned();
    assert!(around.contains("c2-p5"), "Cap 3 offset lands on its anchor: {around}");
    // offsets ascend within the same depth
    for d in 0..=1 {
        let offs: Vec<usize> = azw.toc.iter().filter(|t| t.0 == d).map(|t| t.2).collect();
        assert!(offs.windows(2).all(|w| w[0] <= w[1]));
    }
}

#[test]
fn epub3_nav_toc_is_used() {
    let mut spec = novel(3, 10);
    spec.epub3_nav = true;
    let (_, azw) = convert(&spec, true);
    assert_eq!(azw.toc.len(), 3);
    assert_eq!(azw.toc[2].1, "Capítulo 2");
}

#[test]
fn internal_links_are_resolved() {
    let mut spec = novel(3, 20);
    spec.chapters[0]
        .body
        .push_str(r##"<p><a href="ch002.xhtml#c2-p7">ir</a> <a href="#c0-p3">aqui</a> <a href="https://example.com">fora</a></p>"##);
    let (_, azw) = convert(&spec, true);
    let s = String::from_utf8_lossy(&azw.files[0]).into_owned();
    // placeholders were kindle:pos:fid:0000:off:000000000{1,2}; a resolved link may
    // legitimately start with fid 0000 (first chunk), so compare the offsets
    for n in 1..=2 {
        assert!(!s.contains(&format!("kindle:pos:fid:0000:off:000000000{n}\"")), "placeholder {n} left unresolved");
    }
    assert_eq!(s.matches("kindle:pos:fid:").count(), 2);
    assert!(s.contains("href=\"https://example.com\""));
}

#[test]
fn metadata_and_exth() {
    let (out, azw) = convert(&novel(2, 5), true);
    assert_eq!(azw.title, "Livro de Teste");
    assert_eq!(azw.exth_str(100).as_deref(), Some("Autora Teste"));
    assert_eq!(azw.exth_str(503).as_deref(), Some("Livro de Teste"));
    assert_eq!(azw.exth_str(524).as_deref(), Some("pt"));
    assert_eq!(azw.exth_str(501).as_deref(), Some("EBOK"));
    assert_eq!(azw.exth_str(113).as_deref(), Some("123e4567-e89b-12d3-a456-426614174000"));
    assert_eq!(out.asin, "123e4567-e89b-12d3-a456-426614174000");
    assert_eq!(azw.exth_str(106).as_deref(), Some("2026-01-02"));
}

#[test]
fn images_cover_and_thumbnail() {
    let mut spec = novel(2, 5);
    let cover = encode(&picture(600, 900, 1), ImageFormat::Png);
    let webp = encode(&picture(2000, 3000, 9), ImageFormat::WebP);
    let jpg = encode(&picture(300, 200, 4), ImageFormat::Jpeg);
    spec.images = vec![
        ("Images/cover.png".into(), cover, "image/png".into()),
        ("Images/big.webp".into(), webp, "image/webp".into()),
        ("Images/small.jpg".into(), jpg, "image/jpeg".into()),
    ];
    spec.cover = Some("Images/cover.png".into());
    spec.chapters[0].body.push_str(r#"<p><img src="../Images/big.webp" alt=""/><img src="../Images/small.jpg"/></p>"#);
    let (out, azw) = convert(&spec, true);

    let cover_idx = azw.exth_u32(201).expect("cover offset") as usize;
    let thumb_idx = azw.exth_u32(202).expect("thumb offset") as usize;
    let c = image::load_from_memory(azw.resource(cover_idx).unwrap()).unwrap();
    assert_eq!((c.width(), c.height()), (600, 900));
    let t = image::load_from_memory(azw.resource(thumb_idx).unwrap()).unwrap();
    assert!(t.width() <= 180 && t.height() <= 240);
    assert!(out.library_thumb.is_some());

    // webp became a screen-sized jpeg, referenced from the text
    let s = String::from_utf8_lossy(&azw.files[0]).into_owned();
    assert_eq!(s.matches("kindle:embed:").count(), 2);
    let big_ref = s.split("kindle:embed:").nth(1).unwrap()[..4].to_string();
    let idx = usize::from_str_radix(&big_ref, 32).unwrap() - 1;
    let big = azw.resource(idx).unwrap();
    assert!(big.starts_with(&[0xFF, 0xD8]), "webp converted to jpeg");
    let b = image::load_from_memory(big).unwrap();
    assert!(b.width() <= 1264 && b.height() <= 1680);
    // jpeg kept losslessly but gained JFIF
    let small_ref = s.split("kindle:embed:").nth(2).unwrap()[..4].to_string();
    let sm = azw.resource(usize::from_str_radix(&small_ref, 32).unwrap() - 1).unwrap();
    assert_eq!(&sm[6..11], b"JFIF\0");
}

#[test]
fn missing_cover_is_generated() {
    let (out, azw) = convert(&novel(1, 3), true);
    let idx = azw.exth_u32(201).expect("generated cover") as usize;
    let img = image::load_from_memory(azw.resource(idx).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (600, 900));
    assert!(out.warnings.iter().any(|w| w.contains("capa gerada")));
}

#[test]
fn cover_page_image_is_detected() {
    let mut spec = novel(2, 3);
    spec.images = vec![("Images/c.jpg".into(), encode(&picture(500, 700, 3), ImageFormat::Jpeg), "image/jpeg".into())];
    spec.chapters.insert(
        0,
        Chapter {
            file: "Text/cover.xhtml".into(),
            title: "Capa".into(),
            body: r#"<div><img src="../Images/c.jpg" alt="capa"/></div>"#.into(),
        },
    );
    let (out, azw) = convert(&spec, true);
    let idx = azw.exth_u32(201).expect("cover from cover page") as usize;
    let img = image::load_from_memory(azw.resource(idx).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (500, 700));
    assert!(!out.warnings.iter().any(|w| w.contains("capa gerada")));
}

#[test]
fn custom_cover_overrides() {
    let mut spec = novel(1, 2);
    spec.images = vec![("Images/c.jpg".into(), encode(&picture(500, 700, 3), ImageFormat::Jpeg), "image/jpeg".into())];
    spec.cover = Some("Images/c.jpg".into());
    let book = epub::open(&build_epub(&spec)).unwrap();
    let custom = encode(&picture(640, 960, 77), ImageFormat::Png);
    let out = kf8::convert(&book, &kf8::Options { cover: Some(custom), compress: true, fetch_cover: false }).unwrap();
    let azw = reader::read(&out.data).unwrap();
    let img = image::load_from_memory(azw.resource(azw.exth_u32(201).unwrap() as usize).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (640, 960));
}

#[test]
fn inline_styles_become_flows_and_are_deduplicated() {
    let mut spec = novel(3, 2);
    for c in &mut spec.chapters {
        c.body = format!("<style>.k{{color:blue}}</style>{}", c.body);
    }
    let (_, azw) = convert(&spec, true);
    // text + style.css + one deduplicated inline style
    assert_eq!(azw.flows.len(), 3);
    let css = String::from_utf8_lossy(&azw.text[azw.flows[2].0..azw.flows[2].1]).into_owned();
    assert_eq!(css, ".k{color:blue}");
    for f in &azw.files {
        assert!(String::from_utf8_lossy(f).contains("kindle:flow:0002?mime=text/css"));
    }
}

#[test]
fn passthrough_reads_back_our_output() {
    let mut spec = novel(1, 2);
    spec.images = vec![("Images/c.png".into(), encode(&picture(400, 600, 5), ImageFormat::Png), "image/png".into())];
    spec.cover = Some("Images/c.png".into());
    let (out, _) = convert(&spec, true);
    let info = passthrough::read(&out.data).unwrap();
    assert_eq!(info.title, "Livro de Teste");
    assert_eq!(info.asin.as_deref(), Some(out.asin.as_str()));
    assert_eq!(info.cdetype, "EBOK");
    let th = image::load_from_memory(&info.thumb.expect("thumb")).unwrap();
    assert!(th.width() <= 330 && th.height() <= 470);
}

#[test]
fn many_chapters_scale() {
    // 1500 small chapters, like a long web novel
    let (_, azw) = convert(&novel(1500, 3), true);
    assert_eq!(azw.files.len(), 1500);
    assert_eq!(azw.toc.len(), 1500);
    assert!(azw.plain_text().contains("Frase única c1499 número 2:"));
}

#[test]
fn malformed_html_is_tolerated() {
    let mut spec = EpubSpec::default();
    spec.chapters.push(Chapter {
        file: "x.xhtml".into(),
        title: "X".into(),
        body: "<p>sem fechar <b>negrito <i>itálico</p><p>R&D &nbsp; &copy; &#233; &#x4E2D;</p><br><hr>".into(),
    });
    let (_, azw) = convert(&spec, true);
    let t = azw.plain_text();
    assert!(t.contains("sem fechar"));
    assert!(t.contains("R&D"));
    assert!(t.contains('©') && t.contains('é') && t.contains('中'));
}

#[test]
fn not_an_epub_is_an_error() {
    assert!(epub::open(b"definitely not a zip").is_err());
    assert!(reader::read(b"nope").is_err());
}

#[test]
fn png_is_optimized_losslessly() {
    let mut spec = novel(1, 2);
    // noisy-free flat PNG written without compression effort: big, very compressible
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(800, 600, |x, _| {
        image::Rgb([u8::try_from(x / 4 % 256).unwrap(), 10, 200])
    }));
    let mut raw = std::io::Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new_with_quality(
        &mut raw,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(img.as_bytes(), 800, 600, image::ExtendedColorType::Rgb8)
    .unwrap();
    let raw = raw.into_inner();
    spec.images = vec![("Images/p.png".into(), raw.clone(), "image/png".into())];
    spec.chapters[0].body.push_str(r#"<p><img src="../Images/p.png"/></p>"#);
    let (_, azw) = convert(&spec, true);
    let s = String::from_utf8_lossy(&azw.files[0]).into_owned();
    let r = &s.split("kindle:embed:").nth(1).unwrap()[..4];
    let out = azw.resource(usize::from_str_radix(r, 32).unwrap() - 1).unwrap();
    assert!(out.len() < raw.len(), "png not optimized: {} >= {}", out.len(), raw.len());
    let a = image::load_from_memory(&raw).unwrap().to_rgb8();
    let b = image::load_from_memory(out).unwrap().to_rgb8();
    assert_eq!(a, b, "optimization must be lossless");
}
