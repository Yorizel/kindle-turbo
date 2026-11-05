//! Image handling: JPEG/PNG/GIF pass through (JPEG markers patched losslessly),
//! everything else (WebP, BMP, ...) is decoded, fitted to the screen and re-encoded as JPEG.

use anyhow::{Result, anyhow};
use image::{DynamicImage, GenericImageView, imageops::FilterType};

/// Kindle Paperwhite 5 screen, with a little headroom.
pub const MAX_W: u32 = 1264;
pub const MAX_H: u32 = 1680;
const JPEG_QUALITY: u8 = 85;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

pub const PLACEHOLDER_GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\xf0\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00!\xfe calibre-placeholder-gif-for-azw3\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Jpeg,
    Png,
    Gif,
    Other,
}

#[must_use]
pub fn sniff(d: &[u8]) -> Kind {
    if d.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Kind::Jpeg
    } else if d.starts_with(b"\x89PNG\r\n\x1a\n") {
        Kind::Png
    } else if d.starts_with(b"GIF8") {
        Kind::Gif
    } else {
        Kind::Other
    }
}

/// Returns (data, mime).
///
/// # Errors
///
/// Fails if the data is not a decodable image or cannot be re-encoded as JPEG.
pub fn process(data: &[u8]) -> Result<(Vec<u8>, &'static str)> {
    let k = sniff(data);
    if data.len() <= MAX_IMAGE_BYTES {
        match k {
            Kind::Jpeg => return Ok((fix_jpeg_markers(data), "image/jpeg")),
            Kind::Png => return Ok((optimize_png(data), "image/png")),
            Kind::Gif => return Ok((data.to_vec(), "image/gif")),
            Kind::Other => {}
        }
    }
    let img = image::load_from_memory(data).map_err(|e| anyhow!("imagem inválida: {e}"))?;
    let img = fit(img, MAX_W, MAX_H);
    Ok((encode_jpeg(&img, JPEG_QUALITY)?, "image/jpeg"))
}

#[must_use]
pub fn fit(img: DynamicImage, w: u32, h: u32) -> DynamicImage {
    let (iw, ih) = img.dimensions();
    if iw <= w && ih <= h {
        return img;
    }
    img.resize(w, h, FilterType::Triangle)
}

/// Encode `img` as an RGB JPEG; transparency is flattened onto white.
///
/// # Errors
///
/// Fails if the dimensions do not fit the JPEG format (65535 pixels per side)
/// or the encoder reports an error.
pub fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    // flatten alpha onto white
    let rgb = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        let mut out = image::RgbImage::new(rgba.width(), rgba.height());
        for (o, p) in out.pixels_mut().zip(rgba.pixels()) {
            let a = u32::from(p[3]);
            for c in 0..3 {
                // result is at most 255 because each channel and alpha are <= 255
                o[c] = u8::try_from((u32::from(p[c]) * a + 255 * (255 - a)) / 255).unwrap_or(u8::MAX);
            }
        }
        out
    } else {
        img.to_rgb8()
    };
    let width = u16::try_from(rgb.width()).map_err(|_| anyhow!("jpeg: largura excede 65535"))?;
    let height = u16::try_from(rgb.height()).map_err(|_| anyhow!("jpeg: altura excede 65535"))?;
    let mut buf = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut buf, quality);
    enc.set_optimized_huffman_tables(true);
    enc.encode(rgb.as_raw(), width, height, jpeg_encoder::ColorType::Rgb).map_err(|e| anyhow!("jpeg: {e}"))?;
    Ok(buf)
}

/// Lossless PNG optimisation (oxipng). Falls back to the original bytes.
#[must_use]
pub fn optimize_png(data: &[u8]) -> Vec<u8> {
    let mut opts = oxipng::Options::from_preset(2);
    // pixels are untouched; metadata chunks are useless on the Kindle
    opts.strip = oxipng::StripChunks::Safe;
    oxipng::optimize_from_memory(data, &opts).ok().filter(|o| o.len() < data.len()).unwrap_or_else(|| data.to_vec())
}

/// Cover thumbnail for the Kindle library (<= 16 KiB).
///
/// # Errors
///
/// Fails if `data` is not a decodable image or the JPEG encoding fails.
pub fn thumbnail(data: &[u8], w: u32, h: u32, max_bytes: usize) -> Result<Vec<u8>> {
    let img = image::load_from_memory(data).map_err(|e| anyhow!("capa inválida: {e}"))?;
    let img = img.resize(w, h, FilterType::Triangle);
    let mut q = 90u8;
    loop {
        let out = encode_jpeg(&img, q)?;
        if out.len() <= max_bytes || q <= 20 {
            return Ok(out);
        }
        q -= 10;
    }
}

/// The Kindle renderer can't show JPEGs without a JFIF APP0 segment and hides
/// covers that carry EXIF. Patch the markers without re-encoding.
#[must_use]
pub fn fix_jpeg_markers(d: &[u8]) -> Vec<u8> {
    let mut segs: Vec<(usize, usize)> = Vec::new(); // (start, end) of segments before SOS
    let mut has_jfif = false;
    let mut has_exif = false;
    let mut i = 2;
    while i + 4 <= d.len() && d[i] == 0xFF {
        let marker = d[i + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        let end = (i + 2 + len).min(d.len());
        if marker == 0xE0 && d[i + 4..].starts_with(b"JFIF\0") {
            has_jfif = true;
        }
        if marker == 0xE1 && d[i + 4..].starts_with(b"Exif\0") {
            has_exif = true;
        }
        segs.push((i, end));
        i = end;
    }
    if has_jfif && !has_exif {
        return d.to_vec();
    }
    let mut out = Vec::with_capacity(d.len() + 18);
    out.extend_from_slice(&[0xFF, 0xD8]);
    if !has_jfif {
        out.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
    }
    for &(s, e) in &segs {
        if d[s + 1] == 0xE1 && d[s + 4..].starts_with(b"Exif\0") {
            continue;
        }
        out.extend_from_slice(&d[s..e]);
    }
    out.extend_from_slice(&d[i..]);
    out
}
