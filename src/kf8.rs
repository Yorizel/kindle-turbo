//! EPUB -> KF8 (AZW3) pipeline. Port of calibre's writer8, parallelised per file/record/image.

use crate::dom::{Doc, NIL, is_aid_able};
use crate::epub::{Book, TocNode, dirname, is_external, resolve, resolve_href};
use crate::images;
use crate::mobi::{self, IndexEntry, NcxEntry, END, NULL, RECORD_SIZE, tm, to_base, to_ref};
use crate::palmdoc;
use anyhow::Result;
use rayon::prelude::*;
use regex::bytes::Regex as BRegex;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const CHUNK_SIZE: usize = 8192;

pub struct Options {
    pub cover: Option<Vec<u8>>,
    pub compress: bool,
    pub fetch_cover: bool,
}

pub struct Output {
    pub data: Vec<u8>,
    pub asin: String,
    pub library_thumb: Option<Vec<u8>>,
    pub warnings: Vec<String>,
    pub title: String,
}

struct Resources {
    records: Vec<Vec<u8>>,
    item_map: HashMap<String, (usize, &'static str)>, // href -> (1-based index, mime)
    cover_offset: Option<u32>,
    thumbnail_offset: Option<u32>,
    library_thumb: Option<Vec<u8>>,
    always_used: HashSet<usize>, // 0-based record indices
}

fn build_resources(book: &Book, opts: &Options, warnings: &mut Vec<String>) -> Resources {
    let mut items: Vec<(String, &[u8])> = book
        .manifest
        .iter()
        .filter(|m| m.media_type.starts_with("image/") && m.media_type != "image/svg+xml")
        .filter_map(|m| book.files.get(&m.href).map(|d| (m.href.clone(), d.as_slice())))
        .collect();
    let mut cover_href = book.cover.clone();
    let generated = if opts.cover.is_none() && cover_href.is_none() {
        let author = book.meta.authors.iter().find(|a| {
            let l = a.trim().to_lowercase();
            !l.is_empty() && l != "<unknown>" && l != "unknown"
        });
        let fetched = if opts.fetch_cover { crate::fetchcover::find(&book.meta.title, author.map(String::as_str), false) } else { None };
        if let Some((d, src)) = fetched {
            warnings.push(format!("EPUB sem capa: capa baixada do {src}"));
            Some(d)
        } else {
            let g = crate::covergen::generate(&book.meta.title, author.map(String::as_str));
            if g.is_some() {
                warnings.push("EPUB sem capa: capa gerada com o título (use --cover para escolher outra)".into());
            }
            g
        }
    } else {
        None
    };
    if let Some(c) = opts.cover.as_ref().or(generated.as_ref()) {
        if let Some(h) = &cover_href {
            if let Some(slot) = items.iter_mut().find(|(k, _)| k == h) {
                slot.1 = c.as_slice();
            } else {
                items.push((h.clone(), c.as_slice()));
            }
        } else {
            cover_href = Some("__kindle_turbo_cover__".into());
            items.push(("__kindle_turbo_cover__".into(), c.as_slice()));
        }
    }
    let processed: Vec<(String, Result<(Vec<u8>, &'static str)>, Option<Vec<u8>>, Option<Vec<u8>>)> = items
        .par_iter()
        .map(|(href, data)| {
            let is_cover = cover_href.as_deref() == Some(href.as_str());
            let p = images::process(data);
            let (thumb, lib) = if is_cover {
                (images::thumbnail(data, 180, 240, 16 * 1024).ok(), images::thumbnail(data, 330, 470, 64 * 1024).ok())
            } else {
                (None, None)
            };
            (href.clone(), p, thumb, lib)
        })
        .collect();

    let mut r = Resources {
        records: Vec::new(),
        item_map: HashMap::new(),
        cover_offset: None,
        thumbnail_offset: None,
        library_thumb: None,
        always_used: HashSet::new(),
    };
    let mut index = 1usize;
    for (href, p, thumb, lib) in processed {
        match p {
            Err(e) => warnings.push(format!("imagem ignorada {href}: {e}")),
            Ok((data, mime)) => {
                r.records.push(data);
                r.item_map.insert(href.clone(), (index, mime));
                index += 1;
                if cover_href.as_deref() == Some(href.as_str()) {
                    let co = (index - 2) as u32;
                    r.cover_offset = Some(co);
                    r.always_used.insert(co as usize);
                    if let Some(t) = thumb {
                        r.records.push(t);
                        r.thumbnail_offset = Some((index - 1) as u32);
                        r.always_used.insert(index - 1);
                        index += 1;
                    }
                    r.library_thumb = lib;
                }
            }
        }
    }
    r
}

static CSS_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"url\(\s*(['"]?)([^'")]+)(['"]?)\s*\)"#).unwrap());
static CSS_IMPORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"@import\s+(?:url\(\s*)?['"]?([^'")\s;]+)['"]?\s*\)?"#).unwrap());

struct Ctx<'a> {
    res: &'a Resources,
    sheets: HashMap<String, usize>,
    svg_files: HashMap<String, usize>,
    spine_set: HashSet<&'a str>,
}

fn rewrite_css(css: &str, base: &str, ctx: &Ctx, used: &mut HashSet<usize>) -> String {
    let s = CSS_IMPORT.replace_all(css, |c: &regex::Captures| {
        let target = resolve(base, &c[1]);
        match ctx.sheets.get(&target) {
            Some(&i) => format!("@import url(kindle:flow:{}?mime=text/css)", to_ref(i)),
            None => c[0].to_string(),
        }
    });
    CSS_URL
        .replace_all(&s, |c: &regex::Captures| {
            let raw = c[2].trim();
            if is_external(raw) {
                return c[0].to_string();
            }
            let target = resolve(base, raw.split('#').next().unwrap_or(raw));
            match ctx.res.item_map.get(&target) {
                Some(&(i, mime)) => {
                    used.insert(i - 1);
                    format!("url(kindle:embed:{}?mime={mime})", to_ref(i))
                }
                None => c[0].to_string(),
            }
        })
        .into_owned()
}

// ------------------------------------------------------------- phase 1: markup

struct FileState {
    doc: Doc,
    href: String,
    inline_styles: Vec<(usize, String)>, // link node, css
    svgs: Vec<(usize, Vec<u8>)>,         // img node, svg markup
    links: Vec<(u64, String, String)>,   // placeholder number, target file, frag
    ids: Vec<(String, String)>,          // id -> aid/cid
    body_aid: String,
    used: HashSet<usize>,
}

fn placeholder(n: u64) -> String {
    format!("kindle:pos:fid:0000:off:{}", to_base(n, 32, 10))
}

fn process_markup(file_idx: usize, href: &str, data: &[u8], ctx: &Ctx) -> FileState {
    let mut doc = Doc::parse(data);
    let base = dirname(href).to_owned();
    let root = doc.root;
    doc.nodes[root].name = "html".into();
    doc.nodes[root].attrs.retain(|(k, _)| k == "lang" || k == "class" || k == "style");
    doc.nodes[root].tail = Some("\n".into());

    // ensure <body>
    let body = if let Some(b) = doc.nodes[root].children.iter().copied().find(|&c| doc.nodes[c].name == "body") { b } else {
        let b = doc.create("body", vec![]);
        let kids: Vec<usize> = doc.nodes[root].children.clone();
        for k in kids {
            if doc.nodes[k].name != "head" {
                doc.remove(k);
                doc.append_child(b, k);
            }
        }
        if let Some(t) = doc.nodes[root].text.take() {
            doc.nodes[b].text = Some(t);
        }
        doc.append_child(root, b);
        b
    };
    doc.nodes[body].tail = Some("\n".into());

    let mut st = FileState {
        doc,
        href: href.to_owned(),
        inline_styles: Vec::new(),
        svgs: Vec::new(),
        links: Vec::new(),
        ids: Vec::new(),
        body_aid: String::new(),
        used: HashSet::new(),
    };
    let doc = &mut st.doc;

    for n in doc.descendants(root) {
        if doc.nodes[n].parent == NIL {
            continue;
        }
        doc.remove_attr(n, "aid");
        doc.remove_attr(n, "cid");
        let name = doc.nodes[n].name.clone();
        match name.as_str() {
            "script"
                if doc.nodes[n].text.as_deref().is_none_or(|t| t.trim().is_empty()) && doc.attr(n, "src").is_none() => {
                    doc.extract(n);
                }
            "img" | "image" => {
                for key in ["src", "href"] {
                    if let Some(v) = doc.attr(n, key).map(str::to_owned) {
                        if is_external(&v) {
                            continue;
                        }
                        let (target, _) = resolve_href(&base, &v);
                        if let Some(&(i, mime)) = ctx.res.item_map.get(&target) {
                            st.used.insert(i - 1);
                            doc.set_attr(n, key, format!("kindle:embed:{}?mime={mime}", to_ref(i)));
                        } else if let Some(&f) = ctx.svg_files.get(&target) {
                            doc.set_attr(n, key, format!("kindle:flow:{}?mime=image/svg+xml", to_ref(f)));
                        }
                    }
                }
            }
            "link" => {
                if let Some(v) = doc.attr(n, "href").map(str::to_owned) {
                    let (target, _) = resolve_href(&base, &v);
                    if let Some(&i) = ctx.sheets.get(&target) {
                        doc.set_attr(n, "href", format!("kindle:flow:{}?mime=text/css", to_ref(i)));
                    }
                }
            }
            _ => {}
        }
    }

    // inline <style> -> flows
    for n in doc.descendants(root) {
        if doc.nodes[n].name != "style" || doc.nodes[n].parent == NIL {
            continue;
        }
        let raw = doc.nodes[n].text.clone().unwrap_or_default();
        if raw.trim().is_empty() {
            doc.extract(n);
            continue;
        }
        let css = rewrite_css(&raw, &base, ctx, &mut st.used);
        let link = doc.create("link", vec![("type".into(), "text/css".into()), ("rel".into(), "stylesheet".into())]);
        let tail = doc.nodes[n].tail.take();
        doc.replace(n, link);
        doc.nodes[link].tail = Some(format!("\n{}", tail.unwrap_or_default()));
        st.inline_styles.push((link, css));
    }

    // inline <svg> -> image or svg flow
    for n in doc.descendants(root) {
        if doc.nodes[n].name != "svg" || doc.nodes[n].parent == NIL {
            continue;
        }
        // only top-level svgs (whose ancestors are not svg)
        let mut p = doc.nodes[n].parent;
        let mut nested = false;
        while p != NIL {
            if doc.nodes[p].name == "svg" {
                nested = true;
                break;
            }
            p = doc.nodes[p].parent;
        }
        if nested {
            continue;
        }
        let embedded = doc
            .descendants(n)
            .into_iter()
            .find(|&c| doc.nodes[c].name == "image")
            .and_then(|c| doc.attr(c, "href").filter(|h| h.starts_with("kindle:embed:")).map(str::to_owned));
        let img = doc.create("img", vec![]);
        if let Some(src) = embedded {
            doc.set_attr(img, "src", src);
            doc.set_attr(img, "alt", String::new());
            doc.replace(n, img);
        } else {
            let mut raw = doc.to_bytes(n, false);
            if raw.starts_with(b"<svg") {
                raw.splice(4..4, b" xmlns=\"http://www.w3.org/2000/svg\"".iter().copied());
            }
            doc.replace(n, img);
            st.svgs.push((img, raw));
        }
    }

    // internal links -> placeholders
    let mut count: u64 = 0;
    for n in doc.descendants(root) {
        if doc.nodes[n].name != "a" || doc.nodes[n].parent == NIL {
            continue;
        }
        let Some(h) = doc.attr(n, "href").map(str::to_owned) else { continue };
        if is_external(&h) {
            continue;
        }
        let (mut target, frag) = resolve_href(&base, &h);
        if target.is_empty() {
            target = href.to_owned();
        }
        if ctx.spine_set.contains(target.as_str()) {
            count += 1;
            let num = ((file_idx as u64) << 24) | count;
            doc.set_attr(n, "href", placeholder(num));
            st.links.push((num, target, frag));
        }
    }

    // aid / cid attributes
    let aidbase = file_idx as u64 * 1_000_000;
    let mut j: u64 = 0;
    let mut cid: u64 = 0;
    for n in doc.descendants(root) {
        if doc.nodes[n].parent == NIL {
            continue;
        }
        let mut id = doc.attr(n, "id").map(str::to_owned);
        let name = doc.nodes[n].name.clone();
        if id.is_none() && name == "a"
            && let Some(nm) = doc.attr(n, "name").map(str::to_owned) {
                doc.set_attr(n, "id", nm.clone());
                id = Some(nm);
            }
        if id.is_none() && !is_aid_able(&name) {
            continue;
        }
        let in_table = name == "table" || {
            let mut p = doc.nodes[n].parent;
            let mut found = false;
            while p != NIL {
                if doc.nodes[p].name == "table" {
                    found = true;
                    break;
                }
                p = doc.nodes[p].parent;
            }
            found
        };
        if in_table {
            if let Some(id) = id {
                cid += 1;
                let val = format!("c{}", aidbase + cid);
                doc.set_attr(n, "cid", val.clone());
                st.ids.push((id, val));
            }
        } else {
            let aid = to_base(aidbase + j, 32, 0);
            doc.set_attr(n, "aid", aid.clone());
            if n == body {
                st.body_aid = aid.clone();
            }
            if let Some(id) = id {
                st.ids.push((id, aid));
            }
            j += 1;
        }
    }
    st
}

// ------------------------------------------------------------- phase 3: chunking

struct Chunk {
    raw: Vec<u8>,
    starts: Vec<String>,
    ends: Vec<String>,
    insert_pos: usize,
    selector: String,
}

struct FileOut {
    skeleton_len: usize,
    raw_text: Vec<u8>, // skeleton + chunks
    chunks: Vec<(usize, usize, String)>, // (insert_pos local, len, selector)
    aids: Vec<(String, usize)>,          // aid/cid -> offset in rebuilt (local)
    len: usize,
}

struct Chunker<'a> {
    doc: &'a mut Doc,
    chunks: Vec<Chunk>,
    selector: String,
}

fn is_ws(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

impl Chunker<'_> {
    fn new_chunk(&self, raw: Vec<u8>) -> Chunk {
        Chunk { raw, starts: Vec::new(), ends: Vec::new(), insert_pos: 0, selector: self.selector.clone() }
    }

    fn chunk_up_text(&mut self, text: &str) {
        let mut esc = Vec::with_capacity(text.len());
        crate::dom::escape_text(text, &mut esc);
        let mut rest: &[u8] = &esc;
        let mut first = true;
        while first || !rest.is_empty() {
            let mut cut = rest.len().min(CHUNK_SIZE);
            while cut < rest.len() && cut > 0 && (rest[cut] & 0xC0) == 0x80 {
                cut -= 1;
            }
            // don't split an entity
            if cut < rest.len()
                && let Some(amp) = rest[cut.saturating_sub(6)..cut].iter().rposition(|&b| b == b'&') {
                    let a = cut.saturating_sub(6) + amp;
                    if !rest[a..cut].contains(&b';') && a > 0 {
                        cut = a;
                    }
                }
            let piece = &rest[..cut];
            let raw = if first {
                piece.to_vec()
            } else {
                let mut v = b"<span class=\"AmznBigTextBlock\">".to_vec();
                v.extend_from_slice(piece);
                v.extend_from_slice(b"</span>");
                v
            };
            let c = self.new_chunk(raw);
            self.chunks.push(c);
            rest = &rest[cut..];
            first = false;
        }
    }

    fn step_into(&mut self, tag: usize) {
        let aid = self.doc.attr(tag, "aid").unwrap_or("").to_owned();
        self.selector = format!("P-//*[@aid='{aid}']");
        let first = self.chunks.len();
        if let Some(t) = self.doc.nodes[tag].text.clone()
            && !is_ws(&t) {
                self.chunk_up_text(&t);
                self.doc.nodes[tag].text = None;
            }
        for child in self.doc.nodes[tag].children.clone() {
            let raw = self.doc.to_bytes(child, false);
            if raw.len() > CHUNK_SIZE && self.doc.attr(child, "aid").is_some() {
                self.step_into(child);
                if let Some(t) = self.doc.nodes[child].tail.clone()
                    && !is_ws(&t) {
                        self.chunk_up_text(&t);
                        self.doc.nodes[child].tail = None;
                    }
            } else {
                let c = self.new_chunk(raw);
                self.chunks.push(c);
                if let Some(t) = self.doc.nodes[child].tail.clone()
                    && !t.is_empty() {
                        self.chunk_up_text(&t);
                    }
                self.doc.remove(child);
            }
        }
        if self.chunks.len() > first {
            self.chunks[first].starts.push(aid.clone());
            self.chunks.last_mut().unwrap().ends.push(aid.clone());
        }
        self.selector = format!("S-//*[@aid='{aid}']");
    }
}

fn merge_small_chunks(chunks: Vec<Chunk>) -> Vec<Chunk> {
    let mut ans: Vec<Chunk> = Vec::with_capacity(chunks.len());
    for c in chunks {
        if let Some(prev) = ans.last_mut()
            && !(!c.starts.is_empty() || c.raw.len() + prev.raw.len() > CHUNK_SIZE || !prev.ends.is_empty()) {
                prev.raw.extend_from_slice(&c.raw);
                prev.ends = c.ends;
                continue;
            }
        ans.push(c);
    }
    ans
}

fn chunk_file(mut st: FileState) -> FileOut {
    // kindlegen always puts the aid last
    for node in &mut st.doc.nodes {
        if let Some(p) = node.attrs.iter().position(|(k, _)| k == "aid") {
            let a = node.attrs.remove(p);
            node.attrs.push(a);
        }
    }
    let root = st.doc.root;
    let body = st.doc.nodes[root].children.iter().copied().find(|&c| st.doc.nodes[c].name == "body").unwrap();

    let chunks = {
        let mut ch = Chunker { doc: &mut st.doc, chunks: Vec::new(), selector: String::new() };
        ch.step_into(body);
        ch.chunks
    };
    let mut chunks = merge_small_chunks(chunks);

    let mut skeleton = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n".to_vec();
    st.doc.serialize_root(&mut skeleton);
    let body_offset = memchr::memmem::find(&skeleton, b"<body").unwrap_or(0);

    // metrics of tags that remain in the skeleton
    let mut metrics: HashMap<String, (usize, usize)> = HashMap::new();
    for c in &chunks {
        for aid in c.starts.iter().chain(c.ends.iter()) {
            if metrics.contains_key(aid) {
                continue;
            }
            if let Some(n) = st.doc.descendants(root).into_iter().find(|&n| st.doc.attr(n, "aid") == Some(aid.as_str())) {
                let raw = st.doc.to_bytes(n, true);
                let text_len = st.doc.nodes[n].text.as_deref().map_or(0, str::len);
                let open = memchr::memchr(b'>', &raw).unwrap_or(0);
                let start = open + text_len + 1;
                let last_lt = memchr::memrchr(b'<', &raw).unwrap_or(0);
                let end = raw.len() - last_lt;
                metrics.insert(aid.clone(), (start, end));
            }
        }
    }
    let mut pos = body_offset;
    for c in &mut chunks {
        for t in &c.starts {
            pos += metrics.get(t).map_or(0, |m| m.0);
        }
        c.insert_pos = pos;
        pos += c.raw.len();
        for t in &c.ends {
            pos += metrics.get(t).map_or(0, |m| m.1);
        }
    }

    // rebuilt text -> aid offsets
    let total = skeleton.len() + chunks.iter().map(|c| c.raw.len()).sum::<usize>();
    let mut rebuilt = Vec::with_capacity(total);
    let mut sk = 0usize;
    let mut inserted = 0usize;
    for c in &chunks {
        let skel_pos = c.insert_pos - inserted;
        rebuilt.extend_from_slice(&skeleton[sk..skel_pos]);
        rebuilt.extend_from_slice(&c.raw);
        sk = skel_pos;
        inserted += c.raw.len();
    }
    rebuilt.extend_from_slice(&skeleton[sk..]);
    let aids = scan_aids(&rebuilt);

    let mut raw_text = skeleton;
    let skeleton_len = raw_text.len();
    let mut out_chunks = Vec::with_capacity(chunks.len());
    for c in chunks {
        out_chunks.push((c.insert_pos, c.raw.len(), c.selector));
        raw_text.extend_from_slice(&c.raw);
    }
    let len = raw_text.len();
    FileOut { skeleton_len, raw_text, chunks: out_chunks, aids, len }
}

/// Find `<tag ... aid="X">` / `cid="X"` positions (offset of the '<').
fn scan_aids(text: &[u8]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(lt) = memchr::memchr(b'<', &text[i..]) {
        let start = i + lt;
        let Some(gt) = memchr::memchr(b'>', &text[start..]) else { break };
        let tag = &text[start..start + gt];
        for pat in [&b" aid=\""[..], &b" cid=\""[..]] {
            if let Some(p) = memchr::memmem::find(tag, pat) {
                let vs = p + pat.len();
                if let Some(q) = memchr::memchr(b'"', &tag[vs..]) {
                    out.push((String::from_utf8_lossy(&tag[vs..vs + q]).into_owned(), start));
                }
            }
        }
        i = start + gt + 1;
    }
    out
}

// ------------------------------------------------------------- main conversion

struct ChunkRow {
    insert_pos: usize,
    selector: String,
    file_number: usize,
    seq: usize,
    start_pos: usize,
    length: usize,
}

pub fn convert(book: &Book, opts: &Options) -> Result<Output> {
    let mut warnings = book.warnings.clone();
    let res = build_resources(book, opts, &mut warnings);

    // flows: 0 = text, then stylesheets, svg files
    let mut flows: Vec<Vec<u8>> = vec![Vec::new()];
    let mut sheets = HashMap::new();
    let css_items: Vec<&crate::epub::ManItem> =
        book.manifest.iter().filter(|m| m.media_type == "text/css" && book.files.contains_key(&m.href)).collect();
    for m in &css_items {
        sheets.insert(m.href.clone(), flows.len());
        flows.push(Vec::new());
    }
    let mut svg_files = HashMap::new();
    for m in book.manifest.iter().filter(|m| m.media_type == "image/svg+xml") {
        if let Some(d) = book.files.get(&m.href) {
            svg_files.insert(m.href.clone(), flows.len());
            flows.push(d.clone());
        }
    }
    let ctx = Ctx { res: &res, sheets, svg_files, spine_set: book.spine.iter().map(String::as_str).collect() };

    let mut used: HashSet<usize> = res.always_used.clone();
    let css_out: Vec<(usize, String, HashSet<usize>)> = css_items
        .par_iter()
        .map(|m| {
            let mut u = HashSet::new();
            let txt = String::from_utf8_lossy(&book.files[&m.href]);
            let out = rewrite_css(&txt, dirname(&m.href), &ctx, &mut u);
            (ctx.sheets[&m.href], out, u)
        })
        .collect();
    for (i, css, u) in css_out {
        flows[i] = css.into_bytes();
        used.extend(u);
    }

    // phase 1 (parallel): markup
    let mut states: Vec<FileState> = book
        .spine
        .par_iter()
        .enumerate()
        .map(|(i, href)| process_markup(i, href, &book.files[href], &ctx))
        .collect();

    // phase 2 (sequential, cheap): inline style + svg flows
    let mut inline_map: HashMap<String, usize> = HashMap::new();
    for st in &mut states {
        for (link, css) in std::mem::take(&mut st.inline_styles) {
            let idx = *inline_map.entry(css.clone()).or_insert_with(|| {
                flows.push(css.into_bytes());
                flows.len() - 1
            });
            st.doc.set_attr(link, "href", format!("kindle:flow:{}?mime=text/css", to_ref(idx)));
        }
        for (img, svg) in std::mem::take(&mut st.svgs) {
            flows.push(svg);
            st.doc.set_attr(img, "src", format!("kindle:flow:{}?mime=image/svg+xml", to_ref(flows.len() - 1)));
        }
        used.extend(st.used.iter().copied());
    }

    let mut id_map: HashMap<(String, String), String> = HashMap::new();
    let mut link_targets: Vec<(u64, String, String)> = Vec::new();
    for st in &mut states {
        for (id, aid) in std::mem::take(&mut st.ids) {
            id_map.entry((st.href.clone(), id)).or_insert(aid);
        }
        id_map.insert((st.href.clone(), String::new()), st.body_aid.clone());
        link_targets.extend(std::mem::take(&mut st.links));
    }

    // phase 3 (parallel): chunk every file
    let outs: Vec<FileOut> = states.into_par_iter().map(chunk_file).collect();

    // phase 4: global tables
    let mut skel_table = Vec::with_capacity(outs.len());
    let mut chunk_table: Vec<ChunkRow> = Vec::new();
    let mut sp = 0usize;
    let mut file_starts = Vec::with_capacity(outs.len());
    for (fnum, o) in outs.iter().enumerate() {
        file_starts.push(sp);
        skel_table.push((fnum, o.chunks.len(), sp, o.skeleton_len));
        let mut cp = 0;
        for (ip, len, sel) in &o.chunks {
            let seq = chunk_table.len();
            chunk_table.push(ChunkRow { insert_pos: ip + sp, selector: sel.clone(), file_number: fnum, seq, start_pos: cp, length: *len });
            cp += len;
        }
        sp += o.len;
    }
    let text0_len = sp;

    // aid -> (chunk seq, offset in chunk, global offset)  (sweep, same rules as calibre)
    let mut aid_offset: HashMap<String, (usize, usize, usize)> = HashMap::new();
    {
        let mut ci = 0usize;
        for (fnum, o) in outs.iter().enumerate() {
            for (aid, off) in &o.aids {
                let g = file_starts[fnum] + off;
                while ci < chunk_table.len() && chunk_table[ci].insert_pos + chunk_table[ci].length <= g {
                    ci += 1;
                }
                let v = if chunk_table.is_empty() {
                    (0, 0, g)
                } else if ci >= chunk_table.len() {
                    let last = chunk_table.last().unwrap();
                    (last.seq, g.saturating_sub(last.insert_pos), g)
                } else if chunk_table[ci].insert_pos > g {
                    (chunk_table[ci].seq, 0, g)
                } else {
                    (chunk_table[ci].seq, g - chunk_table[ci].insert_pos, g)
                };
                aid_offset.insert(aid.clone(), v);
            }
        }
    }

    // phase 5 (parallel): resolve internal links
    let link_map: HashMap<String, Vec<u8>> = link_targets
        .iter()
        .filter_map(|(num, file, frag)| {
            let aid = id_map.get(&(file.clone(), frag.clone())).or_else(|| id_map.get(&(file.clone(), String::new())))?;
            let &(pos, fid, _) = aid_offset.get(aid)?;
            let repl = format!("{}:off:{}", to_base(pos as u64, 32, 4), to_base(fid as u64, 32, 10));
            Some((to_base(*num, 32, 10), repl.into_bytes()))
        })
        .collect();
    static PH: LazyLock<BRegex> = LazyLock::new(|| BRegex::new(r"kindle:pos:fid:0000:off:([0-9A-Za-z]{10})").unwrap());
    let texts: Vec<Vec<u8>> = outs
        .into_par_iter()
        .map(|o| {
            let mut t = o.raw_text;
            let hits: Vec<(usize, Vec<u8>)> = PH
                .captures_iter(&t)
                .filter_map(|c| {
                    let m = c.get(0).unwrap();
                    let key = String::from_utf8_lossy(&c[1]).into_owned();
                    link_map.get(&key).filter(|r| r.len() == 19).map(|r| (m.end() - 19, r.clone()))
                })
                .collect();
            for (at, r) in hits {
                t[at..at + 19].copy_from_slice(&r);
            }
            t
        })
        .collect();
    let mut text0 = Vec::with_capacity(text0_len);
    for t in texts {
        text0.extend_from_slice(&t);
    }
    flows[0] = text0;

    // text records (parallel compression)
    let text: Vec<u8> = flows.concat();
    let text_length = text.len();
    let nrec = text_length.div_ceil(RECORD_SIZE);
    let mut text_records: Vec<(Vec<u8>, usize)> = (0..nrec)
        .into_par_iter()
        .map(|i| {
            let start = i * RECORD_SIZE;
            let end = (start + RECORD_SIZE).min(text_length);
            let data = &text[start..end];
            let mut overlap: &[u8] = &[];
            if end < text_length {
                let mut k = end - 1;
                while k > start && (text[k] & 0xC0) == 0x80 {
                    k -= 1;
                }
                let lead = text[k];
                let clen = if lead < 0x80 {
                    1
                } else if lead >= 0xF0 {
                    4
                } else if lead >= 0xE0 {
                    3
                } else if lead >= 0xC0 {
                    2
                } else {
                    1
                };
                if k + clen > end {
                    overlap = &text[end..(k + clen).min(text_length)];
                }
            }
            let mut rec = if opts.compress { palmdoc::compress(data) } else { data.to_vec() };
            rec.extend_from_slice(overlap);
            rec.push(overlap.len() as u8);
            (rec, data.len())
        })
        .collect();
    let records_size: usize = text_records.iter().map(|r| r.0.len()).sum();
    let uncompressed: Vec<usize> = text_records.iter().map(|r| r.1).collect();

    // FDST
    let mut fdst = b"FDST".to_vec();
    fdst.extend_from_slice(&mobi::be32(12));
    fdst.extend_from_slice(&mobi::be32(flows.len() as u32));
    let mut s = 0u32;
    for f in &flows {
        fdst.extend_from_slice(&mobi::be32(s));
        s += f.len() as u32;
        fdst.extend_from_slice(&mobi::be32(s));
    }

    // SKEL + chunk indices
    let skel_entries: Vec<IndexEntry> = skel_table
        .iter()
        .map(|&(fnum, cc, start, len)| IndexEntry {
            name: format!("SKEL{fnum:010}").into_bytes(),
            tags: vec![("chunk_count", vec![cc as u64, cc as u64]), ("geometry", vec![start as u64, len as u64, start as u64, len as u64])],
        })
        .collect();
    let skel_records = mobi::build_index(&[tm("chunk_count", 1, 1, 3), tm("geometry", 6, 2, 12), END], 1, &skel_entries, &mobi::Cncx::empty());
    let chunk_cncx = mobi::Cncx::new(chunk_table.iter().map(|c| c.selector.as_str()));
    let chunk_entries: Vec<IndexEntry> = chunk_table
        .iter()
        .map(|c| IndexEntry {
            name: format!("{:010}", c.insert_pos).into_bytes(),
            tags: vec![
                ("cncx_offset", vec![chunk_cncx.get(&c.selector)]),
                ("file_number", vec![c.file_number as u64]),
                ("sequence_number", vec![c.seq as u64]),
                ("geometry", vec![c.start_pos as u64, c.length as u64]),
            ],
        })
        .collect();
    let chunk_records = mobi::build_index(
        &[tm("cncx_offset", 2, 1, 1), tm("file_number", 3, 1, 2), tm("sequence_number", 4, 1, 4), tm("geometry", 6, 2, 8), END],
        1,
        &chunk_entries,
        &chunk_cncx,
    );

    // NCX
    let locate = |href: &str| -> (usize, usize, usize) {
        let (file, frag) = match href.find('#') {
            Some(i) => (&href[..i], &href[i + 1..]),
            None => (href, ""),
        };
        let aid = id_map.get(&(file.to_owned(), frag.to_owned())).or_else(|| id_map.get(&(file.to_owned(), String::new())));
        match aid.and_then(|a| aid_offset.get(a)) {
            Some(&v) => v,
            None => (0, 0, chunk_table.first().map_or(0, |c| c.insert_pos)),
        }
    };
    struct Flat {
        id: usize,
        label: String,
        depth: i64,
        parent: Option<usize>,
        children: Vec<usize>,
        pos: usize,
        fid: usize,
        offset: usize,
    }
    let mut flat: Vec<Flat> = Vec::new();
    fn walk(nodes: &[TocNode], depth: i64, parent: Option<usize>, flat: &mut Vec<Flat>, locate: &dyn Fn(&str) -> (usize, usize, usize)) {
        for n in nodes {
            let id = flat.len();
            let (pos, fid, offset) = locate(&n.href);
            let label = if n.title.trim().is_empty() { "Unknown".to_owned() } else { n.title.clone() };
            flat.push(Flat { id, label, depth, parent, children: Vec::new(), pos, fid, offset });
            if let Some(p) = parent {
                flat[p].children.push(id);
            }
            walk(&n.children, depth + 1, Some(id), flat, locate);
        }
    }
    walk(&book.toc, 0, None, &mut flat, &locate);

    let mut ncx_records = Vec::new();
    let mut has_tbs = false;
    let mut tbs: Vec<Vec<u8>> = Vec::new();
    if !flat.is_empty() {
        let mut order: Vec<usize> = (0..flat.len()).collect();
        order.sort_by_key(|&i| (flat[i].depth, flat[i].offset));
        let mut new_index = vec![0usize; flat.len()];
        for (ni, &oi) in order.iter().enumerate() {
            new_index[oi] = ni;
        }
        let entries: Vec<&Flat> = order.iter().map(|&i| &flat[i]).collect();
        let lengths: Vec<usize> = entries
            .iter()
            .map(|e| {
                entries
                    .iter()
                    .filter(|o| o.depth <= e.depth && o.offset > e.offset)
                    .map(|o| o.offset)
                    .min()
                    .unwrap_or(text0_len)
                    - e.offset
            })
            .collect();
        let ncx_entries: Vec<NcxEntry> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| NcxEntry {
                index: i as i64,
                offset: e.offset as i64,
                length: lengths[i] as i64,
                depth: e.depth,
                parent: e.parent.map(|p| new_index[p] as i64),
            })
            .collect();
        tbs = mobi::trailing_byte_sequences(&ncx_entries, &uncompressed);
        has_tbs = true;

        let cncx = mobi::Cncx::new(entries.iter().map(|e| e.label.as_str()));
        let largest = entries.len().saturating_sub(1);
        let width = format!("{largest:X}").len().max(2);
        let idx_entries: Vec<IndexEntry> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut tags: Vec<(&'static str, Vec<u64>)> = vec![
                    ("offset", vec![e.offset as u64]),
                    ("length", vec![lengths[i] as u64]),
                    ("label", vec![cncx.get(&e.label)]),
                    ("depth", vec![e.depth as u64]),
                ];
                if let Some(p) = e.parent {
                    tags.push(("parent", vec![new_index[p] as u64]));
                }
                if let (Some(f), Some(l)) = (e.children.first(), e.children.last()) {
                    tags.push(("first_child", vec![new_index[*f] as u64]));
                    tags.push(("last_child", vec![new_index[*l] as u64]));
                }
                tags.push(("pos_fid", vec![e.pos as u64, e.fid as u64]));
                let _ = e.id;
                IndexEntry { name: format!("{i:0width$X}").into_bytes(), tags }
            })
            .collect();
        ncx_records = mobi::build_index(
            &[
                tm("offset", 1, 1, 1),
                tm("length", 2, 1, 2),
                tm("label", 3, 1, 4),
                tm("depth", 4, 1, 8),
                tm("parent", 21, 1, 16),
                tm("first_child", 22, 1, 32),
                tm("last_child", 23, 1, 64),
                tm("pos_fid", 6, 2, 128),
                END,
            ],
            1,
            &idx_entries,
            &cncx,
        );
    }

    // guide
    let mut start_offset = None;
    let mut guide: Vec<(String, String, usize, usize)> = Vec::new();
    for g in &book.guide {
        let (file, frag) = match g.href.find('#') {
            Some(i) => (&g.href[..i], &g.href[i + 1..]),
            None => (g.href.as_str(), ""),
        };
        let aid = id_map.get(&(file.to_owned(), frag.to_owned())).or_else(|| id_map.get(&(file.to_owned(), String::new())));
        let Some(&(pos, fid, off)) = aid.and_then(|a| aid_offset.get(a)) else { continue };
        let t = g.typ.to_lowercase();
        if g.title.to_lowercase() == "start" || matches!(t.as_str(), "start" | "other.start" | "text") {
            start_offset = Some(off as u32);
        }
        let title = if g.title.trim().is_empty() { "Unknown".to_owned() } else { g.title.clone() };
        guide.push((g.typ.clone(), title, pos, fid));
    }
    guide.sort_by(|a, b| a.0.cmp(&b.0));
    let guide_records = if guide.is_empty() {
        Vec::new()
    } else {
        let cncx = mobi::Cncx::new(guide.iter().map(|g| g.1.as_str()));
        let entries: Vec<IndexEntry> = guide
            .iter()
            .map(|g| IndexEntry {
                name: g.0.as_bytes().to_vec(),
                tags: vec![("title", vec![cncx.get(&g.1)]), ("pos_fid", vec![g.2 as u64, g.3 as u64])],
            })
            .collect();
        mobi::build_index(&[tm("title", 1, 1, 1), tm("pos_fid", 6, 2, 2), END], 1, &entries, &cncx)
    };

    // apply TBS
    if has_tbs {
        text_records.par_iter_mut().zip(tbs.par_iter()).for_each(|(r, t)| {
            r.0.extend_from_slice(&mobi::encode_trailing_data(t));
        });
    }

    // assemble
    let mut records: Vec<Vec<u8>> = vec![Vec::new()];
    let last_text_record = text_records.len();
    records.extend(text_records.into_iter().map(|r| r.0));
    let mut first_non_text = last_text_record + 1;
    if !records_size.is_multiple_of(4) {
        records.push(vec![0; records_size % 4]);
        first_non_text += 1;
    }
    let chunk_index = records.len();
    records.extend(chunk_records);
    let skel_index = records.len();
    records.extend(skel_records);
    let guide_index = if guide_records.is_empty() { NULL } else { records.len() as u32 };
    records.extend(guide_records);
    let ncx_index = if ncx_records.is_empty() { NULL } else { records.len() as u32 };
    records.extend(ncx_records);

    let first_resource = if res.records.is_empty() { NULL } else { records.len() as u32 };
    let num_resources = res.records.len() as u32;
    for (i, r) in res.records.iter().enumerate() {
        if used.contains(&i) {
            records.push(r.clone());
        } else {
            records.push(images::PLACEHOLDER_GIF.to_vec());
        }
    }
    let fdst_record = records.len();
    records.push(fdst);
    let flis_record = records.len();
    records.push(mobi::FLIS.to_vec());
    let fcis_record = records.len();
    records.push(mobi::fcis(text_length as u32));
    records.push(mobi::EOF_RECORD.to_vec());

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let asin = book.meta.uuid.clone().unwrap_or_else(|| pseudo_uuid(&book.meta.title, &book.meta.authors));
    let pubdate = book.meta.date.clone().unwrap_or_else(|| "2000-01-01".into());
    let exth = mobi::build_exth(&mobi::ExthInput {
        title: &book.meta.title,
        authors: &book.meta.authors,
        publisher: book.meta.publisher.as_deref(),
        description: book.meta.description.as_deref(),
        subjects: &book.meta.subjects,
        isbn: book.meta.isbn.as_deref(),
        language: &book.meta.language,
        asin: &asin,
        pubdate: &pubdate,
        cover_offset: res.cover_offset,
        thumbnail_offset: res.thumbnail_offset,
        start_offset,
        num_of_resources: num_resources,
        has_resources: num_resources > 0,
    });
    let full_title = mobi::utf8_text(&book.meta.title);
    let uid = (fxhash(asin.as_bytes()) ^ now) as u32;
    records[0] = mobi::build_record0(&mobi::Record0Input {
        compression: if opts.compress { 2 } else { 1 },
        text_length: text_length as u32,
        last_text_record: last_text_record as u16,
        uid,
        first_non_text_record: first_non_text as u32,
        language_code: mobi::language_code(&book.meta.language),
        first_resource_record: first_resource,
        fdst_record: fdst_record as u32,
        fdst_count: flows.len() as u32,
        fcis_record: fcis_record as u32,
        flis_record: flis_record as u32,
        extra_data_flags: if has_tbs { 0b11 } else { 0b1 },
        ncx_index,
        chunk_index: chunk_index as u32,
        skel_index: skel_index as u32,
        guide_index,
        exth: &exth,
        full_title: &full_title,
    });
    if last_text_record > 0xFFFF {
        anyhow::bail!("livro grande demais para AZW3 ({last_text_record} registros de texto, máximo 65535)");
    }

    let data = mobi::write_pdb(&book.meta.title, &records, now as u32);
    Ok(Output { data, asin, library_thumb: res.library_thumb, warnings, title: book.meta.title.clone() })
}

fn fxhash(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &x in b {
        h ^= u64::from(x);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn pseudo_uuid(title: &str, authors: &[String]) -> String {
    let h1 = fxhash(title.as_bytes());
    let h2 = fxhash(authors.join(",").as_bytes()) ^ h1.rotate_left(17);
    let s = format!("{h1:016x}{h2:016x}");
    format!("{}-{}-{}-{}-{}", &s[0..8], &s[8..12], &s[12..16], &s[16..20], &s[20..32])
}
