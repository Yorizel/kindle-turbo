//! Fallback cover with title and author, for books that ship without one.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use image::{Rgb, RgbImage};

const W: u32 = 600;
const H: u32 = 900;

fn find_font() -> Option<(Vec<u8>, u32)> {
    // fontconfig knows best (handles CJK titles too)
    for pattern in ["Noto Sans CJK KR:bold", "sans:bold", "sans"] {
        if let Ok(o) = std::process::Command::new("fc-match").args(["-f", "%{file}|%{index}", pattern]).output() {
            let s = String::from_utf8_lossy(&o.stdout);
            if let Some((f, i)) = s.split_once('|') {
                if let Ok(data) = std::fs::read(f) {
                    return Some((data, i.trim().parse().unwrap_or(0)));
                }
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

fn wrap(font: &FontRef, size: f32, text: &str, max_w: f32) -> Vec<String> {
    let sf = font.as_scaled(PxScale::from(size));
    let width = |s: &str| s.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum::<f32>();
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if width(&cand) <= max_w || cur.is_empty() {
            cur = cand;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
        // very long single word (e.g. CJK without spaces): hard-break by chars
        while width(&cur) > max_w && cur.chars().count() > 1 {
            let mut head = String::new();
            for c in cur.chars() {
                if width(&format!("{head}{c}")) > max_w {
                    break;
                }
                head.push(c);
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

fn draw_line(img: &mut RgbImage, font: &FontRef, size: f32, text: &str, y: f32, color: [u8; 3]) {
    let sf = font.as_scaled(PxScale::from(size));
    let total: f32 = text.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum();
    let mut x = (W as f32 - total) / 2.0;
    for c in text.chars() {
        let gid = font.glyph_id(c);
        let g = gid.with_scale_and_position(PxScale::from(size), point(x, y + sf.ascent()));
        x += sf.h_advance(gid);
        if let Some(o) = font.outline_glyph(g) {
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let px = b.min.x as i32 + gx as i32;
                let py = b.min.y as i32 + gy as i32;
                if px >= 0 && py >= 0 && (px as u32) < W && (py as u32) < H {
                    let p = img.get_pixel_mut(px as u32, py as u32);
                    for i in 0..3 {
                        p[i] = (p[i] as f32 * (1.0 - cov) + color[i] as f32 * cov) as u8;
                    }
                }
            });
        }
    }
}

pub fn generate(title: &str, author: Option<&str>) -> Option<Vec<u8>> {
    let (data, index) = find_font()?;
    let font = FontRef::try_from_slice_and_index(&data, index).ok()?;
    let mut img = RgbImage::from_fn(W, H, |_, y| {
        let t = y as f32 / H as f32;
        Rgb([(22.0 + 40.0 * t) as u8, (20.0 + 6.0 * t) as u8, (34.0 - 6.0 * t) as u8])
    });
    // frame
    for m in [24u32, 30] {
        for x in m..W - m {
            img.put_pixel(x, m, Rgb([190, 160, 110]));
            img.put_pixel(x, H - m - 1, Rgb([190, 160, 110]));
        }
        for y in m..H - m {
            img.put_pixel(m, y, Rgb([190, 160, 110]));
            img.put_pixel(W - m - 1, y, Rgb([190, 160, 110]));
        }
    }
    let max_w = W as f32 - 110.0;
    let mut size = 66.0;
    let mut lines = wrap(&font, size, title, max_w);
    while lines.len() > 6 && size > 30.0 {
        size -= 4.0;
        lines = wrap(&font, size, title, max_w);
    }
    let lh = size * 1.18;
    let block = lh * lines.len() as f32;
    let mut y = (H as f32 * 0.42 - block / 2.0).max(70.0);
    for l in &lines {
        draw_line(&mut img, &font, size, l, y, [240, 232, 215]);
        y += lh;
    }
    if let Some(a) = author.filter(|a| !a.trim().is_empty()) {
        let al = wrap(&font, 30.0, a, max_w);
        let mut ay = H as f32 - 110.0 - 36.0 * (al.len().saturating_sub(1)) as f32;
        for l in al.iter().take(2) {
            draw_line(&mut img, &font, 30.0, l, ay, [190, 160, 110]);
            ay += 36.0;
        }
    }
    crate::images::encode_jpeg(&image::DynamicImage::ImageRgb8(img), 90).ok()
}
