//! Long-strip (webtoon / manhwa) layout for paged e-ink reading.
//!
//! The whole chapter is treated as one continuous strip. Gutters are found
//! with an edge test (robust to gradients and dark backgrounds, unlike a
//! flat-colour test), panels are packed whole onto fixed-size pages, and only
//! panels taller than the screen are cut, into windows that overlap so the
//! reader keeps context across the page turn. Inspired by KCC's `comic2panel`.

use rayon::prelude::*;

/// Layout parameters, in strip pixels.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// Strip / page width.
    pub width: usize,
    /// Page height (screen aspect ratio at `width`).
    pub page_h: usize,
    /// Pixel difference that counts as an edge.
    pub edge_threshold: u8,
    /// Minimum run of edge-free rows that separates two panels.
    pub min_gutter: usize,
    /// Space kept between panels packed on the same page.
    pub gap: usize,
    /// Percentage of a page repeated when a tall panel is split.
    pub overlap_pct: usize,
}

impl Params {
    /// Sensible defaults for a strip `width` px wide shown on a `screen_w` x `screen_h` display.
    #[must_use]
    pub const fn for_width(width: usize, screen_w: usize, screen_h: usize) -> Self {
        let page_h = width * screen_h / screen_w;
        Self { width, page_h, edge_threshold: 14, min_gutter: width / 80 + 2, gap: width / 50, overlap_pct: 12 }
    }
}

/// A vertical slice `[start, end)` of the strip.
pub type Span = (usize, usize);

/// For every row: does it contain no edge (ignoring the side margins)?
#[must_use]
pub fn quiet_rows(buf: &[u8], width: usize, threshold: u8) -> Vec<bool> {
    if width == 0 {
        return Vec::new();
    }
    let rows = buf.len() / width;
    let margin = width / 20;
    let (lo, hi) = (margin, width.saturating_sub(margin).max(margin + 1));
    (0..rows)
        .into_par_iter()
        .map(|r| {
            let row = &buf[r * width..(r + 1) * width];
            let prev = if r == 0 { row } else { &buf[(r - 1) * width..r * width] };
            let horiz = row[lo..hi].windows(2).all(|p| p[0].abs_diff(p[1]) <= threshold);
            let vert = row[lo..hi].iter().zip(&prev[lo..hi]).all(|(a, b)| a.abs_diff(*b) <= threshold);
            horiz && vert
        })
        .collect()
}

/// Panels = maximal runs of rows not separated by a gutter of `min_gutter` quiet rows.
#[must_use]
pub fn panels(quiet: &[bool], min_gutter: usize) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_busy = 0usize;
    let mut quiet_run = 0usize;
    for (r, &q) in quiet.iter().enumerate() {
        if q {
            quiet_run += 1;
            if let Some(s) = start
                && quiet_run >= min_gutter
            {
                out.push((s, last_busy + 1));
                start = None;
            }
        } else {
            quiet_run = 0;
            last_busy = r;
            if start.is_none() {
                start = Some(r);
            }
        }
    }
    if let Some(s) = start {
        out.push((s, last_busy + 1));
    }
    // drop specks (a few noisy rows are not a panel)
    out.retain(|&(a, b)| b - a >= min_gutter);
    out
}

/// One element placed on a page: a slice of the strip.
pub type Page = Vec<Span>;

/// Split a panel taller than a page into overlapping windows of exactly `page_h` rows.
fn split_tall((a, b): Span, page_h: usize, overlap_pct: usize) -> Vec<Span> {
    let h = b - a;
    if h <= page_h {
        return vec![(a, b)];
    }
    let ov = page_h * overlap_pct / 100;
    let advance = page_h.saturating_sub(ov).max(1);
    let n = (h - ov).div_ceil(advance).max(2);
    // spread windows evenly so first starts at a and last ends at b
    let span = h - page_h;
    (0..n).map(|i| a + span * i / (n - 1)).map(|s| (s, s + page_h)).collect()
}

/// Pack panels onto pages, top to bottom, never splitting a panel that fits.
#[must_use]
pub fn layout(panels: &[Span], p: &Params) -> Vec<Page> {
    let mut pages: Vec<Page> = Vec::new();
    let mut cur: Page = Vec::new();
    let mut used = 0usize;
    for &(a, b) in panels {
        let h = b - a;
        if h > p.page_h {
            if !cur.is_empty() {
                pages.push(std::mem::take(&mut cur));
                used = 0;
            }
            pages.extend(split_tall((a, b), p.page_h, p.overlap_pct).into_iter().map(|s| vec![s]));
            continue;
        }
        let need = if cur.is_empty() { h } else { h + p.gap };
        if used + need > p.page_h {
            pages.push(std::mem::take(&mut cur));
            used = 0;
        }
        used += if cur.is_empty() { h } else { h + p.gap };
        cur.push((a, b));
    }
    if !cur.is_empty() {
        pages.push(cur);
    }
    pages
}

/// Render a page: slices stacked from the top with `gap` between them, padded
/// with `background` to exactly `page_h` rows so every page has the same size.
#[must_use]
pub fn compose(buf: &[u8], page: &[Span], p: &Params, background: u8) -> Vec<u8> {
    let w = p.width;
    let mut out = vec![background; w * p.page_h];
    let mut y = 0usize;
    for (i, &(a, b)) in page.iter().enumerate() {
        if i > 0 {
            y += p.gap;
        }
        let rows = (b - a).min(p.page_h.saturating_sub(y));
        if rows == 0 {
            break;
        }
        out[y * w..(y + rows) * w].copy_from_slice(&buf[a * w..(a + rows) * w]);
        y += rows;
    }
    out
}

/// Stretch the histogram so the 0.5 % darkest / brightest pixels hit black / white.
/// E-ink has a narrow tonal range; scans are often greyish.
pub fn autocontrast(buf: &mut [u8]) {
    let mut hist = [0usize; 256];
    for &v in buf.iter().step_by(7) {
        hist[usize::from(v)] += 1;
    }
    let total: usize = hist.iter().sum();
    if total == 0 {
        return;
    }
    let clip = total / 200;
    let lo = hist
        .iter()
        .scan(0usize, |acc, &c| {
            *acc += c;
            Some(*acc)
        })
        .position(|c| c > clip)
        .unwrap_or(0);
    let hi = 255
        - hist
            .iter()
            .rev()
            .scan(0usize, |acc, &c| {
                *acc += c;
                Some(*acc)
            })
            .position(|c| c > clip)
            .unwrap_or(0);
    if hi <= lo + 32 || (lo < 4 && hi > 251) {
        return; // already full range, or a flat image we should not blow up
    }
    let lut: Vec<u8> = (0..256usize)
        .map(|v| {
            let s = v.saturating_sub(lo) * 255 / (hi - lo);
            u8::try_from(s.min(255)).unwrap_or(u8::MAX)
        })
        .collect();
    buf.par_iter_mut().for_each(|v| *v = lut[usize::from(*v)]);
}

/// Most frequent value of the strip edges: the natural page background.
#[must_use]
pub fn background(buf: &[u8], width: usize) -> u8 {
    let mut hist = [0usize; 256];
    for row in buf.chunks(width).step_by(16) {
        if let (Some(&l), Some(&r)) = (row.first(), row.last()) {
            hist[usize::from(l)] += 1;
            hist[usize::from(r)] += 1;
        }
    }
    let best = hist.iter().enumerate().max_by_key(|(_, c)| **c).map_or(255, |(v, _)| v);
    u8::try_from(best).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn params() -> Params {
        Params { width: 100, page_h: 130, edge_threshold: 14, min_gutter: 4, gap: 2, overlap_pct: 12 }
    }

    /// Strip with textured panels (edges) on a gradient background (no edges).
    fn strip(height: usize, panels_at: &[Span]) -> Vec<u8> {
        let w = 100;
        let mut b: Vec<u8> = (0..height).flat_map(|r| std::iter::repeat_n(u8::try_from(r * 200 / height).unwrap(), w)).collect();
        for &(a, z) in panels_at {
            for r in a..z {
                for x in 10..90 {
                    b[r * w + x] = if (x + r) % 3 == 0 { 0 } else { 250 };
                }
            }
        }
        b
    }

    #[test]
    fn finds_panels_on_gradient_background() {
        let s = strip(1000, &[(50, 200), (300, 310), (400, 900)]);
        let q = quiet_rows(&s, 100, 14);
        // the row right after a panel differs from it (panel -> background), so it counts as edge
        assert_eq!(panels(&q, 4), vec![(50, 201), (300, 311), (400, 901)]);
    }

    #[test]
    fn small_panels_share_a_page_and_tall_ones_overlap() {
        let p = params();
        let pages = layout(&[(0, 50), (60, 110), (120, 170), (200, 600)], &p);
        assert_eq!(pages[0], vec![(0, 50), (60, 110)]);
        assert_eq!(pages[1], vec![(120, 170)]);
        let windows: Vec<Span> = pages[2..].iter().map(|pg| pg[0]).collect();
        assert_eq!(windows.first().unwrap().0, 200);
        assert_eq!(windows.last().unwrap().1, 600);
        for w in windows.windows(2) {
            assert!(w[1].0 < w[0].1, "consecutive windows must overlap: {w:?}");
        }
    }

    #[test]
    fn compose_pads_to_fixed_height() {
        let p = params();
        let s = strip(300, &[(0, 300)]);
        let out = compose(&s, &[(10, 40), (50, 60)], &p, 255);
        assert_eq!(out.len(), p.width * p.page_h);
        assert_eq!(&out[..100], &s[10 * 100..11 * 100]);
        assert!(out[(30 + 2) * 100..(30 + 2) * 100 + 10] == s[50 * 100..50 * 100 + 10]);
        assert!(out[(p.page_h - 1) * 100..].iter().all(|&v| v == 255));
    }

    #[test]
    fn autocontrast_stretches_grey_scan() {
        let mut b: Vec<u8> = (0..10_000u32).map(|i| u8::try_from(60 + i % 120).unwrap()).collect();
        autocontrast(&mut b);
        assert!(b.iter().min().unwrap() <= &5);
        assert!(b.iter().max().unwrap() >= &250);
    }

    proptest! {
        #[test]
        fn layout_covers_every_panel_and_fits(cuts in proptest::collection::vec(1usize..400, 1..40)) {
            let p = params();
            let mut panels = Vec::new();
            let mut y = 0;
            for c in cuts { panels.push((y, y + c)); y += c + 7; }
            let pages = layout(&panels, &p);
            for pg in &pages {
                let used: usize = pg.iter().map(|(a, b)| b - a).sum::<usize>() + p.gap * (pg.len() - 1);
                prop_assert!(used <= p.page_h);
            }
            // every row of every panel appears on some page
            for &(a, b) in &panels {
                for r in [a, usize::midpoint(a, b), b - 1] {
                    prop_assert!(pages.iter().flatten().any(|&(s, e)| s <= r && r < e), "row {} lost", r);
                }
            }
        }
    }
}
