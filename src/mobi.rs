//! Low-level MOBI/KF8 building blocks: variable width ints, CNCX, INDX records,
//! trailing byte sequences, EXTH and record 0. Ported from calibre's writer8.

use std::collections::HashMap;

pub const RECORD_SIZE: usize = 4096;
pub const NULL: u32 = 0xFFFF_FFFF;

pub fn to_base(mut num: u64, base: u64, min_digits: usize) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut v = Vec::new();
    while num > 0 {
        v.push(DIGITS[(num % base) as usize]);
        num /= base;
    }
    if v.is_empty() && min_digits == 0 {
        v.push(b'0');
    }
    while v.len() < min_digits {
        v.push(b'0');
    }
    v.reverse();
    String::from_utf8(v).unwrap()
}

/// kindle:embed / kindle:flow reference numbers.
pub fn to_ref(n: usize) -> String {
    to_base(n as u64, 32, 4)
}

pub fn encint(mut value: u64, forward: bool) -> Vec<u8> {
    let mut b = Vec::new();
    loop {
        b.push((value & 0x7F) as u8);
        value >>= 7;
        if value == 0 {
            break;
        }
    }
    if forward {
        b[0] |= 0x80;
    } else {
        let l = b.len();
        b[l - 1] |= 0x80;
    }
    b.reverse();
    b
}

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

pub fn align4(mut v: Vec<u8>) -> Vec<u8> {
    let extra = v.len() % 4;
    if extra != 0 {
        v.resize(v.len() + 4 - extra, 0);
    }
    v
}

pub fn utf8_text(s: &str) -> Vec<u8> {
    let t = s.trim();
    if t.is_empty() { b"Unknown".to_vec() } else { t.as_bytes().to_vec() }
}

pub fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

// ---------------------------------------------------------------- CNCX

pub struct Cncx {
    pub offsets: HashMap<String, u64>,
    pub records: Vec<Vec<u8>>,
}

impl Cncx {
    pub fn new<'a>(strings: impl IntoIterator<Item = &'a str>) -> Cncx {
        let mut offsets: HashMap<String, u64> = HashMap::new();
        let mut records = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        let mut offset: u64 = 0;
        const LIMIT: usize = 0x10000 - 1024;
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
            offsets.insert(s.to_string(), offset);
            offset += raw.len() as u64;
        }
        if !buf.is_empty() {
            records.push(align4(buf));
        }
        Cncx { offsets, records }
    }
    pub fn empty() -> Cncx {
        Cncx { offsets: HashMap::new(), records: Vec::new() }
    }
    pub fn get(&self, s: &str) -> u64 {
        self.offsets[s]
    }
}

// ---------------------------------------------------------------- INDX

#[derive(Clone, Copy)]
pub struct TagMeta {
    pub name: &'static str,
    pub number: u8,
    pub vpe: u8,
    pub mask: u8,
    pub end: u8,
}

pub const fn tm(name: &'static str, number: u8, vpe: u8, mask: u8) -> TagMeta {
    TagMeta { name, number, vpe, mask, end: 0 }
}
pub const END: TagMeta = TagMeta { name: "eof", number: 0, vpe: 0, mask: 0, end: 1 };

pub struct IndexEntry {
    pub name: Vec<u8>,
    pub tags: Vec<(&'static str, Vec<u64>)>,
}

impl IndexEntry {
    fn get(&self, name: &str) -> Option<&Vec<u64>> {
        self.tags.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }
}

fn mask_shift(mask: u8) -> u32 {
    match mask {
        1 | 3 => 0,
        2 => 1,
        4 | 12 => 2,
        8 => 3,
        16 | 48 => 4,
        32 => 5,
        64 | 192 => 6,
        128 => 7,
        _ => panic!("bad mask"),
    }
}

const INDX_HEADER_LENGTH: usize = 192;

pub fn build_index(tag_types: &[TagMeta], control_byte_count: usize, entries: &[IndexEntry], cncx: &Cncx) -> Vec<Vec<u8>> {
    // control bytes
    let control: Vec<Vec<u8>> = entries
        .iter()
        .map(|e| {
            let mut cbs = Vec::new();
            let mut ans: u32 = 0;
            for t in tag_types {
                if t.end == 1 {
                    cbs.push(ans as u8);
                    ans = 0;
                    continue;
                }
                let nvals = e.get(t.name).map(|v| v.len()).unwrap_or(0) as u32;
                let nentries = nvals / t.vpe as u32;
                ans |= (t.mask as u32) & (nentries << mask_shift(t.mask));
            }
            assert_eq!(cbs.len(), control_byte_count);
            cbs
        })
        .collect();

    let record_limit = 0x10000 - INDX_HEADER_LENGTH - 1048;
    let mut index_blocks: Vec<Vec<u8>> = vec![Vec::new()];
    let mut idxt_blocks: Vec<Vec<u8>> = vec![Vec::new()];
    let mut record_counts: Vec<u16> = vec![0];
    let mut last_indices: Vec<Vec<u8>> = vec![Vec::new()];

    for (i, e) in entries.iter().enumerate() {
        let mut raw = Vec::with_capacity(32);
        raw.push(e.name.len() as u8);
        raw.extend_from_slice(&e.name);
        raw.extend_from_slice(&control[i]);
        for t in tag_types {
            if let Some(vals) = e.get(t.name) {
                for &v in vals {
                    raw.extend_from_slice(&encint(v, true));
                }
            }
        }
        let mut offset = index_blocks.last().unwrap().len();
        let idxt_pos = idxt_blocks.last().unwrap().len();
        if offset + idxt_pos + raw.len() + 2 > record_limit {
            index_blocks.push(Vec::new());
            idxt_blocks.push(Vec::new());
            record_counts.push(0);
            last_indices.push(Vec::new());
            offset = 0;
        }
        *record_counts.last_mut().unwrap() += 1;
        idxt_blocks.last_mut().unwrap().extend_from_slice(&((INDX_HEADER_LENGTH + offset) as u16).to_be_bytes());
        index_blocks.last_mut().unwrap().extend_from_slice(&raw);
        *last_indices.last_mut().unwrap() = e.name.clone();
    }

    let mut index_records = Vec::new();
    for ((ib, ix), &count) in index_blocks.into_iter().zip(idxt_blocks).zip(&record_counts) {
        let ib = align4(ib);
        let mut idxt = b"IDXT".to_vec();
        idxt.extend_from_slice(&ix);
        let idxt = align4(idxt);
        let mut rec = b"INDX".to_vec();
        rec.extend_from_slice(&be32(INDX_HEADER_LENGTH as u32));
        rec.extend_from_slice(&[0; 4]);
        rec.extend_from_slice(&be32(1));
        rec.extend_from_slice(&[0; 4]);
        rec.extend_from_slice(&be32((INDX_HEADER_LENGTH + ib.len()) as u32));
        rec.extend_from_slice(&be32(count as u32));
        rec.extend_from_slice(&[0xFF; 8]);
        rec.extend_from_slice(&[0; 156]);
        rec.extend_from_slice(&ib);
        rec.extend_from_slice(&idxt);
        assert!(rec.len() <= 0x10000, "index record overflow");
        index_records.push(rec);
    }

    // TAGX
    let mut tagx = b"TAGX".to_vec();
    let body: Vec<u8> = tag_types.iter().flat_map(|t| [t.number, t.vpe, t.mask, t.end]).collect();
    tagx.extend_from_slice(&be32((12 + body.len()) as u32));
    tagx.extend_from_slice(&be32(control_byte_count as u32));
    tagx.extend_from_slice(&body);

    // geometry + IDXT of header
    let mut geometry = Vec::new();
    let mut idxt = b"IDXT".to_vec();
    let mut pos = INDX_HEADER_LENGTH + tagx.len();
    for (last, &num) in last_indices.iter().zip(&record_counts) {
        let start = geometry.len();
        idxt.extend_from_slice(&(pos as u16).to_be_bytes());
        geometry.push(last.len() as u8);
        geometry.extend_from_slice(last);
        geometry.extend_from_slice(&num.to_be_bytes());
        pos += geometry.len() - start;
    }
    let tagx = align4(tagx);
    let geometry = align4(geometry);
    let idxt = align4(idxt);

    let num_entries: u32 = record_counts.iter().map(|&c| c as u32).sum();
    let mut h = b"INDX".to_vec();
    h.extend_from_slice(&be32(INDX_HEADER_LENGTH as u32));
    h.extend_from_slice(&[0; 8]);
    h.extend_from_slice(&be32(2));
    let idxt_offset = INDX_HEADER_LENGTH + tagx.len() + geometry.len();
    h.extend_from_slice(&be32(idxt_offset as u32));
    h.extend_from_slice(&be32(index_records.len() as u32));
    h.extend_from_slice(&be32(65001));
    h.extend_from_slice(&be32(NULL));
    h.extend_from_slice(&be32(num_entries));
    h.extend_from_slice(&[0; 12]); // ordt, ligt, num ordt
    h.extend_from_slice(&be32(cncx.records.len() as u32));
    h.extend_from_slice(&[0; 124]);
    h.extend_from_slice(&be32(INDX_HEADER_LENGTH as u32));
    h.extend_from_slice(&[0; 8]);
    debug_assert_eq!(h.len(), INDX_HEADER_LENGTH);
    h.extend_from_slice(&tagx);
    h.extend_from_slice(&geometry);
    h.extend_from_slice(&idxt);
    let h = align4(h);

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

fn populate_strand(parent: TEntry, entries: &mut Vec<TEntry>) -> Vec<TEntry> {
    let mut ans = vec![parent.clone()];
    if let Some(pos) = entries.iter().position(|c| c.parent == Some(parent.index)) {
        let child = entries.remove(pos);
        ans.extend(populate_strand(child, entries));
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
                    siblings.extend(populate_strand(entry, entries));
                    break;
                } else {
                    siblings.push(entry);
                }
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
        let strand = populate_strand(top, &mut entries);
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
            if entries.last().unwrap().action == Action::Spans {
                extra.f1 = Some(0);
            }
            if Some(entries[0].index) == first_index {
                extra.f2 = Some(tbs_type);
            }
            if entries.len() > 1 {
                extra.f4 = Some(entries.len() as u8);
            }
            let mut index = entries[0].index - entries[0].parent.unwrap_or(0);
            if !ans.is_empty() && seqs.is_empty() {
                index = last_index - entries[0].index;
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
            last_index = entries.last().unwrap().index;
            seqs.push((index as u64, extra));
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
        (e.f1.is_some() as u64) | ((e.f2.is_some() as u64) << 1) | ((e.f4.is_some() as u64) << 2) | ((e.f8 as u64) << 3);
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
pub fn trailing_byte_sequences(entries: &[NcxEntry], record_lengths: &[usize]) -> Vec<Vec<u8>> {
    let mut sorted: Vec<&NcxEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.offset);
    let mut per_record: Vec<Vec<Strand>> = Vec::with_capacity(record_lengths.len());
    let mut record_start: i64 = 0;
    let mut first_live = 0usize;
    for &rl in record_lengths {
        let rl = rl as i64;
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

pub fn build_exth(m: &ExthInput) -> Vec<u8> {
    let mut b: Vec<u8> = Vec::new();
    let mut n = 0u32;
    let mut s = |b: &mut Vec<u8>, code: u32, data: &[u8]| {
        b.extend_from_slice(&be32(code));
        b.extend_from_slice(&be32(data.len() as u32 + 8));
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
        s(&mut b, 129, format!("kindle:embed:{}", to_base(t as u64, 32, 4)).as_bytes());
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
    out.extend_from_slice(&be32((b.len() + 12) as u32));
    out.extend_from_slice(&be32(n));
    out.extend_from_slice(&b);
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

pub fn language_code(lang: &str) -> u32 {
    let base = lang.split(['-', '_']).next().unwrap_or("").to_lowercase();
    let (l, sub): (u32, u32) = match base.as_str() {
        "af" => (54, 0), "ar" => (1, 0), "as" => (77, 0), "az" => (44, 0), "be" => (35, 0), "bg" => (2, 0),
        "bn" => (69, 0), "ca" => (3, 0), "cs" => (5, 0), "da" => (6, 0), "de" => (7, 0), "el" => (8, 0),
        "en" => (9, 0), "es" => (10, 0), "et" => (37, 0), "eu" => (45, 0), "fa" => (41, 0), "fi" => (11, 0),
        "fo" => (56, 0), "fr" => (12, 0), "gu" => (71, 0), "he" => (13, 0), "hi" => (57, 0), "hr" => (26, 0),
        "hu" => (14, 0), "hy" => (43, 0), "id" => (33, 0), "is" => (15, 0), "it" => (16, 0), "ja" => (17, 0),
        "ka" => (55, 0), "kk" => (63, 0), "kn" => (75, 0), "ko" => (18, 0), "lt" => (39, 0), "lv" => (38, 0),
        "mk" => (47, 0), "ml" => (76, 0), "mr" => (78, 0), "ms" => (62, 0), "mt" => (58, 0), "ne" => (97, 0),
        "nl" => (19, 0), "no" => (20, 0), "or" => (72, 0), "pa" => (70, 0), "pl" => (21, 0), "pt" => (22, 0),
        "rm" => (23, 0), "ro" => (24, 0), "ru" => (25, 0), "sa" => (79, 0), "se" => (59, 0), "sk" => (27, 0),
        "sl" => (36, 0), "sq" => (28, 0), "sr" => (26, 12), "st" => (48, 0), "sv" => (29, 0), "sw" => (65, 0),
        "ta" => (73, 0), "te" => (74, 0), "th" => (30, 0), "tn" => (50, 0), "tr" => (31, 0), "ts" => (49, 0),
        "tt" => (68, 0), "uk" => (34, 0), "ur" => (32, 0), "uz" => (67, 0), "vi" => (42, 0), "xh" => (52, 0),
        "zh" => (4, 0), "zu" => (53, 0),
        _ => (0, 0),
    };
    (sub << 8) | l
}

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

pub fn build_record0(r: &Record0Input) -> Vec<u8> {
    let mut b = Vec::with_capacity(9000);
    b.extend_from_slice(&r.compression.to_be_bytes()); // 0
    b.extend_from_slice(&[0; 2]); // 2
    b.extend_from_slice(&be32(r.text_length)); // 4
    b.extend_from_slice(&r.last_text_record.to_be_bytes()); // 8
    b.extend_from_slice(&(RECORD_SIZE as u16).to_be_bytes()); // 10
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
    b.extend_from_slice(&be32(r.full_title.len() as u32)); // 88
    b.extend_from_slice(&be32(r.language_code)); // 92
    b.extend_from_slice(&[0; 8]); // 96 in/out lang
    b.extend_from_slice(&be32(8)); // 104 min version
    b.extend_from_slice(&be32(r.first_resource_record)); // 108
    b.extend_from_slice(&[0; 16]); // 112 huff
    b.extend_from_slice(&be32(0b1010000)); // 128 exth flags
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
    let title_pos = b.len() as u32;
    b[title_offset_pos..title_offset_pos + 4].copy_from_slice(&be32(title_pos));
    b.extend_from_slice(r.full_title);
    b.extend(std::iter::repeat_n(0u8, 8192));
    b
}

pub fn fcis(text_length: u32) -> Vec<u8> {
    let mut v = b"FCIS\x00\x00\x00\x14\x00\x00\x00\x10\x00\x00\x00\x02\x00\x00\x00\x00".to_vec();
    v.extend_from_slice(&be32(text_length));
    v.extend_from_slice(b"\x00\x00\x00\x00\x00\x00\x00\x28\x00\x00\x00\x00\x00\x00\x00");
    v.extend_from_slice(b"\x28\x00\x00\x00\x08\x00\x01\x00\x01\x00\x00\x00\x00");
    v
}

pub const FLIS: &[u8] = b"FLIS\0\0\0\x08\0\x41\0\0\0\0\0\0\xff\xff\xff\xff\0\x01\0\x03\0\0\0\x03\0\0\0\x01\xff\xff\xff\xff";
pub const EOF_RECORD: &[u8] = b"\xe9\x8e\r\n";

pub fn write_pdb(title: &str, records: &[Vec<u8>], now: u32) -> Vec<u8> {
    let mut name: Vec<u8> = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-.()[]".contains(c) { c as u8 } else { b'_' })
        .take(31)
        .collect();
    name.resize(32, 0);
    let total: usize = records.iter().map(|r| r.len()).sum();
    let n = records.len();
    let mut out = Vec::with_capacity(78 + 8 * n + 2 + total);
    out.extend_from_slice(&name);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&be32(now));
    out.extend_from_slice(&be32(now));
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(b"BOOKMOBI");
    out.extend_from_slice(&be32((2 * n - 1) as u32));
    out.extend_from_slice(&be32(0));
    out.extend_from_slice(&(n as u16).to_be_bytes());
    let mut offset = out.len() + 8 * n + 2;
    for (i, r) in records.iter().enumerate() {
        out.extend_from_slice(&be32(offset as u32));
        let uid = be32((2 * i) as u32);
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
