//! Manhwa / webtoon / manga input: a folder of images or a .cbz/.zip.
//!
//! Optional sub-folders become chapters. Long strips are stitched and re-cut into
//! screen-sized pages, preferring cuts on blank rows so speech bubbles are not split.

use crate::epub::{Book, ManItem, Meta, TocNode};
use anyhow::{Context, Result, anyhow};
use image::{GrayImage, imageops::FilterType};
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

pub const PAGE_W: u32 = 1236;
pub const PAGE_H: u32 = 1648;
const JPEG_Q: u8 = 82;
const BATCH: usize = 48;
const CSS: &str = "@page{margin:0}body{margin:0;padding:0;text-align:center}img{width:100%;height:auto;margin:0;padding:0;display:block}div{page-break-after:always;margin:0;padding:0}";

/// One input image: its archive or relative path, and the raw bytes.
type Entry = (String, Vec<u8>);

fn is_image_name(n: &str) -> bool {
    let l = n.to_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".gif", ".bmp", ".avif"].iter().any(|e| l.ends_with(e))
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
    jpeg_encoder::Encoder::new(&mut buf, JPEG_Q)
        .encode(g.as_raw(), u16::try_from(g.width())?, u16::try_from(g.height())?, jpeg_encoder::ColorType::Luma)
        .map_err(|e| anyhow!("falha ao codificar JPEG: {e}"))?;
    Ok(buf)
}

/// Wraps `PAGE_W`-wide grayscale rows in an image; the height comes from the buffer length.
fn rows_to_gray(rows: Vec<u8>) -> Result<GrayImage> {
    let w = PAGE_W as usize;
    let h = u32::try_from(rows.len() / w)?;
    GrayImage::from_raw(PAGE_W, h, rows).ok_or_else(|| anyhow!("buffer de página inconsistente"))
}

/// A row is a good cut if it is (almost) a single flat colour.
fn row_is_flat(buf: &[u8], w: usize, row: usize) -> bool {
    let r = &buf[row * w..(row + 1) * w];
    let (mut lo, mut hi) = (255u8, 0u8);
    for &p in r.iter().step_by(3) {
        lo = lo.min(p);
        hi = hi.max(p);
        if hi - lo > 16 {
            return false;
        }
    }
    true
}

fn find_cut(buf: &[u8], w: usize, h: usize) -> usize {
    let target = PAGE_H as usize;
    if h <= target {
        return h;
    }
    let min = target / 3;
    // need a run of flat rows to avoid cutting through thin lines
    let mut row = target;
    while row > min {
        if row_is_flat(buf, w, row) && row_is_flat(buf, w, row - 1) && row_is_flat(buf, w, row - 2) {
            return row;
        }
        row -= 1;
    }
    target
}

fn is_blank(g: &GrayImage) -> bool {
    let w = g.width() as usize;
    (0..g.height() as usize).step_by(8).all(|r| row_is_flat(g.as_raw(), w, r))
}

struct Chapter {
    title: String,
    pages: Vec<Vec<u8>>,
}

/// Stream images of one chapter into screen pages.
fn render_chapter(images: &[&Entry]) -> Result<Vec<Vec<u8>>> {
    let mut pages: Vec<GrayImage> = Vec::new();
    let mut out: Vec<Vec<u8>> = Vec::new();
    let w = PAGE_W as usize;
    let mut carry: Vec<u8> = Vec::new(); // rows of width PAGE_W
    for batch in images.chunks(BATCH) {
        let decoded: Vec<Option<GrayImage>> = batch.par_iter().map(|(_, d)| decode_gray(d)).collect();
        for g in decoded.into_iter().flatten() {
            let strip = f64::from(g.height()) / f64::from(g.width()) > 1.9;
            if !strip {
                // regular page: flush pending strip first
                if !carry.is_empty() {
                    pages.push(rows_to_gray(std::mem::take(&mut carry))?);
                }
                pages.push(fit_page(g));
                continue;
            }
            let g = scale_to_width(g, PAGE_W);
            carry.extend_from_slice(g.as_raw());
            loop {
                let h = carry.len() / w;
                if h < PAGE_H as usize {
                    break;
                }
                let cut = find_cut(&carry, w, h);
                let rest = carry.split_off(cut * w);
                pages.push(rows_to_gray(std::mem::replace(&mut carry, rest))?);
            }
        }
        // encode what we have so far in parallel to bound memory
        let ready: Vec<GrayImage> = std::mem::take(&mut pages);
        let encoded: Vec<Vec<u8>> = ready.into_par_iter().filter(|p| !is_blank(p)).map(|p| encode_gray(&p)).collect::<Result<_>>()?;
        out.extend(encoded);
    }
    if carry.len() / w > 40 {
        let g = rows_to_gray(carry)?;
        if !is_blank(&g) {
            out.push(encode_gray(&g)?);
        }
    }
    Ok(out)
}

/// Reads a manhwa folder or CBZ and builds an EPUB book model with one image per screen page.
///
/// # Errors
///
/// Fails if the input cannot be read, holds no images, or produces no pages.
pub fn open(input: &Path, title: Option<&str>) -> Result<Book> {
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
    let chapters: Vec<Chapter> = chapters_raw
        .par_iter()
        .with_max_len(1)
        .enumerate()
        .map(|(i, (dir, imgs))| -> Result<Chapter> {
            let title = if multi {
                dir.rsplit('/').next().filter(|s| !s.is_empty()).map_or_else(|| format!("Capítulo {}", i + 1), str::to_owned)
            } else {
                "Início".to_owned()
            };
            Ok(Chapter { title, pages: render_chapter(imgs)? })
        })
        .collect::<Result<_>>()?;

    let name = title.map_or_else(|| input.file_stem().map_or_else(|| "Manhwa".into(), |s| s.to_string_lossy().into_owned()), str::to_owned);

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
