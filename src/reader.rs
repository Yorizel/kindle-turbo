//! A small, independent KF8 (AZW3) reader.
//!
//! It decodes everything the writer produces (`PalmDB`, record 0, EXTH,
//! trailing entries, `PalmDOC`, FDST, INDX/TAGX/CNCX, skeleton + fragments)
//! and rebuilds the XHTML files. Used by `--check` and by the test-suite to
//! verify real output end to end.

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

/// Reads the TAGX block of an INDX header: the tag definitions and the
/// number of control bytes per entry.
fn read_tagx(header: &[u8]) -> Result<(Vec<[u8; 4]>, usize)> {
    let tagx = u32_at(header, 180)?;
    ensure!(header.get(tagx..tagx + 4) == Some(b"TAGX"), "TAGX magic");
    let table_len = u32_at(header, tagx + 4)?;
    let control_len = u32_at(header, tagx + 8)?;
    let table_end = tagx + table_len;
    let defs = (tagx + 12..)
        .step_by(4)
        .take_while(|&pos| pos + 4 <= table_end)
        .map(|pos| header.get(pos..pos + 4).and_then(|raw| <[u8; 4]>::try_from(raw).ok()).ok_or_else(|| anyhow!("TAGX")))
        .collect::<Result<_>>()?;
    Ok((defs, control_len))
}

/// Concatenates the CNCX records (each padded to 64 KiB) that follow an index.
fn read_cncx(records: &[Vec<u8>], start: usize, count: usize) -> Result<Vec<u8>> {
    let mut cncx = Vec::new();
    for offset in 0..count {
        let record = records.get(start + offset).ok_or_else(|| anyhow!("missing CNCX"))?;
        let mut padded = record.clone();
        padded.resize(0x10000, 0);
        cncx.extend_from_slice(&padded);
    }
    Ok(cncx)
}

/// Decodes one tag's values from the entry at `offset` inside an INDX data record.
fn read_index_entry(record: &[u8], offset: usize, tag_defs: &[[u8; 4]], control_len: usize) -> Result<Entry> {
    let name_len = usize::from(*record.get(offset).ok_or_else(|| anyhow!("entry"))?);
    let name_end = offset + 1 + name_len;
    let name = record.get(offset + 1..name_end).ok_or_else(|| anyhow!("entry name"))?.to_vec();
    let controls = record.get(name_end..name_end + control_len).ok_or_else(|| anyhow!("control bytes"))?;
    let mut pos = name_end + control_len;
    let mut tags: FxHashMap<u8, Vec<usize>> = FxHashMap::default();
    let mut control_index = 0usize;
    for &[tag, values_per_entry, mask, end] in tag_defs {
        if end == 1 {
            control_index += 1;
            continue;
        }
        let control_byte = *controls.get(control_index).ok_or_else(|| anyhow!("control byte"))?;
        let masked = control_byte & mask;
        if masked == 0 {
            continue;
        }
        let count = if masked == mask && mask.count_ones() > 1 {
            let (quantity, used) = decint(record.get(pos..).unwrap_or_default())?;
            pos += used;
            quantity * usize::from(values_per_entry)
        } else {
            usize::from(masked >> mask.trailing_zeros()) * usize::from(values_per_entry)
        };
        let mut decoded = Vec::with_capacity(count);
        for _ in 0..count {
            let (number, used) = decint(record.get(pos..).unwrap_or_default())?;
            pos += used;
            decoded.push(number);
        }
        tags.insert(tag, decoded);
    }
    Ok(Entry { name, tags })
}

/// Decodes every entry of one INDX data record.
fn read_index_record(record: &[u8], tag_defs: &[[u8; 4]], control_len: usize) -> Result<Vec<Entry>> {
    ensure!(record.starts_with(b"INDX"), "index record magic");
    let idxt = u32_at(record, 20)?;
    let count = u32_at(record, 24)?;
    ensure!(record.get(idxt..idxt + 4) == Some(b"IDXT"), "IDXT magic");
    (0..count)
        .map(|entry_index| {
            let offset = u16_at(record, idxt + 4 + 2 * entry_index)?;
            read_index_entry(record, offset, tag_defs, control_len)
        })
        .collect()
}

/// Reads an INDX index starting at record `first`, returning its entries and its CNCX strings.
fn read_index(records: &[Vec<u8>], first: usize) -> Result<(Vec<Entry>, Vec<u8>)> {
    let header = records.get(first).ok_or_else(|| anyhow!("missing index header"))?;
    ensure!(header.starts_with(b"INDX"), "index header magic");
    let record_count = u32_at(header, 24)?;
    let cncx_count = u32_at(header, 52)?;
    let (tag_defs, control_len) = read_tagx(header)?;
    let cncx = read_cncx(records, first + 1 + record_count, cncx_count)?;
    let mut entries = Vec::new();
    for data_index in 0..record_count {
        let record = records.get(first + 1 + data_index).ok_or_else(|| anyhow!("missing index record"))?;
        entries.extend(read_index_record(record, &tag_defs, control_len)?);
    }
    Ok((entries, cncx))
}

fn cncx_string(cncx: &[u8], off: usize) -> Result<String> {
    let (len, used) = decint(cncx.get(off..).unwrap_or_default())?;
    let s = cncx.get(off + used..off + used + len).ok_or_else(|| anyhow!("CNCX string out of range"))?;
    Ok(String::from_utf8_lossy(s).into_owned())
}

/// Splits the `PalmDB` container into its records.
fn split_records(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let count = u16_at(data, 76)?;
    let mut offsets = Vec::with_capacity(count + 1);
    for index in 0..count {
        offsets.push(u32_at(data, 78 + 8 * index)?);
    }
    offsets.push(data.len());
    (0..count)
        .map(|index| data.get(offsets[index]..offsets[index + 1]).map(<[u8]>::to_vec).ok_or_else(|| anyhow!("record {index} out of range")))
        .collect()
}

/// Reads the EXTH block that starts at `start` in record 0 (empty if absent).
fn read_exth(record: &[u8], start: usize) -> Result<FxHashMap<u32, Vec<Vec<u8>>>> {
    let mut exth: FxHashMap<u32, Vec<Vec<u8>>> = FxHashMap::default();
    if record.get(start..start + 4) != Some(b"EXTH") {
        return Ok(exth);
    }
    let count = u32_at(record, start + 8)?;
    let mut pos = start + 12;
    for _ in 0..count {
        let code = u32::try_from(u32_at(record, pos)?)?;
        let len = u32_at(record, pos + 4)?;
        ensure!(len >= 8, "bad EXTH record length");
        let value = record.get(pos + 8..pos + len).ok_or_else(|| anyhow!("EXTH out of range"))?.to_vec();
        exth.entry(code).or_default().push(value);
        pos += len;
    }
    Ok(exth)
}

/// Decompresses the text records (1..=`text_records`) into one buffer.
fn decode_text(records: &[Vec<u8>], text_records: usize, compression: usize, extra: usize, text_len: usize) -> Result<Vec<u8>> {
    let mut text = Vec::with_capacity(text_len);
    for index in 1..=text_records {
        let record = records.get(index).ok_or_else(|| anyhow!("missing text record {index}"))?;
        let body = strip_trailing(record, extra).with_context(|| format!("text record {index}"))?;
        if compression == 2 {
            text.extend(palmdoc::decompress(body).ok_or_else(|| anyhow!("bad PalmDOC in record {index}"))?);
        } else {
            text.extend_from_slice(body);
        }
    }
    ensure!(text.len() == text_len, "text length {} != header {}", text.len(), text_len);
    Ok(text)
}

/// Reads the FDST flow table, checking that it covers all `text_len` bytes.
fn read_flows(records: &[Vec<u8>], fdst: usize, text_len: usize) -> Result<Vec<(usize, usize)>> {
    let fdst_record = records.get(fdst).ok_or_else(|| anyhow!("missing FDST"))?;
    ensure!(fdst_record.starts_with(b"FDST"), "FDST magic");
    let count = u32_at(fdst_record, 8)?;
    let flows = (0..count)
        .map(|index| Ok((u32_at(fdst_record, 12 + 8 * index)?, u32_at(fdst_record, 16 + 8 * index)?)))
        .collect::<Result<Vec<(usize, usize)>>>()?;
    ensure!(flows.last().map(|flow| flow.1) == Some(text_len), "FDST does not cover the text");
    Ok(flows)
}

/// Rebuilds each XHTML file from its skeleton and the fragments inserted into it.
fn rebuild_files(text: &[u8], skels: &[Entry], chunks: &[Entry], chunk_cncx: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut files = Vec::with_capacity(skels.len());
    let mut chunk_cursor = 0usize;
    for skel in skels {
        let chunk_count = *skel.tags.get(&1).and_then(|v| v.first()).ok_or_else(|| anyhow!("skel chunk count"))?;
        let geometry = skel.tags.get(&6).ok_or_else(|| anyhow!("skel geometry"))?;
        let (start, len) = (geometry[0], geometry[1]);
        let mut html = text.get(start..start + len).ok_or_else(|| anyhow!("skeleton out of range"))?.to_vec();
        let mut base = start + len;
        for _ in 0..chunk_count {
            let chunk = chunks.get(chunk_cursor).ok_or_else(|| anyhow!("missing chunk {chunk_cursor}"))?;
            chunk_cursor += 1;
            let insert: usize = std::str::from_utf8(&chunk.name)?.parse()?;
            let chunk_geometry = chunk.tags.get(&6).ok_or_else(|| anyhow!("chunk geometry"))?;
            let frag = text.get(base..base + chunk_geometry[1]).ok_or_else(|| anyhow!("chunk out of range"))?;
            if let Some(selector) = chunk.tags.get(&2).and_then(|v| v.first()) {
                let selector = cncx_string(chunk_cncx, *selector)?;
                ensure!(selector.contains("//*[@aid="), "bad chunk selector {selector}");
            }
            let at = insert.checked_sub(start).ok_or_else(|| anyhow!("insert before file start"))?;
            ensure!(at <= html.len(), "insert position {at} beyond file length {}", html.len());
            html.splice(at..at, frag.iter().copied());
            base += chunk_geometry[1];
        }
        files.push(html);
    }
    ensure!(chunk_cursor == chunks.len(), "unused chunks");
    Ok(files)
}

/// Reads the NCX table of contents as (depth, label, offset) triples.
fn read_toc(records: &[Vec<u8>], ncx_index: usize) -> Result<Vec<(u32, String, usize)>> {
    if ncx_index == 0xFFFF_FFFF {
        return Ok(Vec::new());
    }
    let (entries, cncx) = read_index(records, ncx_index).context("NCX index")?;
    entries
        .into_iter()
        .map(|entry| -> Result<(u32, String, usize)> {
            let label = match entry.tags.get(&3).and_then(|v| v.first()) {
                Some(&label_offset) => cncx_string(&cncx, label_offset)?,
                None => String::new(),
            };
            let depth = entry.tags.get(&4).and_then(|v| v.first()).copied().unwrap_or(0);
            let offset = entry.tags.get(&1).and_then(|v| v.first()).copied().unwrap_or(0);
            Ok((u32::try_from(depth)?, label, offset))
        })
        .collect()
}

/// Parse and fully decode an AZW3 file.
///
/// # Errors
/// Any structural problem is reported with context.
pub fn read(data: &[u8]) -> Result<Azw3> {
    ensure!(data.get(60..68) == Some(b"BOOKMOBI"), "not a BOOKMOBI file");
    let records = split_records(data)?;
    let header = records.first().ok_or_else(|| anyhow!("no records"))?;
    let compression = u16_at(header, 0)?;
    let text_len = u32_at(header, 4)?;
    let text_records = u16_at(header, 8)?;
    ensure!(u16_at(header, 10)? == 4096, "record size must be 4096");
    ensure!(header.get(16..20) == Some(b"MOBI"), "MOBI magic");
    let hlen = u32_at(header, 20)?;
    ensure!(u32_at(header, 36)? == 8, "not KF8 (file version != 8)");
    let first_res = u32_at(header, 108)?;
    let fdst = u32_at(header, 192)?;
    let extra = u32_at(header, 240)?;
    let ncx_index = u32_at(header, 244)?;
    let chunk_index = u32_at(header, 248)?;
    let skel_index = u32_at(header, 252)?;
    let title_off = u32_at(header, 84)?;
    let title_len = u32_at(header, 88)?;
    let title = String::from_utf8_lossy(header.get(title_off..title_off + title_len).unwrap_or_default()).into_owned();
    let exth = read_exth(header, 16 + hlen)?;

    let text = decode_text(&records, text_records, compression, extra, text_len)?;
    let flows = read_flows(&records, fdst, text.len())?;

    // skeleton + fragments
    let (skels, _) = read_index(&records, skel_index).context("SKEL index")?;
    let (chunks, chunk_cncx) = read_index(&records, chunk_index).context("FRAG index")?;
    let files = rebuild_files(&text, &skels, &chunks, &chunk_cncx)?;
    let toc = read_toc(&records, ncx_index)?;

    Ok(Azw3 {
        title,
        exth,
        text,
        flows,
        files,
        toc,
        first_resource: (first_res != 0xFFFF_FFFF).then_some(first_res),
        text_records,
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

    /// Plain text of the book (tags stripped, entities decoded), for content comparisons.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for f in &self.files {
            let mut text = String::new();
            let s = String::from_utf8_lossy(f);
            let body = s.find("<body").map_or(&s[..], |i| &s[i..]);
            let mut in_tag = false;
            for c in body.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => {
                        in_tag = false;
                        text.push(' ');
                    }
                    _ if !in_tag => text.push(c),
                    _ => {}
                }
            }
            out.push_str(&crate::dom::decode_entities(&text));
        }
        out
    }
}
