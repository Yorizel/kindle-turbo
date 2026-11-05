//! Manhwa / webtoon / manga input: a folder of images or a .cbz/.zip.
//!
//! Optional sub-folders become chapters. Long strips are stitched and re-cut into
//! screen-sized pages, preferring cuts on blank rows so speech bubbles are not split.

use crate::dom::escape_text;
use crate::epub::{Book, ManItem, Meta, TocNode};
use crate::webtoon;
use anyhow::{Context, Result, anyhow};
use image::{GrayImage, imageops::FilterType};
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

pub const PAGE_W: u32 = 1236;
pub const PAGE_H: u32 = 1648;
/// e-ink shows 16 grey levels: q55 is indistinguishable there and keeps pages light
const JPEG_Q: u8 = 55;
const CHAPTERS_IN_FLIGHT: usize = 4;
const CSS: &str = "@page{margin:0}body{margin:0;padding:0;text-align:center}img{width:100%;height:auto;margin:0;padding:0;display:block}div{page-break-after:always;margin:0;padding:0}";

/// One input image: its archive or relative path, and the raw bytes.
type Entry = (String, Vec<u8>);

fn is_image_name(n: &str) -> bool {
    let l = n.to_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".gif", ".bmp", ".avif", ".img"].iter().any(|e| l.ends_with(e))
}

/// Natural ordering: "ch2" < "ch10".
fn natural_key(s: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut num = String::new();
    let mut txt = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !txt.is_empty() {
                out.push((1, 0, std::mem::take(&mut txt)));
            }
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((0, num.parse().unwrap_or(u64::MAX), String::new()));
                num.clear();
            }
            txt.push(c);
        }
    }
    if !num.is_empty() {
        out.push((0, num.parse().unwrap_or(u64::MAX), String::new()));
    }
    if !txt.is_empty() {
        out.push((1, 0, txt));
    }
    out
}

/// (relative path, bytes) of every image, naturally sorted.
fn collect(input: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut items: Vec<(String, Vec<u8>)> = if input.is_dir() {
        let mut paths = Vec::new();
        let mut stack = vec![input.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d)? {
                let p = e?.path();
                if p.is_dir() {
                    stack.push(p);
                } else if is_image_name(&p.to_string_lossy()) {
                    paths.push(p);
                }
            }
        }
        paths
            .par_iter()
            .map(|p| {
                let rel = p.strip_prefix(input).unwrap_or(p).to_string_lossy().replace('\\', "/");
                Ok((rel, std::fs::read(p)?))
            })
            .collect::<Result<_>>()?
    } else {
        let bytes = std::fs::read(input)?;
        let archive = zip::ZipArchive::new(Cursor::new(&bytes[..])).context("CBZ/ZIP inválido (CBR/RAR não é suportado)")?;
        (0..archive.len())
            .into_par_iter()
            .map_init(
                || archive.clone(),
                |a, i| -> Option<(String, Vec<u8>)> {
                    let mut f = a.by_index(i).ok()?;
                    let name = f.name().map(|n| n.to_string()).unwrap_or_default();
                    if !is_image_name(&name) || name.contains("__MACOSX") {
                        return None;
                    }
                    let mut v = Vec::new();
                    f.read_to_end(&mut v).ok()?;
                    Some((name, v))
                },
            )
            .flatten()
            .collect()
    };
    items.sort_by_cached_key(|(n, _)| n.split('/').map(natural_key).collect::<Vec<_>>());
    if items.is_empty() {
        return Err(anyhow!("nenhuma imagem encontrada em {}", input.display()));
    }
    Ok(items)
}

fn decode_gray(data: &[u8]) -> Option<GrayImage> {
    let img = image::load_from_memory(data).ok()?;
    let g = img.to_luma8();
    if g.width() == 0 || g.height() == 0 {
        return None;
    }
    Some(g)
}

fn scale_to_width(g: GrayImage, w: u32) -> GrayImage {
    if g.width() == w {
        return g;
    }
    let scaled_h = (u64::from(g.height()) * u64::from(w)) / u64::from(g.width());
    let h = u32::try_from(scaled_h.max(1)).unwrap_or(u32::MAX);
    image::imageops::resize(&g, w, h, FilterType::Triangle)
}

/// Shrinks a page to fit inside `PAGE_W` x `PAGE_H`, keeping the aspect ratio.
fn fit_page(g: GrayImage) -> GrayImage {
    let (w, h) = (u64::from(g.width()), u64::from(g.height()));
    let (max_w, max_h) = (u64::from(PAGE_W), u64::from(PAGE_H));
    if w <= max_w && h <= max_h {
        return g;
    }
    // scale by the tighter of the two bounds
    let (new_w, new_h) = if max_w * h < max_h * w { (max_w, (h * max_w / w).max(1)) } else { ((w * max_h / h).max(1), max_h) };
    let to_u32 = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    image::imageops::resize(&g, to_u32(new_w), to_u32(new_h), FilterType::Triangle)
}

fn encode_gray(g: &GrayImage) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut buf, JPEG_Q);
    enc.set_optimized_huffman_tables(true);
    enc.encode(g.as_raw(), u16::try_from(g.width())?, u16::try_from(g.height())?, jpeg_encoder::ColorType::Luma)
        .map_err(|e| anyhow!("falha ao codificar JPEG: {e}"))?;
    Ok(buf)
}

/// Wraps `width`-wide grayscale rows in an image; the height comes from the buffer length.
fn rows_to_gray(rows: Vec<u8>, width: u32) -> Result<GrayImage> {
    let w = usize::try_from(width)?.max(1);
    let h = u32::try_from(rows.len() / w)?;
    GrayImage::from_raw(width, h, rows).ok_or_else(|| anyhow!("buffer de página inconsistente"))
}

/// A chapter rendered into screen-sized grayscale JPEG pages.
#[derive(Debug)]
pub struct Chapter {
    /// Chapter name (folder name, or "Início" for a single folder).
    pub title: String,
    /// JPEG pages: strips keep their width (never upscaled past `PAGE_W`) with the
    /// screen aspect ratio; regular pages fit inside `PAGE_W` x `PAGE_H`.
    pub pages: Vec<Vec<u8>>,
}

/// A whole comic rendered for the Kindle screen.
#[derive(Debug)]
pub struct Rendered {
    /// Title (from `--title` or the input name).
    pub title: String,
    /// Chapters in natural order.
    pub chapters: Vec<Chapter>,
}

/// Image dimensions without decoding pixels.
fn dims(images: &[&Entry]) -> Vec<(u32, u32)> {
    images
        .par_iter()
        .filter_map(|(_, d)| image::ImageReader::new(Cursor::new(d)).with_guessed_format().ok()?.into_dimensions().ok())
        .collect()
}

/// Strip width: the most common image width, never upscaled past the screen.
fn strip_width(sizes: &[(u32, u32)]) -> u32 {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for &(w, _) in sizes {
        *counts.entry(w).or_default() += 1;
    }
    counts.into_iter().max_by_key(|&(w, c)| (c, w)).map_or(PAGE_W, |(w, _)| w).min(PAGE_W)
}

/// A chapter is a long strip when its images are tall (median height > 1.8 x width; manga pages are ~1.4-1.5).
fn is_strip(sizes: &[(u32, u32)]) -> bool {
    let mut ratios: Vec<u64> = sizes.iter().map(|&(w, h)| u64::from(h) * 100 / u64::from(w.max(1))).collect();
    ratios.sort_unstable();
    ratios.get(ratios.len() / 2).is_some_and(|&r| r > 180)
}

/// Manga / comic pages: one image per page, fitted to the screen.
fn render_pages(images: &[&Entry]) -> Result<Vec<Vec<u8>>> {
    images
        .par_iter()
        .filter_map(|(_, d)| decode_gray(d))
        .map(|g| {
            let mut g = fit_page(g);
            webtoon::autocontrast(&mut g);
            encode_gray(&g)
        })
        .collect()
}

/// Render one chapter: long strips are stitched into a single strip and laid
/// out panel by panel (see [`webtoon`]); regular pages are fitted one by one.
fn render_chapter(images: &[&Entry]) -> Result<Vec<Vec<u8>>> {
    let sizes = dims(images);
    if !is_strip(&sizes) {
        return render_pages(images);
    }
    let width = strip_width(&sizes);
    let w = usize::try_from(width)?;
    // decode, grey and scale every image in parallel, then stitch in order
    let parts: Vec<GrayImage> = images.par_iter().filter_map(|(_, d)| decode_gray(d).map(|g| scale_to_width(g, width))).collect();
    let mut strip = Vec::with_capacity(parts.iter().map(|g| g.as_raw().len()).sum());
    for g in parts {
        strip.extend_from_slice(g.as_raw());
    }
    webtoon::autocontrast(&mut strip);
    let params = webtoon::Params::for_width(w, PAGE_W as usize, PAGE_H as usize);
    let quiet = webtoon::quiet_rows(&strip, w, params.edge_threshold);
    let panels = webtoon::panels(&quiet, params.min_gutter);
    let pages = webtoon::layout(&panels, &params);
    let bg = webtoon::background(&strip, w);
    pages
        .par_iter()
        .map(|page| {
            let raw = webtoon::compose(&strip, page, &params, bg);
            encode_gray(&rows_to_gray(raw, width)?)
        })
        .collect()
}

/// Reads a manhwa folder or CBZ and builds an EPUB book model with one image per screen page.
///
/// # Errors
///
/// Fails if the input cannot be read, holds no images, or produces no pages.
pub fn open(input: &Path, title: Option<&str>) -> Result<Book> {
    to_book(render(input, title)?)
}

/// Decode, stitch and cut every image of `input` into screen pages.
///
/// # Errors
/// Fails when the input has no readable images.
pub fn render(input: &Path, title: Option<&str>) -> Result<Rendered> {
    let items = collect(input)?;
    // group by parent folder => chapters
    let mut chapters_raw: Vec<(String, Vec<&Entry>)> = Vec::new();
    for it in &items {
        let dir = it.0.rsplit_once('/').map(|(d, _)| d.to_owned()).unwrap_or_default();
        match chapters_raw.last_mut() {
            Some((d, v)) if *d == dir => v.push(it),
            _ => chapters_raw.push((dir, vec![it])),
        }
    }
    let multi = chapters_raw.len() > 1;
    // a stitched chapter can take a few hundred MB: render a few at a time,
    // each one already uses every core internally
    let mut chapters: Vec<Chapter> = Vec::with_capacity(chapters_raw.len());
    for (g, group) in chapters_raw.chunks(CHAPTERS_IN_FLIGHT).enumerate() {
        let done: Vec<Chapter> = group
            .par_iter()
            .enumerate()
            .map(|(k, (dir, imgs))| -> Result<Chapter> {
                let i = g * CHAPTERS_IN_FLIGHT + k;
                let title = if multi {
                    dir.rsplit('/').next().filter(|s| !s.is_empty()).map_or_else(|| format!("Capítulo {}", i + 1), str::to_owned)
                } else {
                    "Início".to_owned()
                };
                Ok(Chapter { title, pages: render_chapter(imgs)? })
            })
            .collect::<Result<_>>()?;
        chapters.extend(done);
    }

    let name = title.map_or_else(|| input.file_stem().map_or_else(|| "Manhwa".into(), |s| s.to_string_lossy().into_owned()), str::to_owned);
    if chapters.iter().all(|c| c.pages.is_empty()) {
        return Err(anyhow!("nenhuma página gerada"));
    }
    Ok(Rendered { title: name, chapters })
}

/// Package one chapter as a CBZ (stored: JPEGs are already compressed).
///
/// # Errors
/// Only fails on an internal zip write error.
pub fn chapter_cbz(series: &str, chapter: &Chapter) -> Result<Vec<u8>> {
    use std::io::Write as _;
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (n, p) in chapter.pages.iter().enumerate() {
        z.start_file(format!("{:04}.jpg", n + 1), opts)?;
        z.write_all(p)?;
    }
    z.start_file("ComicInfo.xml", opts)?;
    let mut series_x = Vec::new();
    escape_text(series, &mut series_x);
    let mut title_x = Vec::new();
    escape_text(&chapter.title, &mut title_x);
    z.write_all(b"<?xml version=\"1.0\"?><ComicInfo><Series>")?;
    z.write_all(&series_x)?;
    z.write_all(b"</Series><Title>")?;
    z.write_all(&title_x)?;
    write!(z, "</Title><PageCount>{}</PageCount><Manga>No</Manga></ComicInfo>", chapter.pages.len())?;
    Ok(z.finish()?.into_inner())
}

fn to_book(r: Rendered) -> Result<Book> {
    let Rendered { title: name, chapters } = r;
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    let mut manifest = Vec::new();
    let mut spine = Vec::new();
    let mut toc = Vec::new();
    files.insert("OEBPS/style.css".into(), CSS.as_bytes().to_vec());
    manifest.push(ManItem { id: "css".into(), href: "OEBPS/style.css".into(), media_type: "text/css".into(), properties: String::new() });
    let mut n = 0usize;
    let mut cover = None;
    for ch in chapters {
        let first = n;
        for p in ch.pages {
            let img = format!("OEBPS/img/{n:05}.jpg");
            let html = format!("OEBPS/p{n:05}.xhtml");
            let x = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{n}</title><link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/></head><body><div><img src=\"img/{n:05}.jpg\" alt=\"\"/></div></body></html>"
            );
            files.insert(img.clone(), p);
            files.insert(html.clone(), x.into_bytes());
            manifest.push(ManItem { id: format!("i{n}"), href: img.clone(), media_type: "image/jpeg".into(), properties: String::new() });
            manifest.push(ManItem {
                id: format!("p{n}"),
                href: html.clone(),
                media_type: "application/xhtml+xml".into(),
                properties: String::new(),
            });
            if cover.is_none() {
                cover = Some(img);
            }
            spine.push(html);
            n += 1;
        }
        if n > first {
            toc.push(TocNode { title: ch.title, href: format!("OEBPS/p{first:05}.xhtml"), children: vec![] });
        }
    }
    if spine.is_empty() {
        return Err(anyhow!("nenhuma página gerada"));
    }
    Ok(Book {
        meta: Meta { title: name, language: "en".into(), ..Default::default() },
        manifest,
        spine,
        cover,
        toc,
        guide: vec![],
        files,
        warnings: vec![],
    })
}
