//! Fallback cover with title and author, for books that ship without one.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use image::{Rgb, RgbImage};

const W: u32 = 600;
const H: u32 = 900;
const ACCENT: [u8; 3] = [190, 160, 110];
const TITLE_COLOR: [u8; 3] = [240, 232, 215];

/// Truncates a float to a whole pixel coordinate or colour step. `as` is the only
/// float-to-int conversion std provides, and its saturating behaviour is wanted here.
#[expect(clippy::cast_possible_truncation, reason = "pixel and colour truncation; the cast saturates by design")]
const fn trunc_i32(v: f32) -> i32 {
    v as i32
}

/// Converts a pixel count or index to `f32`. Image sizes here are far below `u16::MAX`.
fn px_f32(n: impl TryInto<u16>) -> f32 {
    f32::from(n.try_into().unwrap_or(u16::MAX))
}

/// Converts a colour or coverage value to a byte, clamping to 0..=255.
fn byte(v: f32) -> u8 {
    u8::try_from(trunc_i32(v.clamp(0.0, 255.0))).unwrap_or(u8::MAX)
}

/// Blends `src` over `dst` by coverage in 0..=1.
fn blend(dst: u8, src: u8, cov: f32) -> u8 {
    byte(f32::from(src).mul_add(cov, f32::from(dst) * (1.0 - cov)))
}

fn find_font() -> Option<(Vec<u8>, u32)> {
    // fontconfig knows best (handles CJK titles too)
    for pattern in ["Noto Sans CJK KR:bold", "sans:bold", "sans"] {
        if let Ok(o) = std::process::Command::new("fc-match").args(["-f", "%{file}|%{index}", pattern]).output() {
            let s = String::from_utf8_lossy(&o.stdout);
            if let Some((f, i)) = s.split_once('|')
                && let Ok(data) = std::fs::read(f)
            {
                return Some((data, i.trim().parse().unwrap_or(0)));
            }
        }
    }
    for p in [
        "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
    ] {
        if let Ok(d) = std::fs::read(p) {
            return Some((d, 0));
        }
    }
    None
}

fn wrap(font: &FontRef<'_>, size: f32, text: &str, max_w: f32) -> Vec<String> {
    let scaled = font.as_scaled(PxScale::from(size));
    let width = |s: &str| s.chars().map(|ch| scaled.h_advance(font.glyph_id(ch))).sum::<f32>();
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_owned() } else { format!("{cur} {word}") };
        if width(&cand) <= max_w || cur.is_empty() {
            cur = cand;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
        // very long single word (e.g. CJK without spaces): hard-break by chars
        while width(&cur) > max_w && cur.chars().count() > 1 {
            let mut head = String::new();
            for ch in cur.chars() {
                if width(&format!("{head}{ch}")) > max_w {
                    break;
                }
                head.push(ch);
            }
            if head.is_empty() {
                break;
            }
            let rest: String = cur.chars().skip(head.chars().count()).collect();
            lines.push(head);
            cur = rest;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Draws one centred line whose glyph box starts at `top`.
fn draw_line(img: &mut RgbImage, font: &FontRef<'_>, size: f32, text: &str, top: f32, color: [u8; 3]) {
    let scaled = font.as_scaled(PxScale::from(size));
    let total: f32 = text.chars().map(|ch| scaled.h_advance(font.glyph_id(ch))).sum();
    let mut pen_x = (px_f32(W) - total) / 2.0;
    for ch in text.chars() {
        let glyph_id = font.glyph_id(ch);
        let glyph = glyph_id.with_scale_and_position(PxScale::from(size), point(pen_x, top + scaled.ascent()));
        pen_x += scaled.h_advance(glyph_id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        let (x0, y0) = (i64::from(trunc_i32(bounds.min.x)), i64::from(trunc_i32(bounds.min.y)));
        outline.draw(|gx, gy, cov| {
            let (Ok(px), Ok(py)) = (u32::try_from(x0 + i64::from(gx)), u32::try_from(y0 + i64::from(gy))) else {
                return;
            };
            if px < W && py < H {
                let pixel = img.get_pixel_mut(px, py);
                for (dst, &src) in pixel.0.iter_mut().zip(color.iter()) {
                    *dst = blend(*dst, src, cov);
                }
            }
        });
    }
}

fn draw_frame(img: &mut RgbImage) {
    for inset in [24u32, 30] {
        for x in inset..W - inset {
            img.put_pixel(x, inset, Rgb(ACCENT));
            img.put_pixel(x, H - inset - 1, Rgb(ACCENT));
        }
        for y in inset..H - inset {
            img.put_pixel(inset, y, Rgb(ACCENT));
            img.put_pixel(W - inset - 1, y, Rgb(ACCENT));
        }
    }
}

#[must_use]
pub fn generate(title: &str, author: Option<&str>) -> Option<Vec<u8>> {
    let (data, index) = find_font()?;
    let font = FontRef::try_from_slice_and_index(&data, index).ok()?;
    let mut img = RgbImage::from_fn(W, H, |_, row| {
        let t = px_f32(row) / px_f32(H);
        Rgb([byte(40.0f32.mul_add(t, 22.0)), byte(6.0f32.mul_add(t, 20.0)), byte(6.0f32.mul_add(-t, 34.0))])
    });
    draw_frame(&mut img);

    let max_w = px_f32(W) - 110.0;
    let mut size = 66.0;
    let mut lines = wrap(&font, size, title, max_w);
    while lines.len() > 6 && size > 30.0 {
        size -= 4.0;
        lines = wrap(&font, size, title, max_w);
    }
    let line_h = size * 1.18;
    let block = line_h * px_f32(lines.len());
    let mut top = px_f32(H).mul_add(0.42, -(block / 2.0)).max(70.0);
    for line in &lines {
        draw_line(&mut img, &font, size, line, top, TITLE_COLOR);
        top += line_h;
    }
    if let Some(author) = author.filter(|name| !name.trim().is_empty()) {
        let author_lines = wrap(&font, 30.0, author, max_w);
        let mut author_top = 36.0f32.mul_add(-px_f32(author_lines.len().saturating_sub(1)), px_f32(H) - 110.0);
        for line in author_lines.iter().take(2) {
            draw_line(&mut img, &font, 30.0, line, author_top, ACCENT);
            author_top += 36.0;
        }
    }
    crate::images::encode_jpeg(&image::DynamicImage::ImageRgb8(img), 90).ok()
}
