//! A small, independent KF8 (AZW3) reader. It decodes everything the writer
//! produces (`PalmDB`, record 0, EXTH, trailing entries, `PalmDOC`, FDST,
//! INDX/TAGX/CNCX, skeleton + fragments) and rebuilds the XHTML files.
//! Used by `--check` and by the test-suite to verify real output end to end.

use crate::palmdoc;
use anyhow::{Context, Result, anyhow, bail, ensure};
use rustc_hash::FxHashMap;

/// A decoded AZW3 file.
#[derive(Debug)]
pub struct Azw3 {
    /// Raw records of the `PalmDB` container.
    pub records: Vec<Vec<u8>>,
    /// EXTH entries (code -> values).
    pub exth: FxHashMap<u32, Vec<Vec<u8>>>,
    /// Full title from record 0.
    pub title: String,
    /// Decompressed text of all flows, concatenated.
    pub text: Vec<u8>,
    /// Flow boundaries from FDST.
    pub flows: Vec<(usize, usize)>,
    /// Rebuilt XHTML files (skeleton with fragments inserted).
    pub files: Vec<Vec<u8>>,
    /// Table of contents: (depth, label, offset).
    pub toc: Vec<(u32, String, usize)>,
    /// Index of the first resource record (images), if any.
    pub first_resource: Option<usize>,
    /// Number of text records.
    pub text_records: usize,
}

fn u16_at(d: &[u8], o: usize) -> Result<usize> {
    let b = d.get(o..o + 2).ok_or_else(|| anyhow!("truncated at {o}"))?;
    Ok(usize::from(u16::from_be_bytes([b[0], b[1]])))
}

fn u32_at(d: &[u8], o: usize) -> Result<usize> {
    let b = d.get(o..o + 4).ok_or_else(|| anyhow!("truncated at {o}"))?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// Forward variable width integer. Returns (value, bytes consumed).
fn decint(d: &[u8]) -> Result<(usize, usize)> {
    let mut v = 0usize;
    for (i, &b) in d.iter().enumerate() {
        v = (v << 7) | usize::from(b & 0x7f);
        if b & 0x80 != 0 {
            return Ok((v, i + 1));
        }
    }
    bail!("unterminated vwi")
}

/// Backward variable width integer at the end of `d`. Returns the value.
fn decint_back(d: &[u8]) -> usize {
    let mut v = 0usize;
    let mut shift = 0;
    for &b in d.iter().rev().take(4) {
        v |= usize::from(b & 0x7f) << shift;
        shift += 7;
        if b & 0x80 != 0 {
            break;
        }
    }
    v
}

fn strip_trailing(rec: &[u8], flags: usize) -> Result<&[u8]> {
    let mut r = rec;
    let mut f = flags >> 1;
    while f != 0 {
        if f & 1 == 1 {
            let sz = decint_back(r);
            ensure!(sz <= r.len() && sz > 0, "bad trailing entry");
            r = &r[..r.len() - sz];
        }
        f >>= 1;
    }
    if flags & 1 == 1 {
        let last = *r.last().ok_or_else(|| anyhow!("empty record"))?;
        let sz = usize::from(last & 0b11) + 1;
        ensure!(sz <= r.len(), "bad multibyte trailer");
        r = &r[..r.len() - sz];
    }
    Ok(r)
}

/// One decoded index entry.
#[derive(Debug)]
struct Entry {
    name: Vec<u8>,
    tags: FxHashMap<u8, Vec<usize>>,
}

fn read_index(records: &[Vec<u8>], first: usize) -> Result<(Vec<Entry>, Vec<u8>)> {
    let h = records.get(first).ok_or_else(|| anyhow!("missing index header"))?;
    ensure!(h.starts_with(b"INDX"), "index header magic");
    let nrec = u32_at(h, 24)?;
    let ncncx = u32_at(h, 52)?;
    let tagx = u32_at(h, 180)?;
    ensure!(h.get(tagx..tagx + 4) == Some(b"TAGX"), "TAGX magic");
    let tlen = u32_at(h, tagx + 4)?;
    let cbc = u32_at(h, tagx + 8)?;
    let mut tags: Vec<[u8; 4]> = Vec::new();
    let mut p = tagx + 12;
    while p + 4 <= tagx + tlen {
        let t = h.get(p..p + 4).ok_or_else(|| anyhow!("TAGX"))?;
        tags.push([t[0], t[1], t[2], t[3]]);
        p += 4;
    }
    let mut cncx = Vec::new();
    for i in 0..ncncx {
        let r = records.get(first + 1 + nrec + i).ok_or_else(|| anyhow!("missing CNCX"))?;
        let mut padded = r.clone();
        padded.resize(0x10000, 0);
        cncx.extend_from_slice(&padded);
    }
    let mut entries = Vec::new();
    for ri in 0..nrec {
        let r = records.get(first + 1 + ri).ok_or_else(|| anyhow!("missing index record"))?;
        ensure!(r.starts_with(b"INDX"), "index record magic");
        let idxt = u32_at(r, 20)?;
        let count = u32_at(r, 24)?;
        ensure!(r.get(idxt..idxt + 4) == Some(b"IDXT"), "IDXT magic");
        for e in 0..count {
            let off = u16_at(r, idxt + 4 + 2 * e)?;
            let nlen = usize::from(*r.get(off).ok_or_else(|| anyhow!("entry"))?);
            let name = r.get(off + 1..off + 1 + nlen).ok_or_else(|| anyhow!("entry name"))?.to_vec();
            let cb = r.get(off + 1 + nlen..off + 1 + nlen + cbc).ok_or_else(|| anyhow!("control bytes"))?;
            let mut pos = off + 1 + nlen + cbc;
            let mut vals: FxHashMap<u8, Vec<usize>> = FxHashMap::default();
            let mut cbi = 0usize;
            for &[num, vpe, mask, end] in &tags {
                if end == 1 {
                    cbi += 1;
                    continue;
                }
                let c = *cb.get(cbi).ok_or_else(|| anyhow!("control byte"))?;
                let value = c & mask;
                if value == 0 {
                    continue;
                }
                let n = if value == mask && mask.count_ones() > 1 {
                    let (v, used) = decint(r.get(pos..).unwrap_or_default())?;
                    pos += used;
                    v * usize::from(vpe)
                } else {
                    usize::from(value >> mask.trailing_zeros()) * usize::from(vpe)
                };
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    let (x, used) = decint(r.get(pos..).unwrap_or_default())?;
                    pos += used;
                    v.push(x);
                }
                vals.insert(num, v);
            }
            entries.push(Entry { name, tags: vals });
        }
    }
    Ok((entries, cncx))
}

fn cncx_string(cncx: &[u8], off: usize) -> Result<String> {
    let (len, used) = decint(cncx.get(off..).unwrap_or_default())?;
    let s = cncx.get(off + used..off + used + len).ok_or_else(|| anyhow!("CNCX string out of range"))?;
    Ok(String::from_utf8_lossy(s).into_owned())
}

/// Parse and fully decode an AZW3 file.
///
/// # Errors
/// Any structural problem is reported with context.
pub fn read(data: &[u8]) -> Result<Azw3> {
    ensure!(data.get(60..68) == Some(b"BOOKMOBI"), "not a BOOKMOBI file");
    let n = u16_at(data, 76)?;
    let mut offs = Vec::with_capacity(n + 1);
    for i in 0..n {
        offs.push(u32_at(data, 78 + 8 * i)?);
    }
    offs.push(data.len());
    let records: Vec<Vec<u8>> = (0..n)
        .map(|i| data.get(offs[i]..offs[i + 1]).map(<[u8]>::to_vec).ok_or_else(|| anyhow!("record {i} out of range")))
        .collect::<Result<_>>()?;
    let r0 = records.first().ok_or_else(|| anyhow!("no records"))?;
    let compression = u16_at(r0, 0)?;
    let text_len = u32_at(r0, 4)?;
    let ntext = u16_at(r0, 8)?;
    ensure!(u16_at(r0, 10)? == 4096, "record size must be 4096");
    ensure!(r0.get(16..20) == Some(b"MOBI"), "MOBI magic");
    let hlen = u32_at(r0, 20)?;
    ensure!(u32_at(r0, 36)? == 8, "not KF8 (file version != 8)");
    let first_res = u32_at(r0, 108)?;
    let fdst = u32_at(r0, 192)?;
    let extra = u32_at(r0, 240)?;
    let ncx_index = u32_at(r0, 244)?;
    let chunk_index = u32_at(r0, 248)?;
    let skel_index = u32_at(r0, 252)?;
    let title_off = u32_at(r0, 84)?;
    let title_len = u32_at(r0, 88)?;
    let title = String::from_utf8_lossy(r0.get(title_off..title_off + title_len).unwrap_or_default()).into_owned();

    let mut exth: FxHashMap<u32, Vec<Vec<u8>>> = FxHashMap::default();
    let e = 16 + hlen;
    if r0.get(e..e + 4) == Some(b"EXTH") {
        let count = u32_at(r0, e + 8)?;
        let mut p = e + 12;
        for _ in 0..count {
            let code = u32::try_from(u32_at(r0, p)?)?;
            let len = u32_at(r0, p + 4)?;
            ensure!(len >= 8, "bad EXTH record length");
            exth.entry(code).or_default().push(r0.get(p + 8..p + len).ok_or_else(|| anyhow!("EXTH out of range"))?.to_vec());
            p += len;
        }
    }

    let mut text = Vec::with_capacity(text_len);
    for i in 1..=ntext {
        let rec = records.get(i).ok_or_else(|| anyhow!("missing text record {i}"))?;
        let body = strip_trailing(rec, extra).with_context(|| format!("text record {i}"))?;
        if compression == 2 {
            text.extend(palmdoc::decompress(body).ok_or_else(|| anyhow!("bad PalmDOC in record {i}"))?);
        } else {
            text.extend_from_slice(body);
        }
    }
    ensure!(text.len() == text_len, "text length {} != header {}", text.len(), text_len);

    let fd = records.get(fdst).ok_or_else(|| anyhow!("missing FDST"))?;
    ensure!(fd.starts_with(b"FDST"), "FDST magic");
    let nflows = u32_at(fd, 8)?;
    let flows: Vec<(usize, usize)> = (0..nflows).map(|i| Ok((u32_at(fd, 12 + 8 * i)?, u32_at(fd, 16 + 8 * i)?))).collect::<Result<_>>()?;
    ensure!(flows.last().map(|f| f.1) == Some(text.len()), "FDST does not cover the text");

    // skeleton + fragments
    let (skels, _) = read_index(&records, skel_index).context("SKEL index")?;
    let (chunks, chunk_cncx) = read_index(&records, chunk_index).context("FRAG index")?;
    let mut files = Vec::with_capacity(skels.len());
    let mut ci = 0usize;
    for s in &skels {
        let cc = *s.tags.get(&1).and_then(|v| v.first()).ok_or_else(|| anyhow!("skel chunk count"))?;
        let g = s.tags.get(&6).ok_or_else(|| anyhow!("skel geometry"))?;
        let (start, len) = (g[0], g[1]);
        let mut html = text.get(start..start + len).ok_or_else(|| anyhow!("skeleton out of range"))?.to_vec();
        let mut base = start + len;
        for _ in 0..cc {
            let c = chunks.get(ci).ok_or_else(|| anyhow!("missing chunk {ci}"))?;
            ci += 1;
            let insert: usize = std::str::from_utf8(&c.name)?.parse()?;
            let cg = c.tags.get(&6).ok_or_else(|| anyhow!("chunk geometry"))?;
            let frag = text.get(base..base + cg[1]).ok_or_else(|| anyhow!("chunk out of range"))?;
            if let Some(sel) = c.tags.get(&2).and_then(|v| v.first()) {
                let sel = cncx_string(&chunk_cncx, *sel)?;
                ensure!(sel.contains("//*[@aid="), "bad chunk selector {sel}");
            }
            let at = insert.checked_sub(start).ok_or_else(|| anyhow!("insert before file start"))?;
            ensure!(at <= html.len(), "insert position {at} beyond file length {}", html.len());
            html.splice(at..at, frag.iter().copied());
            base += cg[1];
        }
        files.push(html);
    }
    ensure!(ci == chunks.len(), "unused chunks");

    let mut toc = Vec::new();
    if ncx_index != 0xFFFF_FFFF {
        let (entries, cncx) = read_index(&records, ncx_index).context("NCX index")?;
        for e in entries {
            let label = e.tags.get(&3).and_then(|v| v.first()).map_or_else(|| Ok(String::new()), |&o| cncx_string(&cncx, o))?;
            let depth = e.tags.get(&4).and_then(|v| v.first()).copied().unwrap_or(0);
            let off = e.tags.get(&1).and_then(|v| v.first()).copied().unwrap_or(0);
            toc.push((u32::try_from(depth)?, label, off));
            let _ = e.name;
        }
    }
    Ok(Azw3 {
        title,
        exth,
        text,
        flows,
        files,
        toc,
        first_resource: (first_res != 0xFFFF_FFFF).then_some(first_res),
        text_records: ntext,
        records,
    })
}

impl Azw3 {
    /// First value of an EXTH record as UTF-8.
    #[must_use]
    pub fn exth_str(&self, code: u32) -> Option<String> {
        self.exth.get(&code).and_then(|v| v.first()).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// First value of a 4-byte numeric EXTH record.
    #[must_use]
    pub fn exth_u32(&self, code: u32) -> Option<u32> {
        let b = self.exth.get(&code)?.first()?;
        Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
    }

    /// Resource record (image) by 0-based resource index.
    #[must_use]
    pub fn resource(&self, i: usize) -> Option<&[u8]> {
        self.records.get(self.first_resource? + i).map(Vec::as_slice)
    }

    /// Plain text of the book (tags stripped), for content comparisons.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for f in &self.files {
            let s = String::from_utf8_lossy(f);
            let body = s.find("<body").map_or(&s[..], |i| &s[i..]);
            let mut in_tag = false;
            for c in body.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => {
                        in_tag = false;
                        out.push(' ');
                    }
                    _ if !in_tag => out.push(c),
                    _ => {}
                }
            }
        }
        out
    }
}
