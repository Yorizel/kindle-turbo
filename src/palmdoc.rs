//! PalmDOC (LZ77 variant) compression used by MOBI/KF8 text records.
//!
//! Each text record (<= 4096 bytes) is compressed independently, so records
//! can be compressed in parallel.

const WINDOW: usize = 2047;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 10;
const HASH_BITS: u32 = 12;
const HASH_SIZE: usize = 1 << HASH_BITS;
const MAX_CHAIN: usize = 64;

#[inline]
fn hash3(d: &[u8], i: usize) -> usize {
    let v = (d[i] as u32) | ((d[i + 1] as u32) << 8) | ((d[i + 2] as u32) << 16);
    (v.wrapping_mul(2654435761) >> (32 - HASH_BITS)) as usize
}

#[inline]
fn is_plain(b: u8) -> bool {
    b == 0 || (0x09..0x80).contains(&b)
}

pub fn compress(data: &[u8]) -> Vec<u8> {
    let n = data.len();
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return out;
    }
    let mut head = [usize::MAX; HASH_SIZE];
    let mut prev = vec![usize::MAX; n];

    let insert = |head: &mut [usize; HASH_SIZE], prev: &mut Vec<usize>, pos: usize| {
        if pos + 2 < n {
            let h = hash3(data, pos);
            prev[pos] = head[h];
            head[h] = pos;
        }
    };

    let mut i = 0;
    while i < n {
        // Try to find a back reference (non-overlapping, within window).
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + MIN_MATCH <= n {
            let h = hash3(data, i);
            let mut cand = head[h];
            let mut chain = 0;
            let max_len_total = MAX_MATCH.min(n - i);
            while cand != usize::MAX && chain < MAX_CHAIN {
                let dist = i - cand;
                if dist > WINDOW {
                    break;
                }
                // non-overlapping: match must end before i
                let max_len = max_len_total.min(dist);
                if max_len >= MIN_MATCH && max_len > best_len {
                    let mut l = 0;
                    while l < max_len && data[cand + l] == data[i + l] {
                        l += 1;
                    }
                    if l > best_len {
                        best_len = l;
                        best_dist = dist;
                        if l == max_len_total {
                            break;
                        }
                    }
                }
                cand = prev[cand];
                chain += 1;
            }
        }
        if best_len >= MIN_MATCH {
            let code: u16 = 0x8000 | (((best_dist << 3) & 0x3FF8) as u16) | ((best_len - 3) as u16);
            out.extend_from_slice(&code.to_be_bytes());
            for p in i..i + best_len {
                insert(&mut head, &mut prev, p);
            }
            i += best_len;
            continue;
        }

        let ch = data[i];
        insert(&mut head, &mut prev, i);
        // space + char in 0x40..0x80 => single byte
        if ch == b' ' && i + 1 < n {
            let nx = data[i + 1];
            if (0x40..0x80).contains(&nx) {
                out.push(nx ^ 0x80);
                insert(&mut head, &mut prev, i + 1);
                i += 2;
                continue;
            }
        }
        if is_plain(ch) {
            out.push(ch);
            i += 1;
        } else {
            // run of up to 8 "binary" bytes
            let start = i;
            let mut j = i + 1;
            while j < n && j - start < 8 && !is_plain(data[j]) {
                insert(&mut head, &mut prev, j);
                j += 1;
            }
            out.push((j - start) as u8);
            out.extend_from_slice(&data[start..j]);
            i = j;
        }
    }
    out
}

#[cfg(test)]
pub fn decompress(data: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        i += 1;
        match c {
            1..=8 => {
                out.extend_from_slice(&data[i..i + c as usize]);
                i += c as usize;
            }
            0 | 0x09..=0x7f => out.push(c),
            0xc0..=0xff => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
            _ => {
                let v = (((c as usize) << 8) | data[i] as usize) & 0x3fff;
                i += 1;
                let dist = v >> 3;
                let len = (v & 7) + 3;
                let start = out.len() - dist;
                for k in 0..len {
                    out.push(out[start + k]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let samples: Vec<Vec<u8>> = vec![
            b"abc\x03\x04\x05\x06ms".to_vec(),
            b"a b c \xfed ".to_vec(),
            b"0123456789axyz2bxyz2cdfgfo9iuyerh".to_vec(),
            b"0123456789asd0123456789asd|yyzzxxffhhjjkk".to_vec(),
            "<p>Olá, mundo — ação ção 诡秘之主 诡秘之主 </p>".repeat(80).into_bytes(),
            (0..4096u32).map(|x| (x * 7 % 256) as u8).collect(),
            vec![b'a'; 4096],
        ];
        for s in samples {
            let c = compress(&s);
            assert_eq!(decompress(&c), s);
        }
    }
}
