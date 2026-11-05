//! Existing AZW3/MOBI files: copied as-is, but we read the cover so the Kindle
//! library can show it (thumbnail sidecar).

use crate::images;
use anyhow::{Result, anyhow};

pub struct Info {
    pub title: String,
    pub asin: Option<String>,
    pub cdetype: String,
    pub thumb: Option<Vec<u8>>,
}

fn u16be(d: &[u8], o: usize) -> Option<usize> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
}
fn u32be(d: &[u8], o: usize) -> Option<usize> {
    d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

pub fn read(data: &[u8]) -> Result<Info> {
    let bad = || anyhow!("arquivo MOBI/AZW3 inválido");
    if data.get(60..68) != Some(b"BOOKMOBI") {
        return Err(bad());
    }
    let n = u16be(data, 76).ok_or_else(bad)?;
    let rec = |i: usize| -> Option<&[u8]> {
        let s = u32be(data, 78 + 8 * i)?;
        let e = if i + 1 < n { u32be(data, 78 + 8 * (i + 1))? } else { data.len() };
        data.get(s..e)
    };
    let r0 = rec(0).ok_or_else(bad)?;
    let hlen = u32be(r0, 20).ok_or_else(bad)?;
    let first_res = u32be(r0, 108).ok_or_else(bad)?;
    let title_off = u32be(r0, 84).unwrap_or(0);
    let title_len = u32be(r0, 88).unwrap_or(0);
    let mut title = r0.get(title_off..title_off + title_len).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();

    let mut asin = None;
    let mut cdetype = "EBOK".to_owned();
    let mut cover: Option<usize> = None;
    let mut thumb: Option<usize> = None;
    let exth = 16 + hlen;
    if r0.get(exth..exth + 4) == Some(b"EXTH") {
        let count = u32be(r0, exth + 8).unwrap_or(0);
        let mut p = exth + 12;
        for _ in 0..count {
            let (Some(t), Some(l)) = (u32be(r0, p), u32be(r0, p + 4)) else { break };
            if l < 8 {
                break;
            }
            let v = r0.get(p + 8..p + l).unwrap_or(&[]);
            match t {
                113 | 504 if asin.is_none() && !v.is_empty() => asin = Some(String::from_utf8_lossy(v).into_owned()),
                501 if v.len() == 4 => cdetype = String::from_utf8_lossy(v).into_owned(),
                503 if !v.is_empty() => title = String::from_utf8_lossy(v).into_owned(),
                201 => cover = u32be(v, 0),
                202 => thumb = u32be(v, 0),
                _ => {}
            }
            p += l;
        }
    }
    let img = cover
        .or(thumb)
        .filter(|&c| c != 0xFFFF_FFFF && first_res != 0xFFFF_FFFF)
        .and_then(|c| rec(first_res + c));
    let thumb = img.and_then(|d| images::thumbnail(d, 330, 470, 64 * 1024).ok());
    Ok(Info { title, asin, cdetype, thumb })
}
