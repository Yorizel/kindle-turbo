//! Manhwa / webtoon tests with real images on disk.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{encode, picture, webtoon_strip};
use image::ImageFormat;
use kindle_turbo::{comic, kf8, reader};
use std::io::Write;

fn to_azw(book: &kindle_turbo::epub::Book) -> reader::Azw3 {
    let out = kf8::convert(book, &kf8::Options { cover: None, compress: true, fetch_cover: false }).unwrap();
    reader::read(&out.data).unwrap()
}

fn page_images(azw: &reader::Azw3) -> Vec<image::DynamicImage> {
    // every page file references exactly one image
    azw.files
        .iter()
        .map(|f| {
            let s = String::from_utf8_lossy(f);
            let r = &s.split("kindle:embed:").nth(1).expect("page has image")[..4];
            let idx = usize::from_str_radix(r, 32).unwrap() - 1;
            image::load_from_memory(azw.resource(idx).unwrap()).unwrap()
        })
        .collect()
}

#[test]
fn chapters_in_natural_order_and_pages_fit_screen() {
    let dir = tempfile::tempdir().unwrap();
    for ch in ["Cap 10", "Cap 2", "Cap 1"] {
        std::fs::create_dir(dir.path().join(ch)).unwrap();
        for i in 1..=3 {
            let img = webtoon_strip(800, 3200, &[(100, 1000), (1300, 2200), (2500, 3100)]);
            std::fs::write(dir.path().join(ch).join(format!("{i}.webp")), encode(&img, ImageFormat::WebP)).unwrap();
        }
    }
    let book = comic::open(dir.path(), Some("Teste")).unwrap();
    let azw = to_azw(&book);
    let labels: Vec<&str> = azw.toc.iter().map(|t| t.1.as_str()).collect();
    assert_eq!(labels, ["Cap 1", "Cap 2", "Cap 10"]);
    let pages = page_images(&azw);
    assert!(pages.len() >= 9, "strips must be cut into several pages, got {}", pages.len());
    for p in &pages {
        // 800 px strips are never upscaled; page keeps the screen aspect ratio
        assert_eq!(p.width(), 800);
        assert!(p.height() <= 800 * comic::PAGE_H / comic::PAGE_W);
    }
    assert_eq!(azw.title, "Teste");
}

#[test]
fn cuts_land_on_white_gutters() {
    let dir = tempfile::tempdir().unwrap();
    // scaled 800 -> 1236 (x1.545): panels never straddle a cut if cuts land in gutters
    let panels = [(50, 900), (1000, 1900), (2000, 2900)];
    let img = webtoon_strip(800, 3000, &panels);
    std::fs::write(dir.path().join("001.png"), encode(&img, ImageFormat::Png)).unwrap();
    let book = comic::open(dir.path(), None).unwrap();
    let pages = page_images(&to_azw(&book));
    for p in &pages {
        let g = p.to_luma8();
        // first and last rows of every page are (near) white => no panel was sliced
        let row_min = |y: u32| (0..g.width()).map(|x| g.get_pixel(x, y)[0]).min().unwrap();
        assert!(row_min(0) > 200 || row_min(g.height() - 1) > 200, "page edge cuts through a panel");
    }
}

#[test]
fn regular_pages_and_cbz() {
    let dir = tempfile::tempdir().unwrap();
    let cbz = dir.path().join("vol.cbz");
    let f = std::fs::File::create(&cbz).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for i in [3u8, 1, 2, 10] {
        z.start_file(format!("vol/p{i}.jpg"), opts).unwrap();
        z.write_all(&encode(&picture(1600, 2400, i), ImageFormat::Jpeg)).unwrap();
    }
    z.start_file("__MACOSX/._junk.jpg", opts).unwrap();
    z.write_all(b"junk").unwrap();
    z.finish().unwrap();
    let book = comic::open(&cbz, None).unwrap();
    assert_eq!(book.meta.title, "vol");
    let pages = page_images(&to_azw(&book));
    assert_eq!(pages.len(), 4, "manga pages are kept 1:1");
    for p in &pages {
        assert!(p.width() <= comic::PAGE_W && p.height() <= comic::PAGE_H);
    }
    // cover is the first page
    assert!(book.cover.is_some());
}

#[test]
fn empty_folder_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(comic::open(dir.path(), None).is_err());
}

#[test]
fn cbz_for_koreader_keeps_order_and_screen_size() {
    let dir = tempfile::tempdir().unwrap();
    for ch in ["Cap 2", "Cap 1"] {
        std::fs::create_dir(dir.path().join(ch)).unwrap();
        let img = webtoon_strip(800, 4000, &[(100, 1200), (1500, 2600), (2900, 3900)]);
        std::fs::write(dir.path().join(ch).join("01.png"), encode(&img, ImageFormat::Png)).unwrap();
    }
    let r = comic::render(dir.path(), Some("Meu Manhwa")).unwrap();
    let cbz = comic::to_cbz(&r).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(cbz)).unwrap();
    let names: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().unwrap().to_string()).collect();
    assert!(names[0].starts_with("001 Cap 1/"), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("002 Cap 2/")));
    assert_eq!(names.last().map(String::as_str), Some("ComicInfo.xml"));
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(sorted, names, "zip order must equal lexical order");
    for i in 0..z.len() - 1 {
        let mut f = z.by_index(i).unwrap();
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut buf).unwrap();
        let img = image::load_from_memory(&buf).unwrap();
        assert_eq!(img.width(), 800);
        assert!(img.height() <= 800 * comic::PAGE_H / comic::PAGE_W);
    }
}

#[test]
fn gutters_are_collapsed_so_pages_are_few() {
    let dir = tempfile::tempdir().unwrap();
    // two panels separated by a huge 3000 px white gap
    let img = webtoon_strip(800, 5000, &[(0, 900), (3900, 4900)]);
    std::fs::write(dir.path().join("1.png"), encode(&img, ImageFormat::Png)).unwrap();
    let r = comic::render(dir.path(), None).unwrap();
    let pages: usize = r.chapters.iter().map(|c| c.pages.len()).sum();
    assert!(pages <= 2, "white gap must not become pages: {pages}");
}

#[test]
fn pdf_has_one_page_per_image_and_chapter_bookmarks() {
    let dir = tempfile::tempdir().unwrap();
    for ch in ["Cap 10", "Cap 9"] {
        std::fs::create_dir(dir.path().join(ch)).unwrap();
        let img = webtoon_strip(720, 6000, &[(100, 1500), (1700, 3200), (3400, 5900)]);
        std::fs::write(dir.path().join(ch).join("001.img"), encode(&img, ImageFormat::WebP)).unwrap();
    }
    let r = comic::render(dir.path(), Some("Série Ç")).unwrap();
    let pdf = kindle_turbo::pdf::to_pdf(&r).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    let n_pages: usize = r.chapters.iter().map(|c| c.pages.len()).sum();
    assert_eq!(s.matches("/Type /Page ").count(), n_pages);
    assert!(s.contains(&format!("/Count {n_pages}")));
    assert!(s.contains("/Type /Outlines"));
    // chapter titles in natural order as UTF-16BE hex
    let hex = |t: &str| {
        t.encode_utf16().fold(String::new(), |mut acc, u| {
            std::fmt::Write::write_fmt(&mut acc, format_args!("{u:04X}")).unwrap();
            acc
        })
    };
    let p9 = s.find(&hex("Cap 9")).unwrap();
    let p10 = s.find(&hex("Cap 10")).unwrap();
    assert!(p9 < p10);
    assert!(s.ends_with("%%EOF\n"));
    // xref offsets point at real objects
    let xref = s.rfind("xref\n").unwrap();
    for line in s[xref..].lines().skip(3).take_while(|l| l.ends_with(" n ")) {
        let off: usize = line[..10].parse().unwrap();
        assert!(s[off..].starts_with(|c: char| c.is_ascii_digit()) && s[off..off + 20].contains(" 0 obj"));
    }
    if std::process::Command::new("pdfinfo").arg("-v").output().is_ok() {
        let f = dir.path().join("x.pdf");
        std::fs::write(&f, &pdf).unwrap();
        let out = std::process::Command::new("pdfinfo").arg(&f).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("Pages:           {n_pages}")));
    }
}
