//! EPUB reading: container, OPF, NCX / nav TOC. All zip entries are inflated in parallel.

use crate::dom::{Doc, NIL};
use anyhow::{Context, Result, anyhow};
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::{Cursor, Read};

#[derive(Debug, Clone)]
pub struct ManItem {
    pub id: String,
    pub href: String, // full path inside the zip
    pub media_type: String,
    pub properties: String,
}

#[derive(Debug, Clone)]
pub struct TocNode {
    pub title: String,
    pub href: String, // full path, may contain #frag
    pub children: Vec<Self>,
}

#[derive(Debug, Clone)]
pub struct GuideRef {
    pub typ: String,
    pub title: String,
    pub href: String,
}

#[derive(Debug, Default)]
pub struct Meta {
    pub title: String,
    pub authors: Vec<String>,
    pub language: String,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub subjects: Vec<String>,
    pub date: Option<String>,
    pub uuid: Option<String>,
    pub isbn: Option<String>,
}

pub struct Book {
    pub meta: Meta,
    pub manifest: Vec<ManItem>,
    pub spine: Vec<String>,
    pub cover: Option<String>,
    pub toc: Vec<TocNode>,
    pub guide: Vec<GuideRef>,
    pub files: HashMap<String, Vec<u8>>,
    pub warnings: Vec<String>,
}

#[must_use]
pub fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[..i],
        None => "",
    }
}

/// Resolve `rel` (an href, possibly percent-encoded, without fragment) against directory `base`.
#[must_use]
pub fn resolve(base: &str, rel: &str) -> String {
    let rel = percent_encoding::percent_decode_str(rel).decode_utf8_lossy();
    let mut parts: Vec<&str> = if rel.starts_with('/') || base.is_empty() {
        Vec::new()
    } else {
        base.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Split an href into (resolved path, fragment).
#[must_use]
pub fn resolve_href(base: &str, href: &str) -> (String, String) {
    let (p, f) = match href.find('#') {
        Some(i) => (&href[..i], &href[i + 1..]),
        None => (href, ""),
    };
    let frag = percent_encoding::percent_decode_str(f).decode_utf8_lossy().into_owned();
    if p.is_empty() {
        return (String::new(), frag);
    }
    (resolve(base, p), frag)
}

#[must_use]
pub fn is_external(href: &str) -> bool {
    let h = href.trim_start();
    h.starts_with("http:") || h.starts_with("https:") || h.starts_with("mailto:") || h.starts_with("data:") || h.starts_with("kindle:")
}

fn read_all_entries(bytes: &[u8]) -> Result<(HashMap<String, Vec<u8>>, Vec<String>)> {
    let archive = zip::ZipArchive::new(Cursor::new(bytes)).context("not a zip/epub")?;
    let n = archive.len();
    let results: Vec<Result<(String, Vec<u8>), String>> = (0..n)
        .into_par_iter()
        .map_init(
            || archive.clone(),
            |a, i| {
                let mut f = a.by_index(i).map_err(|e| format!("entry {i}: {e}"))?;
                let name = f.name().map(|n| n.to_string()).unwrap_or_default();
                let mut buf = Vec::with_capacity(f.size() as usize);
                f.read_to_end(&mut buf).map_err(|e| format!("{name}: {e}"))?;
                Ok((name, buf))
            },
        )
        .collect();
    let mut files = HashMap::with_capacity(n);
    let mut warnings = Vec::new();
    for r in results {
        match r {
            Ok((k, v)) => {
                if !k.ends_with('/') {
                    files.insert(k, v);
                }
            }
            Err(e) => warnings.push(format!("entrada corrompida ignorada: {e}")),
        }
    }
    Ok((files, warnings))
}

fn children_named(d: &Doc, id: usize, name: &str) -> Vec<usize> {
    d.nodes[id].children.iter().copied().filter(|&c| d.nodes[c].name == name).collect()
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn open(bytes: &[u8]) -> Result<Book> {
    let (files, mut warnings) = read_all_entries(bytes)?;

    let container = files.get("META-INF/container.xml").ok_or_else(|| anyhow!("missing META-INF/container.xml"))?;
    let cdoc = Doc::parse(container);
    let opf_path = cdoc
        .descendants(cdoc.root)
        .into_iter()
        .find_map(|n| (cdoc.nodes[n].name == "rootfile").then(|| cdoc.attr(n, "full-path").map(str::to_owned)).flatten())
        .ok_or_else(|| anyhow!("no rootfile in container.xml"))?;
    let opf_path = resolve("", &opf_path);
    let opf = Doc::parse(files.get(&opf_path).ok_or_else(|| anyhow!("missing OPF {opf_path}"))?);
    let base = dirname(&opf_path).to_owned();

    let mut meta = Meta::default();
    let mut cover_id: Option<String> = None;
    let mut manifest = Vec::new();
    let mut spine_ids = Vec::new();
    let mut toc_id: Option<String> = None;
    let mut guide = Vec::new();

    for n in opf.descendants(opf.root) {
        let name = opf.nodes[n].name.as_str();
        let parent = opf.nodes[n].parent;
        let pname = if parent == NIL { "" } else { opf.nodes[parent].name.as_str() };
        match name {
            "title" if pname == "metadata" && meta.title.is_empty() => meta.title = clean(&opf.text_content(n)),
            "creator" if pname == "metadata" => {
                let a = clean(&opf.text_content(n));
                if !a.is_empty() {
                    meta.authors.push(a);
                }
            }
            "language" if pname == "metadata" && meta.language.is_empty() => meta.language = clean(&opf.text_content(n)),
            "publisher" if pname == "metadata" => meta.publisher = Some(clean(&opf.text_content(n))),
            "description" if pname == "metadata" => meta.description = Some(opf.text_content(n).trim().to_owned()),
            "subject" if pname == "metadata" => meta.subjects.push(clean(&opf.text_content(n))),
            "date" if pname == "metadata" && meta.date.is_none() => meta.date = Some(clean(&opf.text_content(n))),
            "identifier" if pname == "metadata" => {
                let v = clean(&opf.text_content(n));
                let scheme = opf.attr(n, "scheme").unwrap_or("").to_lowercase();
                let lv = v.to_lowercase();
                if lv.starts_with("urn:uuid:") || scheme == "uuid" {
                    meta.uuid.get_or_insert_with(|| v.rsplit(':').next().unwrap_or(&v).to_owned());
                } else if lv.starts_with("urn:isbn:") {
                    meta.isbn = Some(v[9..].to_string());
                } else if scheme == "isbn" {
                    meta.isbn = Some(v);
                }
            }
            "meta" if opf.attr(n, "name") == Some("cover") => cover_id = opf.attr(n, "content").map(str::to_owned),
            "item" if pname == "manifest" => {
                let href = opf.attr(n, "href").unwrap_or("");
                manifest.push(ManItem {
                    id: opf.attr(n, "id").unwrap_or("").to_owned(),
                    href: resolve_href(&base, href).0,
                    media_type: opf.attr(n, "media-type").unwrap_or("").to_lowercase(),
                    properties: opf.attr(n, "properties").unwrap_or("").to_owned(),
                });
            }
            "spine" => toc_id = opf.attr(n, "toc").map(str::to_owned),
            "itemref" => {
                if let Some(id) = opf.attr(n, "idref") {
                    spine_ids.push(id.to_owned());
                }
            }
            "reference" if pname == "guide" => {
                let (p, f) = resolve_href(&base, opf.attr(n, "href").unwrap_or(""));
                guide.push(GuideRef {
                    typ: opf.attr(n, "type").unwrap_or("").to_owned(),
                    title: opf.attr(n, "title").unwrap_or("").to_owned(),
                    href: if f.is_empty() { p } else { format!("{p}#{f}") },
                });
            }
            _ => {}
        }
    }
    if meta.title.is_empty() {
        meta.title = "Unknown".into();
    }
    if meta.language.is_empty() {
        meta.language = "en".into();
    }

    let by_id: HashMap<&str, &ManItem> = manifest.iter().map(|m| (m.id.as_str(), m)).collect();
    let spine: Vec<String> = spine_ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()))
        .filter(|m| files.contains_key(&m.href))
        .map(|m| m.href.clone())
        .collect();
    if spine.is_empty() {
        return Err(anyhow!("empty spine"));
    }

    let mut cover = cover_id.as_deref().and_then(|id| by_id.get(id)).filter(|m| m.media_type.starts_with("image/")).map(|m| m.href.clone());
    if cover.is_none() {
        cover = manifest.iter().find(|m| m.properties.split_whitespace().any(|p| p == "cover-image")).map(|m| m.href.clone());
    }
    let valid_image = |h: &str| files.get(h).is_some_and(|d| image::guess_format(d).is_ok());
    if cover.as_deref().is_some_and(|c| !valid_image(c)) {
        warnings.push("capa declarada no EPUB não é uma imagem válida".into());
        cover = None;
    }
    if cover.is_none() {
        // cover page: guide type=cover, or first spine page; use its single image
        let mut pages: Vec<String> = guide.iter().filter(|g| g.typ.eq_ignore_ascii_case("cover")).map(|g| g.href.split('#').next().unwrap_or("").to_owned()).collect();
        pages.extend(spine.first().cloned());
        for p in pages {
            let Some(data) = files.get(&p) else { continue };
            let d = Doc::parse(data);
            let imgs: Vec<String> = d
                .descendants(d.root)
                .into_iter()
                .filter(|&n| matches!(d.nodes[n].name.as_str(), "img" | "image"))
                .filter_map(|n| d.attr(n, "src").or(d.attr(n, "href")).map(|s| resolve_href(dirname(&p), s).0))
                .collect();
            let text_len = d.text_content(d.root).trim().chars().count();
            if imgs.len() == 1 && text_len < 300 && valid_image(&imgs[0]) {
                cover = Some(imgs[0].clone());
                break;
            }
        }
    }

    // TOC: prefer NCX, then EPUB3 nav
    let mut toc = Vec::new();
    let ncx = toc_id
        .as_deref()
        .and_then(|id| by_id.get(id))
        .map(|m| m.href.clone())
        .or_else(|| manifest.iter().find(|m| m.media_type == "application/x-dtbncx+xml").map(|m| m.href.clone()));
    if let Some(p) = ncx.as_ref().filter(|p| files.contains_key(*p)) {
        let d = Doc::parse(&files[p]);
        if let Some(nm) = d.find_first(d.root, "navMap") {
            toc = parse_ncx_points(&d, nm, dirname(p));
        }
    }
    if toc.is_empty()
        && let Some(m) = manifest.iter().find(|m| m.properties.split_whitespace().any(|p| p == "nav"))
            && let Some(data) = files.get(&m.href) {
                let d = Doc::parse(data);
                let navs: Vec<usize> = d.descendants(d.root).into_iter().filter(|&n| d.nodes[n].name == "nav").collect();
                let nav = navs.iter().copied().find(|&n| d.attr(n, "type") == Some("toc")).or(navs.first().copied());
                if let Some(nav) = nav
                    && let Some(ol) = d.find_first(nav, "ol") {
                        toc = parse_nav_ol(&d, ol, dirname(&m.href));
                    }
            }
    if toc.is_empty() {
        warnings.push("livro sem sumário (TOC)".into());
    }

    Ok(Book { meta, manifest, spine, cover, toc, guide, files, warnings })
}

fn parse_ncx_points(d: &Doc, parent: usize, base: &str) -> Vec<TocNode> {
    let mut out = Vec::new();
    for np in children_named(d, parent, "navPoint") {
        let title = d
            .find_first(np, "navLabel")
            .map(|l| clean(&d.text_content(l)))
            .unwrap_or_default();
        let href = children_named(d, np, "content")
            .first()
            .and_then(|&c| d.attr(c, "src"))
            .map(|s| join_href(base, s))
            .unwrap_or_default();
        let children = parse_ncx_points(d, np, base);
        out.push(TocNode { title, href, children });
    }
    out
}

fn parse_nav_ol(d: &Doc, ol: usize, base: &str) -> Vec<TocNode> {
    let mut out = Vec::new();
    for li in children_named(d, ol, "li") {
        let mut title = String::new();
        let mut href = String::new();
        let mut children = Vec::new();
        for &c in &d.nodes[li].children {
            match d.nodes[c].name.as_str() {
                "a" | "span" if title.is_empty() => {
                    title = clean(&d.text_content(c));
                    if let Some(h) = d.attr(c, "href") {
                        href = join_href(base, h);
                    }
                }
                "ol" => children = parse_nav_ol(d, c, base),
                _ => {}
            }
        }
        out.push(TocNode { title, href, children });
    }
    out
}

fn join_href(base: &str, href: &str) -> String {
    let (p, f) = resolve_href(base, href);
    if f.is_empty() { p } else { format!("{p}#{f}") }
}
