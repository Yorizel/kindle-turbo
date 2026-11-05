//! Low-level MOBI/KF8 building blocks: variable width ints, CNCX, INDX records,
//! trailing byte sequences, EXTH and record 0. Ported from calibre's writer8.

use std::collections::HashMap;

pub const RECORD_SIZE: usize = 4096;
pub const NULL: u32 = 0xFFFF_FFFF;

/// Saturating `usize` -> `u32`; identical to a plain cast for every in-range value.
fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Saturating `usize` -> `u16`; identical to a plain cast for every in-range value.
fn u16_of(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// Saturating `usize` -> `u8`; identical to a plain cast for every in-range value.
fn u8_of(n: usize) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}

/// Formats `num` in `base` (clamped to 2..=36), left padded with zeros.
#[must_use]
pub fn to_base(mut num: u64, base: u64, min_digits: usize) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let base = base.clamp(2, 36);
    let mut v = Vec::new();
    while num > 0 {
        let digit = usize::try_from(num % base).unwrap_or(0);
        v.push(DIGITS.get(digit).copied().unwrap_or(b'0'));
        num /= base;
    }
    if v.is_empty() && min_digits == 0 {
        v.push(b'0');
    }
    while v.len() < min_digits {
        v.push(b'0');
    }
    v.iter().rev().map(|&b| char::from(b)).collect()
}

/// kindle:embed / kindle:flow reference numbers.
#[must_use]
pub fn to_ref(n: usize) -> String {
    to_base(n as u64, 32, 4)
}

#[must_use]
pub fn encint(mut value: u64, forward: bool) -> Vec<u8> {
    let mut b = Vec::new();
    loop {
        b.push((value & 0x7F) as u8);
        value >>= 7;
        if value == 0 {
            break;
        }
    }
    let marked = if forward { b.first_mut() } else { b.last_mut() };
    if let Some(byte) = marked {
        *byte |= 0x80;
    }
    b.reverse();
    b
}

#[must_use]
pub fn encode_trailing_data(raw: &[u8]) -> Vec<u8> {
    let mut lsize = 1;
    loop {
        let enc = encint((raw.len() + lsize) as u64, false);
        if enc.len() == lsize {
            let mut out = raw.to_vec();
            out.extend_from_slice(&enc);
            return out;
        }
        lsize += 1;
    }
}

#[must_use]
pub fn align4(mut v: Vec<u8>) -> Vec<u8> {
    let extra = v.len() % 4;
    if extra != 0 {
        v.resize(v.len() + 4 - extra, 0);
    }
    v
}

#[must_use]
pub fn utf8_text(s: &str) -> Vec<u8> {
    let t = s.trim();
    if t.is_empty() { b"Unknown".to_vec() } else { t.as_bytes().to_vec() }
}

#[must_use]
pub const fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

// ---------------------------------------------------------------- CNCX

#[derive(Debug)]
pub struct Cncx {
    pub offsets: HashMap<String, u64>,
    pub records: Vec<Vec<u8>>,
}

impl Cncx {
    pub fn new<'a>(strings: impl IntoIterator<Item = &'a str>) -> Self {
        const LIMIT: usize = 0x10000 - 1024;
        let mut offsets: HashMap<String, u64> = HashMap::new();
        let mut records = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        let mut offset: u64 = 0;
        for s in strings {
            if offsets.contains_key(s) {
                continue;
            }
            let truncated: String = s.chars().take(500).collect();
            let utf8 = utf8_text(&truncated);
            let mut raw = encint(utf8.len() as u64, true);
            raw.extend_from_slice(&utf8);
            if buf.len() + raw.len() > LIMIT {
                records.push(align4(std::mem::take(&mut buf)));
                offset = records.len() as u64 * 0x10000;
            }
            buf.extend_from_slice(&raw);
            offsets.insert(s.to_owned(), offset);
            offset += raw.len() as u64;
        }
        if !buf.is_empty() {
            records.push(align4(buf));
        }
        Self { offsets, records }
    }
    #[must_use]
    pub fn empty() -> Self {
        Self { offsets: HashMap::new(), records: Vec::new() }
    }
    #[must_use]
    pub fn get(&self, s: &str) -> u64 {
        self.offsets[s]
    }
}

// ---------------------------------------------------------------- INDX

#[derive(Clone, Copy, Debug)]
pub struct TagMeta {
    pub name: &'static str,
    pub number: u8,
    pub vpe: u8,
    pub mask: u8,
    pub end: u8,
}

#[must_use]
pub const fn tm(name: &'static str, number: u8, vpe: u8, mask: u8) -> TagMeta {
    TagMeta { name, number, vpe, mask, end: 0 }
}
pub const END: TagMeta = TagMeta { name: "eof", number: 0, vpe: 0, mask: 0, end: 1 };

#[derive(Debug)]
pub struct IndexEntry {
    pub name: Vec<u8>,
    pub tags: Vec<(&'static str, Vec<u64>)>,
}

impl IndexEntry {
    fn get(&self, name: &str) -> Option<&Vec<u64>> {
        self.tags.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }
}

/// Bit position of the lowest set bit of a tag mask (1/3 -> 0, 4/12 -> 2, ...).
const fn mask_shift(mask: u8) -> u32 {
    mask.trailing_zeros()
}

const INDX_HEADER_LENGTH: usize = 192;

/// One output INDX data record under construction.
#[derive(Default)]
struct Block {
    index: Vec<u8>,
    idxt: Vec<u8>,
    count: u16,
    last: Vec<u8>,
}

fn control_bytes(tag_types: &[TagMeta], e: &IndexEntry) -> Vec<u8> {
    let mut cbs = Vec::new();
    let mut ans: u32 = 0;
    for t in tag_types {
        if t.end == 1 {
            cbs.push(ans.to_le_bytes()[0]);
            ans = 0;
            continue;
        }
        let nvals = u32_of(e.get(t.name).map_or(0, Vec::len));
        let nentries = nvals / u32::from(t.vpe.max(1));
        ans |= u32::from(t.mask) & (nentries << mask_shift(t.mask));
    }
    cbs
}

fn split_blocks(tag_types: &[TagMeta], entries: &[IndexEntry]) -> Vec<Block> {
    let record_limit = 0x10000 - INDX_HEADER_LENGTH - 1048;
    let mut done: Vec<Block> = Vec::new();
    let mut cur = Block::default();
    for e in entries {
        let mut raw = Vec::with_capacity(32);
        raw.push(u8_of(e.name.len()));
        raw.extend_from_slice(&e.name);
        raw.extend_from_slice(&control_bytes(tag_types, e));
        for t in tag_types {
            if let Some(vals) = e.get(t.name) {
                for &v in vals {
                    raw.extend_from_slice(&encint(v, true));
                }
            }
        }
        let mut offset = cur.index.len();
        if offset + cur.idxt.len() + raw.len() + 2 > record_limit {
            done.push(std::mem::take(&mut cur));
            offset = 0;
        }
        cur.count += 1;
        cur.idxt.extend_from_slice(&u16_of(INDX_HEADER_LENGTH + offset).to_be_bytes());
        cur.index.extend_from_slice(&raw);
        cur.last.clone_from(&e.name);
    }
    done.push(cur);
    done
}

fn data_records(blocks: &[Block]) -> Vec<Vec<u8>> {
    blocks
        .iter()
        .map(|blk| {
            let ib = align4(blk.index.clone());
            let mut idxt = b"IDXT".to_vec();
            idxt.extend_from_slice(&blk.idxt);
            let idxt = align4(idxt);
            let mut rec = b"INDX".to_vec();
            rec.extend_from_slice(&be32(u32_of(INDX_HEADER_LENGTH)));
            rec.extend_from_slice(&[0; 4]);
            rec.extend_from_slice(&be32(1));
            rec.extend_from_slice(&[0; 4]);
            rec.extend_from_slice(&be32(u32_of(INDX_HEADER_LENGTH + ib.len())));
            rec.extend_from_slice(&be32(u32::from(blk.count)));
            rec.extend_from_slice(&[0xFF; 8]);
            rec.extend_from_slice(&[0; 156]);
            rec.extend_from_slice(&ib);
            rec.extend_from_slice(&idxt);
            debug_assert!(rec.len() <= 0x10000, "index record overflow");
            rec
        })
        .collect()
}

fn header_record(tag_types: &[TagMeta], control_byte_count: usize, blocks: &[Block], record_count: usize, cncx_records: usize) -> Vec<u8> {
    // TAGX
    let mut tagx = b"TAGX".to_vec();
    let body: Vec<u8> = tag_types.iter().flat_map(|t| [t.number, t.vpe, t.mask, t.end]).collect();
    tagx.extend_from_slice(&be32(u32_of(12 + body.len())));
    tagx.extend_from_slice(&be32(u32_of(control_byte_count)));
    tagx.extend_from_slice(&body);

    // geometry + IDXT of header
    let mut geometry = Vec::new();
    let mut idxt = b"IDXT".to_vec();
    let mut pos = INDX_HEADER_LENGTH + tagx.len();
    for blk in blocks {
        let start = geometry.len();
        idxt.extend_from_slice(&u16_of(pos).to_be_bytes());
        geometry.push(u8_of(blk.last.len()));
        geometry.extend_from_slice(&blk.last);
        geometry.extend_from_slice(&blk.count.to_be_bytes());
        pos += geometry.len() - start;
    }
    let tagx = align4(tagx);
    let geometry = align4(geometry);
    let idxt = align4(idxt);

    let num_entries: u32 = blocks.iter().map(|b| u32::from(b.count)).sum();
    let mut h = b"INDX".to_vec();
    h.extend_from_slice(&be32(u32_of(INDX_HEADER_LENGTH)));
    h.extend_from_slice(&[0; 8]);
    h.extend_from_slice(&be32(2));
    let idxt_offset = INDX_HEADER_LENGTH + tagx.len() + geometry.len();
    h.extend_from_slice(&be32(u32_of(idxt_offset)));
    h.extend_from_slice(&be32(u32_of(record_count)));
    h.extend_from_slice(&be32(65001));
    h.extend_from_slice(&be32(NULL));
    h.extend_from_slice(&be32(num_entries));
    h.extend_from_slice(&[0; 12]); // ordt, ligt, num ordt
    h.extend_from_slice(&be32(u32_of(cncx_records)));
    h.extend_from_slice(&[0; 124]);
    h.extend_from_slice(&be32(u32_of(INDX_HEADER_LENGTH)));
    h.extend_from_slice(&[0; 8]);
    debug_assert_eq!(h.len(), INDX_HEADER_LENGTH);
    h.extend_from_slice(&tagx);
    h.extend_from_slice(&geometry);
    h.extend_from_slice(&idxt);
    align4(h)
}

#[must_use]
pub fn build_index(tag_types: &[TagMeta], control_byte_count: usize, entries: &[IndexEntry], cncx: &Cncx) -> Vec<Vec<u8>> {
    let blocks = split_blocks(tag_types, entries);
    let index_records = data_records(&blocks);
    let h = header_record(tag_types, control_byte_count, &blocks, index_records.len(), cncx.records.len());
    let mut out = vec![h];
    out.extend(index_records);
    out.extend(cncx.records.iter().cloned());
    out
}

// ---------------------------------------------------------------- TBS

#[derive(Clone, Debug)]
pub struct NcxEntry {
    pub index: i64,
    pub offset: i64,
    pub length: i64,
    pub depth: i64,
    pub parent: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Spans,
    Ends,
    Starts,
    Completes,
}

#[derive(Clone, Debug)]
struct TEntry {
    index: i64,
    depth: i64,
    parent: Option<i64>,
    action: Action,
}

fn populate_strand(parent: &TEntry, entries: &mut Vec<TEntry>) -> Vec<TEntry> {
    let mut ans = vec![parent.clone()];
    if let Some(pos) = entries.iter().position(|c| c.parent == Some(parent.index)) {
        let child = entries.remove(pos);
        ans.extend(populate_strand(&child, entries));
    } else {
        let mut current = parent.index;
        let mut siblings = Vec::new();
        let snapshot: Vec<TEntry> = entries.clone();
        for entry in snapshot {
            if entry.depth == parent.depth && entry.parent == parent.parent && entry.index == current + 1 {
                current += 1;
                if let Some(p) = entries.iter().position(|e| e.index == entry.index) {
                    entries.remove(p);
                }
                if entries.iter().any(|c| c.parent == Some(entry.index)) {
                    siblings.extend(populate_strand(&entry, entries));
                    break;
                }
                siblings.push(entry);
            }
        }
        ans.extend(siblings);
    }
    ans
}

type Strand = Vec<(i64, Vec<TEntry>)>;

fn separate_strands(mut entries: Vec<TEntry>) -> Vec<Strand> {
    let mut ans = Vec::new();
    while !entries.is_empty() {
        let top = entries.remove(0);
        let strand = populate_strand(&top, &mut entries);
        let mut layers: Strand = Vec::new();
        for e in strand {
            match layers.iter_mut().find(|(d, _)| *d == e.depth) {
                Some((_, v)) => v.push(e),
                None => layers.push((e.depth, vec![e])),
            }
        }
        ans.push(layers);
    }
    ans
}

#[derive(Default, Clone)]
struct Extra {
    f1: Option<u64>,
    f2: Option<u64>,
    f4: Option<u8>,
    f8: bool,
}

struct Negative;

fn encode_strands(strands: &[Strand], tbs_type: u64) -> Result<Vec<(u64, Extra)>, Negative> {
    let mut ans: Vec<(u64, Extra)> = Vec::new();
    let mut last_index: i64 = 0;
    let first_index = strands.first().and_then(|s| s.first()).and_then(|(_, v)| v.first()).map(|e| e.index);
    for strand in strands {
        let mut seqs: Vec<(u64, Extra)> = Vec::new();
        for (_, entries) in strand {
            let mut extra = Extra::default();
            let (Some(first), Some(last)) = (entries.first(), entries.last()) else {
                continue;
            };
            if last.action == Action::Spans {
                extra.f1 = Some(0);
            }
            if Some(first.index) == first_index {
                extra.f2 = Some(tbs_type);
            }
            if entries.len() > 1 {
                extra.f4 = Some(u8_of(entries.len()));
            }
            let mut index = first.index - first.parent.unwrap_or(0);
            if !ans.is_empty() && seqs.is_empty() {
                index = last_index - first.index;
                if index < 0 {
                    if tbs_type == 5 {
                        index = -index;
                    } else {
                        return Err(Negative);
                    }
                } else {
                    extra.f8 = true;
                }
            }
            last_index = last.index;
            seqs.push((index.cast_unsigned(), extra));
        }
        for i in 0..seqs.len().saturating_sub(1) {
            if seqs[i].1.f1.is_some() && seqs[i + 1].1.f1.is_some() {
                seqs[i].1.f1 = None;
            }
        }
        ans.extend(seqs);
    }
    Ok(ans)
}

fn encode_tbs(val: u64, e: &Extra, flag_size: u32) -> Vec<u8> {
    let flags: u64 =
        u64::from(e.f1.is_some()) | (u64::from(e.f2.is_some()) << 1) | (u64::from(e.f4.is_some()) << 2) | (u64::from(e.f8) << 3);
    let ans = (val << flag_size) | (flags & ((1 << flag_size) - 1));
    let mut out = encint(ans, true);
    if let Some(v) = e.f2 {
        out.extend_from_slice(&encint(v, true));
    }
    if let Some(v) = e.f4 {
        out.push(v);
    }
    if let Some(v) = e.f1 {
        out.extend_from_slice(&encint(v, true));
    }
    out
}

/// Returns the TBS bytes for every text record.
#[must_use]
pub fn trailing_byte_sequences(entries: &[NcxEntry], record_lengths: &[usize]) -> Vec<Vec<u8>> {
    let mut sorted: Vec<&NcxEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.offset);
    let mut per_record: Vec<Vec<Strand>> = Vec::with_capacity(record_lengths.len());
    let mut record_start: i64 = 0;
    let mut first_live = 0usize;
    for &rl in record_lengths {
        let rl = i64::try_from(rl).unwrap_or(i64::MAX);
        let next = record_start + rl;
        let mut local = Vec::new();
        // entries ending before this record can never touch later records either,
        // but they may be interleaved with live ones, so only skip a dead prefix.
        while first_live < sorted.len() && sorted[first_live].offset + sorted[first_live].length <= record_start {
            first_live += 1;
        }
        for e in &sorted[first_live..] {
            if e.offset >= next {
                break;
            }
            if e.offset + e.length <= record_start {
                continue;
            }
            let start_offset = e.offset - record_start;
            let length_offset = start_offset + e.length;
            let action = if start_offset < 0 {
                if length_offset > rl { Action::Spans } else { Action::Ends }
            } else if length_offset > rl {
                Action::Starts
            } else {
                Action::Completes
            };
            local.push(TEntry { index: e.index, depth: e.depth, parent: e.parent, action });
        }
        per_record.push(separate_strands(local));
        record_start = next;
    }
    let calc = |t: u64| -> Result<Vec<Vec<u8>>, Negative> {
        per_record
            .iter()
            .map(|strands| {
                let seqs = encode_strands(strands, t)?;
                let mut out = Vec::new();
                for (i, (v, e)) in seqs.iter().enumerate() {
                    out.extend(encode_tbs(*v, e, if i == 0 { 3 } else { 4 }));
                }
                Ok(out)
            })
            .collect()
    };
    match calc(8) {
        Ok(v) => v,
        Err(Negative) => calc(5).unwrap_or_else(|_| vec![Vec::new(); record_lengths.len()]),
    }
}

// ---------------------------------------------------------------- EXTH / header

#[derive(Debug)]
pub struct ExthInput<'a> {
    pub title: &'a str,
    pub authors: &'a [String],
    pub publisher: Option<&'a str>,
    pub description: Option<&'a str>,
    pub subjects: &'a [String],
    pub isbn: Option<&'a str>,
    pub language: &'a str,
    pub asin: &'a str,
    pub pubdate: &'a str,
    pub cover_offset: Option<u32>,
    pub thumbnail_offset: Option<u32>,
    pub start_offset: Option<u32>,
    pub num_of_resources: u32,
    pub has_resources: bool,
}

#[must_use]
pub fn build_exth(m: &ExthInput<'_>) -> Vec<u8> {
    let mut b: Vec<u8> = Vec::new();
    let mut n = 0u32;
    let mut s = |b: &mut Vec<u8>, code: u32, data: &[u8]| {
        b.extend_from_slice(&be32(code));
        b.extend_from_slice(&be32(u32_of(data.len()).saturating_add(8)));
        b.extend_from_slice(data);
        n += 1;
    };
    let collapse = |x: &str| x.split_whitespace().collect::<Vec<_>>().join(" ");
    for a in m.authors {
        s(&mut b, 100, &utf8_text(&collapse(a)));
    }
    if let Some(p) = m.publisher.filter(|p| !p.trim().is_empty()) {
        s(&mut b, 101, &utf8_text(&collapse(p)));
    }
    if let Some(d) = m.description.filter(|d| !d.trim().is_empty()) {
        s(&mut b, 103, &utf8_text(d));
    }
    if let Some(i) = m.isbn {
        s(&mut b, 104, &utf8_text(i));
    }
    for sub in m.subjects.iter().filter(|x| !x.trim().is_empty()) {
        s(&mut b, 105, &utf8_text(&collapse(sub)));
    }
    s(&mut b, 503, &utf8_text(&collapse(m.title)));
    let lang = m.language.split(['-', '_']).next().unwrap_or("en").to_lowercase();
    s(&mut b, 524, &utf8_text(&lang));
    s(&mut b, 113, m.asin.as_bytes());
    s(&mut b, 112, format!("calibre:{}", m.asin).as_bytes());
    s(&mut b, 501, b"EBOK");
    s(&mut b, 106, m.pubdate.as_bytes());
    for (code, val) in [(204u32, 201u32), (205, 2), (206, 9), (207, 0)] {
        s(&mut b, code, &be32(val));
    }
    s(&mut b, 535, b"0730-890adc2");
    if let Some(c) = m.cover_offset {
        s(&mut b, 201, &be32(c));
        s(&mut b, 203, &be32(0));
    }
    if let Some(t) = m.thumbnail_offset {
        s(&mut b, 202, &be32(t));
        s(&mut b, 129, format!("kindle:embed:{}", to_base(u64::from(t), 32, 4)).as_bytes());
    }
    if let Some(so) = m.start_offset {
        s(&mut b, 116, &be32(so));
    }
    s(&mut b, 125, &be32(m.num_of_resources));
    if m.has_resources {
        s(&mut b, 131, &be32(0));
    }
    s(&mut b, 528, b"true");

    let pad = 4 - (b.len() % 4); // always at least one byte
    let mut out = b"EXTH".to_vec();
    out.extend_from_slice(&be32(u32_of(b.len() + 12)));
    out.extend_from_slice(&be32(n));
    out.extend_from_slice(&b);
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

#[must_use]
pub fn language_code(lang: &str) -> u32 {
    let base = lang.split(['-', '_']).next().unwrap_or("").to_lowercase();
    let (l, sub): (u32, u32) = match base.as_str() {
        "af" => (54, 0),
        "ar" => (1, 0),
        "as" => (77, 0),
        "az" => (44, 0),
        "be" => (35, 0),
        "bg" => (2, 0),
        "bn" => (69, 0),
        "ca" => (3, 0),
        "cs" => (5, 0),
        "da" => (6, 0),
        "de" => (7, 0),
        "el" => (8, 0),
        "en" => (9, 0),
        "es" => (10, 0),
        "et" => (37, 0),
        "eu" => (45, 0),
        "fa" => (41, 0),
        "fi" => (11, 0),
        "fo" => (56, 0),
        "fr" => (12, 0),
        "gu" => (71, 0),
        "he" => (13, 0),
        "hi" => (57, 0),
        "hr" => (26, 0),
        "hu" => (14, 0),
        "hy" => (43, 0),
        "id" => (33, 0),
        "is" => (15, 0),
        "it" => (16, 0),
        "ja" => (17, 0),
        "ka" => (55, 0),
        "kk" => (63, 0),
        "kn" => (75, 0),
        "ko" => (18, 0),
        "lt" => (39, 0),
        "lv" => (38, 0),
        "mk" => (47, 0),
        "ml" => (76, 0),
        "mr" => (78, 0),
        "ms" => (62, 0),
        "mt" => (58, 0),
        "ne" => (97, 0),
        "nl" => (19, 0),
        "no" => (20, 0),
        "or" => (72, 0),
        "pa" => (70, 0),
        "pl" => (21, 0),
        "pt" => (22, 0),
        "rm" => (23, 0),
        "ro" => (24, 0),
        "ru" => (25, 0),
        "sa" => (79, 0),
        "se" => (59, 0),
        "sk" => (27, 0),
        "sl" => (36, 0),
        "sq" => (28, 0),
        "sr" => (26, 12),
        "st" => (48, 0),
        "sv" => (29, 0),
        "sw" => (65, 0),
        "ta" => (73, 0),
        "te" => (74, 0),
        "th" => (30, 0),
        "tn" => (50, 0),
        "tr" => (31, 0),
        "ts" => (49, 0),
        "tt" => (68, 0),
        "uk" => (34, 0),
        "ur" => (32, 0),
        "uz" => (67, 0),
        "vi" => (42, 0),
        "xh" => (52, 0),
        "zh" => (4, 0),
        "zu" => (53, 0),
        _ => (0, 0),
    };
    (sub << 8) | l
}

#[derive(Debug)]
pub struct Record0Input<'a> {
    pub compression: u16,
    pub text_length: u32,
    pub last_text_record: u16,
    pub uid: u32,
    pub first_non_text_record: u32,
    pub language_code: u32,
    pub first_resource_record: u32,
    pub fdst_record: u32,
    pub fdst_count: u32,
    pub fcis_record: u32,
    pub flis_record: u32,
    pub extra_data_flags: u32,
    pub ncx_index: u32,
    pub chunk_index: u32,
    pub skel_index: u32,
    pub guide_index: u32,
    pub exth: &'a [u8],
    pub full_title: &'a [u8],
}

#[must_use]
pub fn build_record0(r: &Record0Input<'_>) -> Vec<u8> {
    let mut b = Vec::with_capacity(9000);
    b.extend_from_slice(&r.compression.to_be_bytes()); // 0
    b.extend_from_slice(&[0; 2]); // 2
    b.extend_from_slice(&be32(r.text_length)); // 4
    b.extend_from_slice(&r.last_text_record.to_be_bytes()); // 8
    b.extend_from_slice(&u16_of(RECORD_SIZE).to_be_bytes()); // 10
    b.extend_from_slice(&[0; 4]); // 12 encryption + unused
    b.extend_from_slice(b"MOBI"); // 16
    b.extend_from_slice(&be32(264)); // 20
    b.extend_from_slice(&be32(2)); // 24 book type
    b.extend_from_slice(&be32(65001)); // 28
    b.extend_from_slice(&be32(r.uid)); // 32
    b.extend_from_slice(&be32(8)); // 36 file version
    for _ in 0..10 {
        b.extend_from_slice(&be32(NULL)); // 40..80 orth, infl, extra 0-7
    }
    b.extend_from_slice(&be32(r.first_non_text_record)); // 80
    let title_offset_pos = b.len();
    b.extend_from_slice(&be32(0)); // 84 title offset (patched)
    b.extend_from_slice(&be32(u32_of(r.full_title.len()))); // 88
    b.extend_from_slice(&be32(r.language_code)); // 92
    b.extend_from_slice(&[0; 8]); // 96 in/out lang
    b.extend_from_slice(&be32(8)); // 104 min version
    b.extend_from_slice(&be32(r.first_resource_record)); // 108
    b.extend_from_slice(&[0; 16]); // 112 huff
    b.extend_from_slice(&be32(0b101_0000)); // 128 exth flags
    b.extend_from_slice(&[0; 32]); // 132
    b.extend_from_slice(&be32(NULL)); // 164
    b.extend_from_slice(&be32(NULL)); // 168 drm offset
    b.extend_from_slice(&[0; 12]); // 172 drm count/size/flags
    b.extend_from_slice(&[0; 8]); // 184
    b.extend_from_slice(&be32(r.fdst_record)); // 192
    b.extend_from_slice(&be32(r.fdst_count)); // 196
    b.extend_from_slice(&be32(r.fcis_record)); // 200
    b.extend_from_slice(&be32(1)); // 204
    b.extend_from_slice(&be32(r.flis_record)); // 208
    b.extend_from_slice(&be32(1)); // 212
    b.extend_from_slice(&[0; 8]); // 216
    b.extend_from_slice(&be32(NULL)); // 224 srcs
    b.extend_from_slice(&be32(0)); // 228
    b.extend_from_slice(&[0xFF; 8]); // 232
    b.extend_from_slice(&be32(r.extra_data_flags)); // 240
    b.extend_from_slice(&be32(r.ncx_index)); // 244
    b.extend_from_slice(&be32(r.chunk_index)); // 248
    b.extend_from_slice(&be32(r.skel_index)); // 252
    b.extend_from_slice(&be32(NULL)); // 256 datp
    b.extend_from_slice(&be32(r.guide_index)); // 260
    b.extend_from_slice(&[0xFF; 4]); // 264
    b.extend_from_slice(&[0; 4]); // 268
    b.extend_from_slice(&[0xFF; 4]); // 272
    b.extend_from_slice(&[0; 4]); // 276
    debug_assert_eq!(b.len(), 280);
    b.extend_from_slice(r.exth);
    let title_pos = u32_of(b.len());
    b[title_offset_pos..title_offset_pos + 4].copy_from_slice(&be32(title_pos));
    b.extend_from_slice(r.full_title);
    b.extend(std::iter::repeat_n(0u8, 8192));
    b
}

#[must_use]
pub fn fcis(text_length: u32) -> Vec<u8> {
    let mut v = b"FCIS\x00\x00\x00\x14\x00\x00\x00\x10\x00\x00\x00\x02\x00\x00\x00\x00".to_vec();
    v.extend_from_slice(&be32(text_length));
    v.extend_from_slice(b"\x00\x00\x00\x00\x00\x00\x00\x28\x00\x00\x00\x00\x00\x00\x00");
    v.extend_from_slice(b"\x28\x00\x00\x00\x08\x00\x01\x00\x01\x00\x00\x00\x00");
    v
}

pub const FLIS: &[u8] = b"FLIS\0\0\0\x08\0\x41\0\0\0\0\0\0\xff\xff\xff\xff\0\x01\0\x03\0\0\0\x03\0\0\0\x01\xff\xff\xff\xff";
pub const EOF_RECORD: &[u8] = b"\xe9\x8e\r\n";

#[must_use]
pub fn write_pdb(title: &str, records: &[Vec<u8>], now: u32) -> Vec<u8> {
    let mut name: Vec<u8> =
        title.chars().map(|c| if c.is_ascii_alphanumeric() || "-.()[]".contains(c) { c as u8 } else { b'_' }).take(31).collect();
    name.resize(32, 0);
    let total: usize = records.iter().map(Vec::len).sum();
    let n = records.len();
    let mut out = Vec::with_capacity(78 + 8 * n + 2 + total);
    out.extend_from_slice(&name);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&be32(now));
    out.extend_from_slice(&be32(now));
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(b"BOOKMOBI");
    out.extend_from_slice(&be32(u32_of((2 * n).saturating_sub(1))));
    out.extend_from_slice(&be32(0));
    out.extend_from_slice(&u16_of(n).to_be_bytes());
    let mut offset = out.len() + 8 * n + 2;
    for (i, r) in records.iter().enumerate() {
        out.extend_from_slice(&be32(u32_of(offset)));
        let uid = be32(u32_of(2 * i));
        out.push(0);
        out.extend_from_slice(&uid[1..]);
        offset += r.len();
    }
    out.extend_from_slice(&[0, 0]);
    for r in records {
        out.extend_from_slice(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encint_matches_calibre() {
        assert_eq!(encint(0x11111, true), vec![0x04, 0x22, 0x91]);
        assert_eq!(encint(0x11111, false), vec![0x84, 0x22, 0x11]);
        assert_eq!(to_base(0, 32, 4), "0000");
        assert_eq!(to_base(1_000_000, 32, 0), "UGI0");
    }
}
