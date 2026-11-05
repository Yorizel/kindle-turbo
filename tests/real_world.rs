//! Tests against real services and real books. They need the network or local
//! files, so they are opt-in: `cargo test -- --ignored`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use kindle_turbo::{epub, fetchcover, kf8, reader};

/// Real `AniList` lookup for a Korean web novel without a cover in its EPUB.
#[test]
#[ignore = "needs network"]
fn anilist_finds_ghost_story_cover() {
    let (img, src) =
        fetchcover::find("Got Dropped into a Ghost Story, Still Gotta Work - Mythic Regressor", None, false).expect("cover found");
    assert_eq!(src, "AniList");
    let i = image::load_from_memory(&img).unwrap();
    assert!(i.height() > i.width());
}

/// Converts every EPUB in `$KT_BOOKS` (default ~/Downloads) and verifies it fully.
#[test]
#[ignore = "needs real books on disk"]
fn real_books_roundtrip() {
    let dir = std::env::var("KT_BOOKS").unwrap_or_else(|_| format!("{}/Downloads", std::env::var("HOME").unwrap()));
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "epub") {
            let book = epub::open(&std::fs::read(&p).unwrap()).unwrap();
            let out = kf8::convert(&book, &kf8::Options { cover: None, compress: true, fetch_cover: false }).unwrap();
            let azw = reader::read(&out.data).unwrap_or_else(|e| panic!("{}: {e:#}", p.display()));
            assert_eq!(azw.files.len(), book.spine.len());
            assert!(azw.exth_u32(201).is_some(), "{} has no cover", p.display());
            n += 1;
        }
    }
    assert!(n > 0);
}
