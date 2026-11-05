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
fn chapter_cbz_pages_in_order_with_same_size() {
    let dir = tempfile::tempdir().unwrap();
    for ch in ["Cap 2", "Cap 1"] {
        std::fs::create_dir(dir.path().join(ch)).unwrap();
        let img = webtoon_strip(800, 4000, &[(100, 1200), (1500, 2600), (2900, 3900)]);
        std::fs::write(dir.path().join(ch).join("01.png"), encode(&img, ImageFormat::Png)).unwrap();
    }
    let r = comic::render(dir.path(), Some("Meu Manhwa")).unwrap();
    assert_eq!(r.chapters.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["Cap 1", "Cap 2"]);
    let cbz = comic::chapter_cbz(&r.title, &r.chapters[0]).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(cbz)).unwrap();
    let names: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().unwrap().to_string()).collect();
    assert_eq!(names.last().map(String::as_str), Some("ComicInfo.xml"));
    let mut sizes = std::collections::HashSet::new();
    for (i, name) in names.iter().enumerate().take(z.len() - 1) {
        assert_eq!(name, &format!("{:04}.jpg", i + 1));
        let mut f = z.by_index(i).unwrap();
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut buf).unwrap();
        let img = image::load_from_memory(&buf).unwrap();
        sizes.insert((img.width(), img.height()));
    }
    // every page has the same size: no zoom jumps between pages
    assert_eq!(sizes.len(), 1, "{sizes:?}");
    assert_eq!(sizes.into_iter().next().unwrap(), (800, 800 * comic::PAGE_H / comic::PAGE_W));
}

#[test]
fn gutters_are_collapsed_so_pages_are_few() {
    let dir = tempfile::tempdir().unwrap();
    // two panels separated by a huge 3000 px white gap
    let img = webtoon_strip(800, 5000, &[(0, 400), (4500, 4900)]);
    std::fs::write(dir.path().join("1.png"), encode(&img, ImageFormat::Png)).unwrap();
    let r = comic::render(dir.path(), None).unwrap();
    let pages: usize = r.chapters.iter().map(|c| c.pages.len()).sum();
    assert_eq!(pages, 1, "two short panels with a huge gap between them fit one page");
}

#[test]
fn series_name_drops_downloader_slug() {
    use std::path::Path;
    assert_eq!(
        comic::series_name(Path::new("/x/Return of the Disaster-Class Hero [return-of-the-disaster-class-hero]")),
        "Return of the Disaster-Class Hero"
    );
    assert_eq!(comic::series_name(Path::new("/x/Solo Leveling")), "Solo Leveling");
    assert_eq!(comic::series_name(Path::new("/x/[only-tag]")), "[only-tag]");
}

#[test]
fn chapter_archives_in_natural_order() {
    let dir = tempfile::tempdir().unwrap();
    for n in ["Capítulo 10.cbz", "Capítulo 2.cbz", "Capítulo 0.cbz", "notes.txt"] {
        std::fs::write(dir.path().join(n), b"x").unwrap();
    }
    std::fs::create_dir(dir.path().join("Capítulo 0.sdr")).unwrap();
    let names: Vec<String> =
        comic::chapter_archives(dir.path()).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["Capítulo 0.cbz", "Capítulo 2.cbz", "Capítulo 10.cbz"]);
}
