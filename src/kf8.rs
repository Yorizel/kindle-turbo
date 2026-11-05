//! EPUB -> KF8 (AZW3) pipeline. Port of calibre's writer8, parallelised per file/record/image.

use crate::dom::{Doc, NIL, is_aid_able};
use crate::epub::{Book, GuideRef, ManItem, TocNode, dirname, is_external, resolve, resolve_href};
use crate::images;
use crate::mobi::{self, END, IndexEntry, NULL, NcxEntry, RECORD_SIZE, tm, to_base, to_ref};
use crate::palmdoc;
use anyhow::{Result, anyhow};
use rayon::prelude::*;
use regex::Regex;
use regex::bytes::Regex as BRegex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const CHUNK_SIZE: usize = 8192;

/// (index of the chunk, offset inside it, global offset) of an anchor.
type Position = (usize, usize, usize);
type IdMap = HashMap<(String, String), String>;
type AidOffsets = HashMap<String, Position>;

static CSS_URL: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| Regex::new(r#"url\(\s*(['"]?)([^'")]+)(['"]?)\s*\)"#));
static CSS_IMPORT: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#"@import\s+(?:url\(\s*)?['"]?([^'")\s;]+)['"]?\s*\)?"#));
static PLACEHOLDER: LazyLock<Result<BRegex, regex::Error>> = LazyLock::new(|| BRegex::new(r"kindle:pos:fid:0000:off:([0-9A-Za-z]{10})"));

fn u32_of(n: usize) -> Result<u32> {
    u32::try_from(n).map_err(|_| anyhow!("valor {n} excede 32 bits"))
}

/// Borrows a lazily compiled regex, turning a compile failure into an error.
fn compiled<T>(re: &'static LazyLock<Result<T, regex::Error>>) -> Result<&'static T> {
    re.as_ref().map_err(|e| anyhow!("regex invalida: {e}"))
}

/// Splits `file#fragment` into its two halves (fragment is empty when absent).
fn split_fragment(href: &str) -> (&str, &str) {
    href.split_once('#').unwrap_or((href, ""))
}

#[derive(Debug)]
pub struct Options {
    pub cover: Option<Vec<u8>>,
    pub compress: bool,
    pub fetch_cover: bool,
}

#[derive(Debug)]
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

/// A cover that is neither supplied nor in the book: download one or draw one.
fn generated_cover(book: &Book, opts: &Options, warnings: &mut Vec<String>) -> Option<Vec<u8>> {
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
}

struct Processed {
    href: String,
    image: Result<(Vec<u8>, &'static str)>,
    thumb: Option<Vec<u8>>,
    library: Option<Vec<u8>>,
}

fn build_resources(book: &Book, opts: &Options, warnings: &mut Vec<String>) -> Result<Resources> {
    let mut items: Vec<(String, &[u8])> = book
        .manifest
        .iter()
        .filter(|m| m.media_type.starts_with("image/") && m.media_type != "image/svg+xml")
        .filter_map(|m| book.files.get(&m.href).map(|d| (m.href.clone(), d.as_slice())))
        .collect();
    let mut cover_href = book.cover.clone();
    let generated = if opts.cover.is_none() && cover_href.is_none() { generated_cover(book, opts, warnings) } else { None };
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
    let processed: Vec<Processed> = items
        .par_iter()
        .map(|(href, data)| {
            let is_cover = cover_href.as_deref() == Some(href.as_str());
            let image = images::process(data);
            let (thumb, library) = if is_cover {
                (images::thumbnail(data, 180, 240, 16 * 1024).ok(), images::thumbnail(data, 330, 470, 64 * 1024).ok())
            } else {
                (None, None)
            };
            Processed { href: href.clone(), image, thumb, library }
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
    for Processed { href, image, thumb, library } in processed {
        match image {
            Err(e) => warnings.push(format!("imagem ignorada {href}: {e}")),
            Ok((data, mime)) => {
                r.records.push(data);
                r.item_map.insert(href.clone(), (index, mime));
                index += 1;
                if cover_href.as_deref() == Some(href.as_str()) {
                    let cover_idx = index - 2;
                    r.cover_offset = Some(u32_of(cover_idx)?);
                    r.always_used.insert(cover_idx);
                    if let Some(t) = thumb {
                        r.records.push(t);
                        r.thumbnail_offset = Some(u32_of(index - 1)?);
                        r.always_used.insert(index - 1);
                        index += 1;
                    }
                    r.library_thumb = library;
                }
            }
        }
    }
    Ok(r)
}

struct Ctx<'a> {
    res: &'a Resources,
    sheets: HashMap<String, usize>,
    svg_files: HashMap<String, usize>,
    spine_set: HashSet<&'a str>,
}

fn rewrite_css(css: &str, base: &str, ctx: &Ctx<'_>, used: &mut HashSet<usize>) -> Result<String> {
    let s = compiled(&CSS_IMPORT)?.replace_all(css, |c: &regex::Captures<'_>| {
        let target = resolve(base, &c[1]);
        match ctx.sheets.get(&target) {
            Some(&i) => format!("@import url(kindle:flow:{}?mime=text/css)", to_ref(i)),
            None => c[0].to_string(),
        }
    });
    let rewritten = compiled(&CSS_URL)?
        .replace_all(&s, |c: &regex::Captures<'_>| {
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
        .into_owned();
    Ok(rewritten)
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

fn has_ancestor_named(doc: &Doc, node: usize, name: &str) -> bool {
    let mut p = doc.nodes[node].parent;
    while p != NIL {
        if doc.nodes[p].name == name {
            return true;
        }
        p = doc.nodes[p].parent;
    }
    false
}

/// Returns the `<body>` of the document, wrapping the loose content in a new one if missing.
fn ensure_body(doc: &mut Doc, root: usize) -> usize {
    if let Some(b) = doc.nodes[root].children.iter().copied().find(|&c| doc.nodes[c].name == "body") {
        return b;
    }
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
}

/// Drops empty scripts and points images and stylesheet links at their kindle resources.
fn rewrite_references(st: &mut FileState, base: &str, ctx: &Ctx<'_>) {
    let doc = &mut st.doc;
    let root = doc.root;
    for n in doc.descendants(root) {
        if doc.nodes[n].parent == NIL {
            continue;
        }
        doc.remove_attr(n, "aid");
        doc.remove_attr(n, "cid");
        let name = doc.nodes[n].name.clone();
        match name.as_str() {
            "script" if doc.nodes[n].text.as_deref().is_none_or(|t| t.trim().is_empty()) && doc.attr(n, "src").is_none() => {
                doc.extract(n);
            }
            "img" | "image" => {
                for key in ["src", "href"] {
                    if let Some(v) = doc.attr(n, key).map(str::to_owned) {
                        if is_external(&v) {
                            continue;
                        }
                        let (target, _) = resolve_href(base, &v);
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
                    let (target, _) = resolve_href(base, &v);
                    if let Some(&i) = ctx.sheets.get(&target) {
                        doc.set_attr(n, "href", format!("kindle:flow:{}?mime=text/css", to_ref(i)));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Inline `<style>` elements become `<link>`s to new CSS flows.
fn extract_inline_styles(st: &mut FileState, base: &str, ctx: &Ctx<'_>) -> Result<()> {
    let doc = &mut st.doc;
    let root = doc.root;
    for n in doc.descendants(root) {
        if doc.nodes[n].name != "style" || doc.nodes[n].parent == NIL {
            continue;
        }
        let raw = doc.nodes[n].text.clone().unwrap_or_default();
        if raw.trim().is_empty() {
            doc.extract(n);
            continue;
        }
        let css = rewrite_css(&raw, base, ctx, &mut st.used)?;
        let link = doc.create("link", vec![("type".into(), "text/css".into()), ("rel".into(), "stylesheet".into())]);
        let tail = doc.nodes[n].tail.take();
        doc.replace(n, link);
        doc.nodes[link].tail = Some(format!("\n{}", tail.unwrap_or_default()));
        st.inline_styles.push((link, css));
    }
    Ok(())
}

/// Inline `<svg>` becomes an `<img>` (embedded raster) or an svg flow.
fn extract_inline_svgs(st: &mut FileState) {
    let doc = &mut st.doc;
    let root = doc.root;
    for n in doc.descendants(root) {
        if doc.nodes[n].name != "svg" || doc.nodes[n].parent == NIL {
            continue;
        }
        // only top-level svgs (whose ancestors are not svg)
        if has_ancestor_named(doc, n, "svg") {
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
}

/// Internal links get numbered placeholders, resolved once positions are known.
fn placeholder_links(st: &mut FileState, base: &str, file_idx: usize, ctx: &Ctx<'_>) {
    let doc = &mut st.doc;
    let root = doc.root;
    let mut count: u64 = 0;
    for n in doc.descendants(root) {
        if doc.nodes[n].name != "a" || doc.nodes[n].parent == NIL {
            continue;
        }
        let Some(h) = doc.attr(n, "href").map(str::to_owned) else { continue };
        if is_external(&h) {
            continue;
        }
        let (mut target, frag) = resolve_href(base, &h);
        if target.is_empty() {
            st.href.clone_into(&mut target);
        }
        if ctx.spine_set.contains(target.as_str()) {
            count += 1;
            let num = ((file_idx as u64) << 24) | count;
            doc.set_attr(n, "href", placeholder(num));
            st.links.push((num, target, frag));
        }
    }
}

/// Assigns `aid` (or `cid` inside tables) attributes and records the id -> aid map.
fn assign_aids(st: &mut FileState, file_idx: usize, body: usize) {
    let doc = &mut st.doc;
    let root = doc.root;
    let aidbase = file_idx as u64 * 1_000_000;
    let mut j: u64 = 0;
    let mut cid: u64 = 0;
    for n in doc.descendants(root) {
        if doc.nodes[n].parent == NIL {
            continue;
        }
        let mut id = doc.attr(n, "id").map(str::to_owned);
        let name = doc.nodes[n].name.clone();
        if id.is_none()
            && name == "a"
            && let Some(nm) = doc.attr(n, "name").map(str::to_owned)
        {
            doc.set_attr(n, "id", nm.clone());
            id = Some(nm);
        }
        if id.is_none() && !is_aid_able(&name) {
            continue;
        }
        let in_table = name == "table" || has_ancestor_named(doc, n, "table");
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
                st.body_aid.clone_from(&aid);
            }
            if let Some(id) = id {
                st.ids.push((id, aid));
            }
            j += 1;
        }
    }
}

fn process_markup(file_idx: usize, href: &str, data: &[u8], ctx: &Ctx<'_>) -> Result<FileState> {
    let mut doc = Doc::parse(data);
    let base = dirname(href).to_owned();
    let root = doc.root;
    doc.nodes[root].name = "html".into();
    doc.nodes[root].attrs.retain(|(k, _)| k == "lang" || k == "class" || k == "style");
    doc.nodes[root].tail = Some("\n".into());

    let body = ensure_body(&mut doc, root);
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
    rewrite_references(&mut st, &base, ctx);
    extract_inline_styles(&mut st, &base, ctx)?;
    extract_inline_svgs(&mut st);
    placeholder_links(&mut st, &base, file_idx, ctx);
    assign_aids(&mut st, file_idx, body);
    Ok(st)
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
    raw_text: Vec<u8>,                   // skeleton + chunks
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
                && let Some(amp) = rest[cut.saturating_sub(6)..cut].iter().rposition(|&b| b == b'&')
            {
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
            && !is_ws(&t)
        {
            self.chunk_up_text(&t);
            self.doc.nodes[tag].text = None;
        }
        for child in self.doc.nodes[tag].children.clone() {
            let raw = self.doc.to_bytes(child, false);
            if raw.len() > CHUNK_SIZE && self.doc.attr(child, "aid").is_some() {
                self.step_into(child);
                if let Some(t) = self.doc.nodes[child].tail.clone()
                    && !is_ws(&t)
                {
                    self.chunk_up_text(&t);
                    self.doc.nodes[child].tail = None;
                }
            } else {
                let c = self.new_chunk(raw);
                self.chunks.push(c);
                if let Some(t) = self.doc.nodes[child].tail.clone()
                    && !t.is_empty()
                {
                    self.chunk_up_text(&t);
                }
                self.doc.remove(child);
            }
        }
        if self.chunks.len() > first {
            self.chunks[first].starts.push(aid.clone());
            if let Some(last) = self.chunks.last_mut() {
                last.ends.push(aid.clone());
            }
        }
        self.selector = format!("S-//*[@aid='{aid}']");
    }
}

fn merge_small_chunks(chunks: Vec<Chunk>) -> Vec<Chunk> {
    let mut ans: Vec<Chunk> = Vec::with_capacity(chunks.len());
    for c in chunks {
        if let Some(prev) = ans.last_mut()
            && !(!c.starts.is_empty() || c.raw.len() + prev.raw.len() > CHUNK_SIZE || !prev.ends.is_empty())
        {
            prev.raw.extend_from_slice(&c.raw);
            prev.ends = c.ends;
            continue;
        }
        ans.push(c);
    }
    ans
}

fn chunk_file(mut st: FileState) -> Result<FileOut> {
    // kindlegen always puts the aid last
    for node in &mut st.doc.nodes {
        if let Some(p) = node.attrs.iter().position(|(k, _)| k == "aid") {
            let a = node.attrs.remove(p);
            node.attrs.push(a);
        }
    }
    let root = st.doc.root;
    let body = st.doc.nodes[root]
        .children
        .iter()
        .copied()
        .find(|&c| st.doc.nodes[c].name == "body")
        .ok_or_else(|| anyhow!("documento sem <body>: {}", st.href))?;

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
    Ok(FileOut { skeleton_len, raw_text, chunks: out_chunks, aids, len })
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

/// Flow 0 is the text, followed by the stylesheets and the svg files.
struct FlowPlan<'a> {
    flows: Vec<Vec<u8>>,
    sheets: HashMap<String, usize>,
    svg_files: HashMap<String, usize>,
    css_items: Vec<&'a ManItem>,
}

fn plan_flows(book: &Book) -> FlowPlan<'_> {
    let mut flows: Vec<Vec<u8>> = vec![Vec::new()];
    let mut sheets = HashMap::new();
    let css_items: Vec<&ManItem> =
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
    FlowPlan { flows, sheets, svg_files, css_items }
}

fn missing_file(href: &str) -> anyhow::Error {
    anyhow!("arquivo ausente no EPUB: {href}")
}

/// Rewrites every stylesheet (in parallel) into its flow.
fn fill_stylesheets(book: &Book, css_items: &[&ManItem], ctx: &Ctx<'_>, flows: &mut [Vec<u8>], used: &mut HashSet<usize>) -> Result<()> {
    let css_out = css_items
        .par_iter()
        .map(|m| {
            let mut u = HashSet::new();
            let data = book.files.get(&m.href).ok_or_else(|| missing_file(&m.href))?;
            let txt = String::from_utf8_lossy(data);
            let out = rewrite_css(&txt, dirname(&m.href), ctx, &mut u)?;
            let flow = ctx.sheets.get(&m.href).copied().ok_or_else(|| missing_file(&m.href))?;
            Ok((flow, out, u))
        })
        .collect::<Result<Vec<(usize, String, HashSet<usize>)>>>()?;
    for (i, css, u) in css_out {
        flows[i] = css.into_bytes();
        used.extend(u);
    }
    Ok(())
}

/// Phase 2 (sequential, cheap): inline style + svg flows.
fn attach_flows(states: &mut [FileState], flows: &mut Vec<Vec<u8>>, used: &mut HashSet<usize>) {
    let mut inline_map: HashMap<String, usize> = HashMap::new();
    for st in states {
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
}

/// Collects the id -> aid map and the pending link placeholders of every file.
fn collect_ids(states: &mut [FileState]) -> (IdMap, Vec<(u64, String, String)>) {
    let mut id_map: IdMap = HashMap::new();
    let mut link_targets = Vec::new();
    for st in states {
        for (id, aid) in std::mem::take(&mut st.ids) {
            id_map.entry((st.href.clone(), id)).or_insert(aid);
        }
        id_map.insert((st.href.clone(), String::new()), st.body_aid.clone());
        link_targets.extend(std::mem::take(&mut st.links));
    }
    (id_map, link_targets)
}

struct Tables {
    skel_table: Vec<(usize, usize, usize, usize)>, // (file, chunk count, start, length)
    chunk_table: Vec<ChunkRow>,
    file_starts: Vec<usize>,
    text0_len: usize,
}

/// Phase 4: global skeleton and chunk tables.
fn build_tables(outs: &[FileOut]) -> Tables {
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
    Tables { skel_table, chunk_table, file_starts, text0_len: sp }
}

/// aid -> (chunk seq, offset in chunk, global offset)  (sweep, same rules as calibre)
fn compute_aid_offsets(outs: &[FileOut], tables: &Tables) -> AidOffsets {
    let chunk_table = &tables.chunk_table;
    let mut aid_offset: AidOffsets = HashMap::new();
    let mut ci = 0usize;
    for (fnum, o) in outs.iter().enumerate() {
        for (aid, off) in &o.aids {
            let g = tables.file_starts[fnum] + off;
            while ci < chunk_table.len() && chunk_table[ci].insert_pos + chunk_table[ci].length <= g {
                ci += 1;
            }
            let v = match chunk_table.get(ci) {
                None => chunk_table.last().map_or((0, 0, g), |last| (last.seq, g.saturating_sub(last.insert_pos), g)),
                Some(row) if row.insert_pos > g => (row.seq, 0, g),
                Some(row) => (row.seq, g - row.insert_pos, g),
            };
            aid_offset.insert(aid.clone(), v);
        }
    }
    aid_offset
}

/// Maps `file#fragment` references to positions in the text.
struct Locator<'a> {
    id_map: &'a IdMap,
    aid_offset: &'a AidOffsets,
    fallback_offset: usize,
}

impl Locator<'_> {
    fn find(&self, file: &str, frag: &str) -> Option<Position> {
        let aid = self.id_map.get(&(file.to_owned(), frag.to_owned())).or_else(|| self.id_map.get(&(file.to_owned(), String::new())));
        aid.and_then(|a| self.aid_offset.get(a)).copied()
    }

    fn find_href(&self, href: &str) -> Option<Position> {
        let (file, frag) = split_fragment(href);
        self.find(file, frag)
    }

    fn locate(&self, href: &str) -> Position {
        self.find_href(href).unwrap_or((0, 0, self.fallback_offset))
    }
}

/// Phase 5 (parallel): patch the link placeholders and join the files into flow 0.
fn resolve_links(outs: Vec<FileOut>, link_targets: &[(u64, String, String)], locator: &Locator<'_>, text_len: usize) -> Result<Vec<u8>> {
    let link_map: HashMap<String, Vec<u8>> = link_targets
        .iter()
        .filter_map(|(num, file, frag)| {
            let (pos, fid, _) = locator.find(file, frag)?;
            let repl = format!("{}:off:{}", to_base(pos as u64, 32, 4), to_base(fid as u64, 32, 10));
            Some((to_base(*num, 32, 10), repl.into_bytes()))
        })
        .collect();
    let placeholder_re = compiled(&PLACEHOLDER)?;
    let texts: Vec<Vec<u8>> = outs
        .into_par_iter()
        .map(|o| {
            let mut t = o.raw_text;
            let hits: Vec<(usize, Vec<u8>)> = placeholder_re
                .captures_iter(&t)
                .filter_map(|c| {
                    let m = c.get(0)?;
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
    let mut joined = Vec::with_capacity(text_len);
    for t in texts {
        joined.extend_from_slice(&t);
    }
    Ok(joined)
}

struct TextRecords {
    records: Vec<(Vec<u8>, usize)>, // (record bytes, uncompressed length)
    size: usize,
}

/// Bytes of a multibyte character that straddles the end of `text[start..end]`.
fn straddling_bytes(text: &[u8], start: usize, end: usize) -> &[u8] {
    if end >= text.len() {
        return &[];
    }
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
    if k + clen > end { &text[end..(k + clen).min(text.len())] } else { &[] }
}

/// Text records (parallel compression), each with its multibyte overflow trailer.
fn build_text_records(text: &[u8], compress: bool) -> Result<TextRecords> {
    let text_length = text.len();
    let nrec = text_length.div_ceil(RECORD_SIZE);
    let records = (0..nrec)
        .into_par_iter()
        .map(|i| {
            let start = i * RECORD_SIZE;
            let end = (start + RECORD_SIZE).min(text_length);
            let data = &text[start..end];
            let overlap = straddling_bytes(text, start, end);
            let mut rec = if compress { palmdoc::compress(data) } else { data.to_vec() };
            rec.extend_from_slice(overlap);
            let overlap_len = u8::try_from(overlap.len())?;
            rec.push(overlap_len);
            Ok((rec, data.len()))
        })
        .collect::<Result<Vec<_>>>()?;
    let size = records.iter().map(|r| r.0.len()).sum();
    Ok(TextRecords { records, size })
}

fn build_fdst(flows: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut fdst = b"FDST".to_vec();
    fdst.extend_from_slice(&mobi::be32(12));
    fdst.extend_from_slice(&mobi::be32(u32_of(flows.len())?));
    let mut s = 0u32;
    for f in flows {
        fdst.extend_from_slice(&mobi::be32(s));
        s += u32_of(f.len())?;
        fdst.extend_from_slice(&mobi::be32(s));
    }
    Ok(fdst)
}

fn build_skel_index(skel_table: &[(usize, usize, usize, usize)]) -> Vec<Vec<u8>> {
    let skel_entries: Vec<IndexEntry> = skel_table
        .iter()
        .map(|&(fnum, cc, start, len)| IndexEntry {
            name: format!("SKEL{fnum:010}").into_bytes(),
            tags: vec![("chunk_count", vec![cc as u64, cc as u64]), ("geometry", vec![start as u64, len as u64, start as u64, len as u64])],
        })
        .collect();
    mobi::build_index(&[tm("chunk_count", 1, 1, 3), tm("geometry", 6, 2, 12), END], 1, &skel_entries, &mobi::Cncx::empty())
}

fn build_chunk_index(chunk_table: &[ChunkRow]) -> Vec<Vec<u8>> {
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
    mobi::build_index(
        &[tm("cncx_offset", 2, 1, 1), tm("file_number", 3, 1, 2), tm("sequence_number", 4, 1, 4), tm("geometry", 6, 2, 8), END],
        1,
        &chunk_entries,
        &chunk_cncx,
    )
}

// ------------------------------------------------------------- NCX

struct Flat {
    label: String,
    depth: usize,
    parent: Option<usize>,
    children: Vec<usize>,
    pos: usize,
    fid: usize,
    offset: usize,
}

fn flatten_toc(nodes: &[TocNode], depth: usize, parent: Option<usize>, flat: &mut Vec<Flat>, locator: &Locator<'_>) {
    for n in nodes {
        let id = flat.len();
        let (pos, fid, offset) = locator.locate(&n.href);
        let label = if n.title.trim().is_empty() { "Unknown".to_owned() } else { n.title.clone() };
        flat.push(Flat { label, depth, parent, children: Vec::new(), pos, fid, offset });
        if let Some(p) = parent {
            flat[p].children.push(id);
        }
        flatten_toc(&n.children, depth + 1, Some(id), flat, locator);
    }
}

struct NcxOut {
    records: Vec<Vec<u8>>,
    tbs: Vec<Vec<u8>>,
}

/// TOC entries sorted by (depth, offset), plus the old -> new index mapping.
struct SortedToc<'a> {
    entries: Vec<&'a Flat>,
    new_index: Vec<usize>,
    lengths: Vec<usize>,
}

fn sort_toc(flat: &[Flat], text_len: usize) -> SortedToc<'_> {
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
            entries.iter().filter(|o| o.depth <= e.depth && o.offset > e.offset).map(|o| o.offset).min().unwrap_or(text_len) - e.offset
        })
        .collect();
    SortedToc { entries, new_index, lengths }
}

fn ncx_index_records(toc: &SortedToc<'_>) -> Vec<Vec<u8>> {
    let SortedToc { entries, new_index, lengths } = toc;
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
            IndexEntry { name: format!("{i:0width$X}").into_bytes(), tags }
        })
        .collect();
    mobi::build_index(
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
    )
}

/// NCX index plus the trailing byte sequences of the text records (none for an empty TOC).
fn build_ncx(toc: &[TocNode], locator: &Locator<'_>, text_len: usize, uncompressed: &[usize]) -> Result<Option<NcxOut>> {
    let mut flat: Vec<Flat> = Vec::new();
    flatten_toc(toc, 0, None, &mut flat, locator);
    if flat.is_empty() {
        return Ok(None);
    }
    let sorted = sort_toc(&flat, text_len);
    let ncx_entries = sorted
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            Ok(NcxEntry {
                index: i64::try_from(i)?,
                offset: i64::try_from(e.offset)?,
                length: i64::try_from(sorted.lengths[i])?,
                depth: i64::try_from(e.depth)?,
                parent: e.parent.map(|p| i64::try_from(sorted.new_index[p])).transpose()?,
            })
        })
        .collect::<Result<Vec<NcxEntry>>>()?;
    let tbs = mobi::trailing_byte_sequences(&ncx_entries, uncompressed);
    Ok(Some(NcxOut { records: ncx_index_records(&sorted), tbs }))
}

// ------------------------------------------------------------- guide

struct GuideRow {
    typ: String,
    title: String,
    pos: usize,
    fid: usize,
}

/// Guide index records and the "start reading" offset.
fn build_guide(guide: &[GuideRef], locator: &Locator<'_>) -> Result<(Vec<Vec<u8>>, Option<u32>)> {
    let mut start_offset = None;
    let mut rows: Vec<GuideRow> = Vec::new();
    for g in guide {
        let Some((pos, fid, off)) = locator.find_href(&g.href) else { continue };
        let t = g.typ.to_lowercase();
        if g.title.to_lowercase() == "start" || matches!(t.as_str(), "start" | "other.start" | "text") {
            start_offset = Some(u32_of(off)?);
        }
        let title = if g.title.trim().is_empty() { "Unknown".to_owned() } else { g.title.clone() };
        rows.push(GuideRow { typ: g.typ.clone(), title, pos, fid });
    }
    rows.sort_by(|a, b| a.typ.cmp(&b.typ));
    if rows.is_empty() {
        return Ok((Vec::new(), start_offset));
    }
    let cncx = mobi::Cncx::new(rows.iter().map(|g| g.title.as_str()));
    let entries: Vec<IndexEntry> = rows
        .iter()
        .map(|g| IndexEntry {
            name: g.typ.as_bytes().to_vec(),
            tags: vec![("title", vec![cncx.get(&g.title)]), ("pos_fid", vec![g.pos as u64, g.fid as u64])],
        })
        .collect();
    let records = mobi::build_index(&[tm("title", 1, 1, 1), tm("pos_fid", 6, 2, 2), END], 1, &entries, &cncx);
    Ok((records, start_offset))
}

// ------------------------------------------------------------- assembly

struct IndexSet {
    chunk: Vec<Vec<u8>>,
    skel: Vec<Vec<u8>>,
    guide: Vec<Vec<u8>>,
    ncx: Vec<Vec<u8>>,
}

/// Where every kind of record ended up in the PDB.
struct Layout {
    records: Vec<Vec<u8>>,
    first_non_text: usize,
    chunk_index: usize,
    skel_index: usize,
    guide_index: u32,
    ncx_index: u32,
    first_resource: u32,
    num_resources: u32,
    fdst_record: usize,
    flis_record: usize,
    fcis_record: usize,
}

fn assemble_records(
    text: TextRecords,
    indices: IndexSet,
    res: &Resources,
    used: &HashSet<usize>,
    fdst: Vec<u8>,
    text_length: usize,
) -> Result<Layout> {
    let mut records: Vec<Vec<u8>> = vec![Vec::new()];
    let last_text_record = text.records.len();
    records.extend(text.records.into_iter().map(|r| r.0));
    let mut first_non_text = last_text_record + 1;
    if !text.size.is_multiple_of(4) {
        records.push(vec![0; text.size % 4]);
        first_non_text += 1;
    }
    let chunk_index = records.len();
    records.extend(indices.chunk);
    let skel_index = records.len();
    records.extend(indices.skel);
    let guide_index = if indices.guide.is_empty() { NULL } else { u32_of(records.len())? };
    records.extend(indices.guide);
    let ncx_index = if indices.ncx.is_empty() { NULL } else { u32_of(records.len())? };
    records.extend(indices.ncx);

    let first_resource = if res.records.is_empty() { NULL } else { u32_of(records.len())? };
    let num_resources = u32_of(res.records.len())?;
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
    let footer_record = records.len();
    records.push(mobi::fcis(u32_of(text_length)?));
    records.push(mobi::EOF_RECORD.to_vec());
    Ok(Layout {
        records,
        first_non_text,
        chunk_index,
        skel_index,
        guide_index,
        ncx_index,
        first_resource,
        num_resources,
        fdst_record,
        flis_record,
        fcis_record: footer_record,
    })
}

/// Everything record 0 needs besides the record layout.
struct HeaderInput<'a> {
    book: &'a Book,
    opts: &'a Options,
    res: &'a Resources,
    asin: &'a str,
    start_offset: Option<u32>,
    has_tbs: bool,
    flow_count: usize,
    text_length: usize,
    text_record_count: usize,
    uid: u32,
}

fn build_header(h: &HeaderInput<'_>, layout: &Layout) -> Result<Vec<u8>> {
    let book = h.book;
    let pubdate = book.meta.date.clone().unwrap_or_else(|| "2000-01-01".into());
    let exth = mobi::build_exth(&mobi::ExthInput {
        title: &book.meta.title,
        authors: &book.meta.authors,
        publisher: book.meta.publisher.as_deref(),
        description: book.meta.description.as_deref(),
        subjects: &book.meta.subjects,
        isbn: book.meta.isbn.as_deref(),
        language: &book.meta.language,
        asin: h.asin,
        pubdate: &pubdate,
        cover_offset: h.res.cover_offset,
        thumbnail_offset: h.res.thumbnail_offset,
        start_offset: h.start_offset,
        num_of_resources: layout.num_resources,
        has_resources: layout.num_resources > 0,
    });
    let full_title = mobi::utf8_text(&book.meta.title);
    let last_text_record = u16::try_from(h.text_record_count)
        .map_err(|_| anyhow!("livro grande demais para AZW3 ({} registros de texto, máximo 65535)", h.text_record_count))?;
    Ok(mobi::build_record0(&mobi::Record0Input {
        compression: if h.opts.compress { 2 } else { 1 },
        text_length: u32_of(h.text_length)?,
        last_text_record,
        uid: h.uid,
        first_non_text_record: u32_of(layout.first_non_text)?,
        language_code: mobi::language_code(&book.meta.language),
        first_resource_record: layout.first_resource,
        fdst_record: u32_of(layout.fdst_record)?,
        fdst_count: u32_of(h.flow_count)?,
        fcis_record: u32_of(layout.fcis_record)?,
        flis_record: u32_of(layout.flis_record)?,
        extra_data_flags: if h.has_tbs { 0b11 } else { 0b1 },
        ncx_index: layout.ncx_index,
        chunk_index: u32_of(layout.chunk_index)?,
        skel_index: u32_of(layout.skel_index)?,
        guide_index: layout.guide_index,
        exth: &exth,
        full_title: &full_title,
    }))
}

/// Converts a parsed EPUB into an AZW3 (KF8) file.
///
/// # Errors
/// Fails when the EPUB references files it does not contain, a regex cannot be
/// compiled, or the book is too large for the AZW3 format's 16/32-bit fields.
pub fn convert(book: &Book, opts: &Options) -> Result<Output> {
    let mut warnings = book.warnings.clone();
    let res = build_resources(book, opts, &mut warnings)?;

    let FlowPlan { mut flows, sheets, svg_files, css_items } = plan_flows(book);
    let ctx = Ctx { res: &res, sheets, svg_files, spine_set: book.spine.iter().map(String::as_str).collect() };

    let mut used: HashSet<usize> = res.always_used.clone();
    fill_stylesheets(book, &css_items, &ctx, &mut flows, &mut used)?;

    // phase 1 (parallel): markup
    let mut states: Vec<FileState> = book
        .spine
        .par_iter()
        .enumerate()
        .map(|(i, href)| {
            let data = book.files.get(href).ok_or_else(|| missing_file(href))?;
            process_markup(i, href, data, &ctx)
        })
        .collect::<Result<_>>()?;
    attach_flows(&mut states, &mut flows, &mut used);
    let (id_map, link_targets) = collect_ids(&mut states);

    // phase 3 (parallel): chunk every file
    let chunked: Vec<FileOut> = states.into_par_iter().map(chunk_file).collect::<Result<_>>()?;
    let tables = build_tables(&chunked);
    let aid_offset = compute_aid_offsets(&chunked, &tables);
    let locator =
        Locator { id_map: &id_map, aid_offset: &aid_offset, fallback_offset: tables.chunk_table.first().map_or(0, |c| c.insert_pos) };
    flows[0] = resolve_links(chunked, &link_targets, &locator, tables.text0_len)?;

    let text: Vec<u8> = flows.concat();
    let text_length = text.len();
    let mut text_records = build_text_records(&text, opts.compress)?;
    let uncompressed: Vec<usize> = text_records.records.iter().map(|r| r.1).collect();
    let fdst = build_fdst(&flows)?;

    let ncx = build_ncx(&book.toc, &locator, tables.text0_len, &uncompressed)?;
    let (guide_records, start_offset) = build_guide(&book.guide, &locator)?;
    let indices = IndexSet {
        chunk: build_chunk_index(&tables.chunk_table),
        skel: build_skel_index(&tables.skel_table),
        guide: guide_records,
        ncx: ncx.as_ref().map(|n| n.records.clone()).unwrap_or_default(),
    };

    // apply TBS
    let has_tbs = ncx.is_some();
    if let Some(n) = &ncx {
        text_records.records.par_iter_mut().zip(n.tbs.par_iter()).for_each(|(r, t)| {
            r.0.extend_from_slice(&mobi::encode_trailing_data(t));
        });
    }

    let text_record_count = text_records.records.len();
    let layout = assemble_records(text_records, indices, &res, &used, fdst, text_length)?;

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let asin = book.meta.uuid.clone().unwrap_or_else(|| pseudo_uuid(&book.meta.title, &book.meta.authors));
    let low32 = |v: u64| u32::try_from(v & 0xFFFF_FFFF);
    let header = HeaderInput {
        book,
        opts,
        res: &res,
        asin: &asin,
        start_offset,
        has_tbs,
        flow_count: flows.len(),
        text_length,
        text_record_count,
        uid: low32(fxhash(asin.as_bytes()) ^ now)?,
    };
    let header_record = build_header(&header, &layout)?;
    let mut records = layout.records;
    records[0] = header_record;

    let data = mobi::write_pdb(&book.meta.title, &records, low32(now)?);
    Ok(Output { data, asin, library_thumb: res.library_thumb, warnings, title: book.meta.title.clone() })
}

fn fxhash(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= u64::from(x);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn pseudo_uuid(title: &str, authors: &[String]) -> String {
    let h1 = fxhash(title.as_bytes());
    let h2 = fxhash(authors.join(",").as_bytes()) ^ h1.rotate_left(17);
    let s = format!("{h1:016x}{h2:016x}");
    format!("{}-{}-{}-{}-{}", &s[0..8], &s[8..12], &s[12..16], &s[16..20], &s[20..32])
}
