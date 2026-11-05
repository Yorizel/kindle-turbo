//! `PalmDOC` compression (LZ77 variant) used by MOBI/KF8 text records.
//!
//! The format is fixed by the Kindle, so the gains come from the encoder:
//! every record is parsed *optimally* (dynamic programming over all
//! encodings) instead of greedily. Records are independent, so callers
//! compress them in parallel.

const WINDOW: usize = 2047;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 10;
const MAX_RUN: usize = 8;
const HASH_BITS: u32 = 12;
const HASH_SIZE: usize = 1 << HASH_BITS;
const MAX_CHAIN: usize = 256;
const NONE: u32 = u32::MAX;

#[inline]
fn hash3(d: &[u8], i: usize) -> usize {
    let v = u32::from(d[i]) | (u32::from(d[i + 1]) << 8) | (u32::from(d[i + 2]) << 16);
    (v.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
}

/// Bytes that can be emitted as themselves.
#[inline]
const fn is_plain(b: u8) -> bool {
    b == 0 || (b >= 0x09 && b < 0x80)
}

/// One step of the chosen parse.
#[derive(Clone, Copy)]
enum Step {
    Literal,
    SpacePair,
    Run(u8),
    Match { len: u8, dist: u16 },
}

/// Longest non-overlapping back-reference at every position.
fn longest_matches(data: &[u8]) -> Vec<(u8, u16)> {
    let n = data.len();
    let mut best = vec![(0u8, 0u16); n];
    if n < MIN_MATCH {
        return best;
    }
    let mut head = vec![NONE; HASH_SIZE];
    let mut prev = vec![NONE; n];
    for i in 0..n {
        if i + MIN_MATCH > n {
            break;
        }
        let h = hash3(data, i);
        let limit = MAX_MATCH.min(n - i);
        let mut cand = head[h];
        let mut chain = 0;
        let (mut blen, mut bdist) = (0usize, 0usize);
        while cand != NONE && chain < MAX_CHAIN {
            let c = cand as usize;
            let dist = i - c;
            if dist > WINDOW {
                break;
            }
            let max_len = limit.min(dist);
            if max_len > blen {
                let l = data[c..c + max_len].iter().zip(&data[i..i + max_len]).take_while(|(a, b)| a == b).count();
                if l > blen {
                    blen = l;
                    bdist = dist;
                    if l == limit {
                        break;
                    }
                }
            }
            cand = prev[c];
            chain += 1;
        }
        if blen >= MIN_MATCH {
            // both fit: blen <= 10, bdist <= 2047
            best[i] = (u8::try_from(blen).unwrap_or(0), u16::try_from(bdist).unwrap_or(0));
        }
        prev[i] = head[h];
        head[h] = u32::try_from(i).unwrap_or(NONE);
    }
    best
}

/// Compress one record (any size, normally <= 4096 bytes) optimally.
#[must_use]
pub fn compress(data: &[u8]) -> Vec<u8> {
    let n = data.len();
    if n == 0 {
        return Vec::new();
    }
    let matches = longest_matches(data);
    // cost[i] = minimal encoded size of data[i..]
    let mut cost = vec![u32::MAX; n + 1];
    let mut step = vec![Step::Literal; n];
    cost[n] = 0;
    for i in (0..n).rev() {
        let mut best = u32::MAX;
        let mut choice = Step::Literal;
        let mut consider = |c: u32, s: Step| {
            if c < best {
                best = c;
                choice = s;
            }
        };
        let b = data[i];
        if is_plain(b) {
            consider(1 + cost[i + 1], Step::Literal);
        }
        if b == b' ' && i + 1 < n && (0x40..0x80).contains(&data[i + 1]) {
            consider(1 + cost[i + 2], Step::SpacePair);
        }
        for k in 1..=MAX_RUN.min(n - i) {
            // a run may contain any bytes; header byte + k raw bytes
            consider(1 + k as u32 + cost[i + k], Step::Run(k as u8));
        }
        let (mlen, dist) = matches[i];
        for len in MIN_MATCH..=mlen as usize {
            consider(2 + cost[i + len], Step::Match { len: len as u8, dist });
        }
        cost[i] = best;
        step[i] = choice;
    }

    let mut out = Vec::with_capacity(cost[0] as usize);
    let mut i = 0;
    while i < n {
        match step[i] {
            Step::Literal => {
                out.push(data[i]);
                i += 1;
            }
            Step::SpacePair => {
                out.push(data[i + 1] ^ 0x80);
                i += 2;
            }
            Step::Run(k) => {
                let k = usize::from(k);
                out.push(k as u8);
                out.extend_from_slice(&data[i..i + k]);
                i += k;
            }
            Step::Match { len, dist } => {
                let code: u16 = 0x8000 | ((dist << 3) & 0x3FF8) | (u16::from(len) - 3);
                out.extend_from_slice(&code.to_be_bytes());
                i += usize::from(len);
            }
        }
    }
    out
}

/// Reference decoder (used by tests and by the bundled reader).
///
/// # Errors
/// Returns `None` on malformed input.
#[must_use]
pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(data.len() * 2);
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        i += 1;
        match c {
            1..=8 => {
                let k = usize::from(c);
                out.extend_from_slice(data.get(i..i + k)?);
                i += k;
            }
            0 | 0x09..=0x7f => out.push(c),
            0xc0..=0xff => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
            _ => {
                let v = ((usize::from(c) << 8) | usize::from(*data.get(i)?)) & 0x3fff;
                i += 1;
                let dist = v >> 3;
                let len = (v & 7) + 3;
                let start = out.len().checked_sub(dist)?;
                if dist == 0 {
                    return None;
                }
                for k in 0..len {
                    let b = *out.get(start + k)?;
                    out.push(b);
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rt(s: &[u8]) {
        let c = compress(s);
        assert_eq!(decompress(&c).as_deref(), Some(s), "roundtrip failed for {} bytes", s.len());
    }

    #[test]
    fn calibre_vectors() {
        rt(b"abc\x03\x04\x05\x06ms");
        rt(b"a b c \xfed ");
        rt(b"0123456789axyz2bxyz2cdfgfo9iuyerh");
        rt(b"0123456789asd0123456789asd|yyzzxxffhhjjkk");
        rt(b"");
        rt(b"a");
        rt(b" A");
    }

    #[test]
    fn real_text_compresses() {
        let s = "<p>Klein Moretti abriu os olhos — ação, ção, 诡秘之主. The fog was thick.</p>\n".repeat(60);
        let s = &s.as_bytes()[..4096];
        let c = compress(s);
        rt(s);
        assert!(c.len() < s.len() / 2, "ratio too poor: {} -> {}", s.len(), c.len());
    }

    #[test]
    fn worst_case_binary() {
        let s: Vec<u8> = (0..4096u32).map(|x| (x.wrapping_mul(2_654_435_761) >> 24) as u8 | 0x80).collect();
        rt(&s);
        // at most 1 header per 8 bytes overhead
        assert!(compress(&s).len() <= s.len() + s.len() / 8 + 1);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]
        #[test]
        fn roundtrip_any_bytes(s in proptest::collection::vec(any::<u8>(), 0..4096)) {
            rt(&s);
        }

        #[test]
        fn roundtrip_texty(s in "[ a-zA-Z0-9<>/=\"áçãé诡\n]{0,1500}") {
            rt(s.as_bytes());
        }

        #[test]
        fn never_bigger_than_greedy_bound(s in proptest::collection::vec(0x20u8..0x7f, 0..4096)) {
            // plain ASCII can always be emitted 1:1, so optimal is never larger
            prop_assert!(compress(&s).len() <= s.len());
        }
    }
}
