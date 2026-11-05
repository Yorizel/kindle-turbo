//! Existing AZW3/MOBI files: copied as-is, but we read the cover so the Kindle
//! library can show it (thumbnail sidecar).

use crate::images;
use anyhow::{Result, anyhow};

#[derive(Debug)]
pub struct Info {
    pub title: String,
    pub asin: Option<String>,
    pub cdetype: String,
    pub thumb: Option<Vec<u8>>,
}

fn u16be(data: &[u8], offset: usize) -> Option<usize> {
    data.get(offset..offset + 2).map(|bytes| usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
}
fn u32be(data: &[u8], offset: usize) -> Option<usize> {
    data.get(offset..offset + 4).map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

/// Reads the title, ASIN, content type and cover thumbnail of a MOBI/AZW3 file.
///
/// # Errors
/// Fails when the data is not a BOOKMOBI container or its headers are truncated.
pub fn read(data: &[u8]) -> Result<Info> {
    let bad = || anyhow!("arquivo MOBI/AZW3 inválido");
    if data.get(60..68) != Some(b"BOOKMOBI") {
        return Err(bad());
    }
    let record_count = u16be(data, 76).ok_or_else(bad)?;
    let record = |index: usize| -> Option<&[u8]> {
        let start = u32be(data, 78 + 8 * index)?;
        let end = if index + 1 < record_count { u32be(data, 78 + 8 * (index + 1))? } else { data.len() };
        data.get(start..end)
    };
    let header = record(0).ok_or_else(bad)?;
    let hlen = u32be(header, 20).ok_or_else(bad)?;
    let first_res = u32be(header, 108).ok_or_else(bad)?;
    let title_off = u32be(header, 84).unwrap_or(0);
    let title_len = u32be(header, 88).unwrap_or(0);
    let mut title = header.get(title_off..title_off + title_len).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();

    let mut asin = None;
    let mut cdetype = "EBOK".to_owned();
    let mut cover: Option<usize> = None;
    let mut thumb: Option<usize> = None;
    let exth = 16 + hlen;
    if header.get(exth..exth + 4) == Some(b"EXTH") {
        let exth_count = u32be(header, exth + 8).unwrap_or(0);
        let mut pos = exth + 12;
        for _ in 0..exth_count {
            let (Some(tag), Some(len)) = (u32be(header, pos), u32be(header, pos + 4)) else { break };
            if len < 8 {
                break;
            }
            let value = header.get(pos + 8..pos + len).unwrap_or(&[]);
            match tag {
                113 | 504 if asin.is_none() && !value.is_empty() => asin = Some(String::from_utf8_lossy(value).into_owned()),
                501 if value.len() == 4 => cdetype = String::from_utf8_lossy(value).into_owned(),
                503 if !value.is_empty() => title = String::from_utf8_lossy(value).into_owned(),
                201 => cover = u32be(value, 0),
                202 => thumb = u32be(value, 0),
                _ => {}
            }
            pos += len;
        }
    }
    let img = cover.or(thumb).filter(|&c| c != 0xFFFF_FFFF && first_res != 0xFFFF_FFFF).and_then(|c| record(first_res + c));
    let thumb = img.and_then(|d| images::thumbnail(d, 330, 470, 64 * 1024).ok());
    Ok(Info { title, asin, cdetype, thumb })
}
