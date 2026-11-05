//! kindle-turbo: EPUB / manhwa to AZW3 (KF8) conversion, parallel on every core.
//!
//! The KF8 writer is a port of calibre's `writer8` (GPLv3).

#![forbid(unsafe_code)]

pub mod comic;
pub mod covergen;
pub mod dom;
pub mod epub;
pub mod fetchcover;
pub mod images;
pub mod kf8;
pub mod mobi;
pub mod palmdoc;
pub mod passthrough;
pub mod reader;
pub mod webtoon;
