//! Per-page comparisons: words and their positions from each renderer, and
//! a perceptual image difference on a normalized luminance grid.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use tiny_skia::Pixmap;

use crate::atrium::Token;

/// How close one page is. Every score is from 0 to 1 unless it says points.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PageCompare {
    pub index: usize,
    /// Both renderers produced this page.
    pub both: bool,
    /// Fraction of words that exist on the other side, by multiset F1.
    pub content_f1: f32,
    /// Fraction of words matched to a word at nearly the same place.
    pub token_match: f32,
    /// Mean normalized distance of matched words; `None` when none matched.
    pub mean_distance: Option<f32>,
    /// 1 minus the mean luminance difference over a 32 x 32 grid.
    pub image_score: f32,
    /// Mean absolute luminance difference over the same grid.
    pub perceptual_diff: f32,
    /// Atrium words, LibreOffice words.
    pub atrium_words: usize,
    pub lo_words: usize,
    /// 0 to 100.
    pub score: f32,
}

/// A whole file's comparison.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FileCompare {
    pub pages: Vec<PageCompare>,
    pub atrium_pages: usize,
    pub lo_pages: usize,
    /// Both renderers produced pages, so the score means something.
    pub compared: bool,
    /// `min / max`, 1 when they agree.
    pub page_penalty: f32,
    /// 0 to 100.
    pub score: f32,
}

/// Compares one page. `atrium` and `lo` are the rendered images; either may
/// be absent when a renderer made fewer pages.
#[allow(clippy::too_many_arguments)]
pub fn compare_page(
    index: usize,
    atrium_tokens: Option<&[Token]>,
    lo_tokens: Option<&[Token]>,
    atrium_size: (f32, f32),
    lo_size: (f32, f32),
    atrium_image: Option<&Pixmap>,
    lo_image: Option<&Pixmap>,
    diff_path: &Path,
) -> Result<PageCompare> {
    let mut out = PageCompare {
        index,
        both: atrium_tokens.is_some() && lo_tokens.is_some(),
        ..Default::default()
    };
    let (content_f1, token_match, mean_distance) = match (atrium_tokens, lo_tokens) {
        (Some(a), Some(b)) => token_scores(a, b, atrium_size, lo_size),
        _ => (0.0, 0.0, None),
    };
    out.content_f1 = content_f1;
    out.token_match = token_match;
    out.mean_distance = mean_distance;
    out.atrium_words = atrium_tokens.map(|t| t.len()).unwrap_or(0);
    out.lo_words = lo_tokens.map(|t| t.len()).unwrap_or(0);
    let (image_score, perceptual) = match (atrium_image, lo_image) {
        (Some(a), Some(b)) => {
            let diff = perceptual_diff(a, b);
            write_diff(a, b, diff_path)?;
            (1.0 - diff, diff)
        }
        _ => (0.0, 1.0),
    };
    out.image_score = image_score;
    out.perceptual_diff = perceptual;
    if out.both {
        let position = position_score(mean_distance);
        let text = 0.5 * content_f1 + 0.5 * position;
        out.score = 100.0 * (0.55 * image_score + 0.45 * text);
    }
    Ok(out)
}

/// Assembles a file's score. Pages missing on one side count as zero.
pub fn compare_file(pages: Vec<PageCompare>, atrium_pages: usize, lo_pages: usize) -> FileCompare {
    let penalty = match (atrium_pages, lo_pages) {
        (0, 0) => 0.0,
        (a, l) => a.min(l) as f32 / a.max(l) as f32,
    };
    let score = if pages.is_empty() {
        0.0
    } else {
        pages.iter().map(|p| p.score).sum::<f32>() / pages.len() as f32
    };
    FileCompare {
        pages,
        atrium_pages,
        lo_pages,
        compared: atrium_pages > 0 && lo_pages > 0,
        page_penalty: penalty,
        score,
    }
}

fn position_score(mean_distance: Option<f32>) -> f32 {
    match mean_distance {
        Some(distance) => 1.0 - (distance / 0.15).min(1.0),
        None => 0.0,
    }
}

/// Multiset F1 and nearest-neighbour matching on normalized positions.
fn token_scores(
    atrium: &[Token],
    lo: &[Token],
    atrium_size: (f32, f32),
    lo_size: (f32, f32),
) -> (f32, f32, Option<f32>) {
    if atrium.is_empty() || lo.is_empty() {
        return (0.0, 0.0, None);
    }
    let (aw, ah) = (atrium_size.0.max(1.0), atrium_size.1.max(1.0));
    let (lw, lh) = (lo_size.0.max(1.0), lo_size.1.max(1.0));
    let mut used = vec![false; lo.len()];
    let mut matched = 0usize;
    let mut distances = Vec::new();
    for a in atrium {
        let key = normalize(&a.text);
        let ax = a.x / aw;
        let ay = a.y / ah;
        let mut best: Option<(usize, f32)> = None;
        for (i, l) in lo.iter().enumerate() {
            if used[i] || normalize(&l.text) != key {
                continue;
            }
            let dx = ax - l.x / lw;
            let dy = ay - l.y / lh;
            let distance = (dx * dx + dy * dy).sqrt();
            if best.is_none_or(|(_, d)| distance < d) {
                best = Some((i, distance));
            }
        }
        if let Some((i, distance)) = best {
            used[i] = true;
            matched += 1;
            distances.push(distance);
        }
    }
    let precision = matched as f32 / atrium.len() as f32;
    let recall = matched as f32 / lo.len() as f32;
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };
    let mean = if distances.is_empty() {
        None
    } else {
        Some(distances.iter().sum::<f32>() / distances.len() as f32)
    };
    let token_match = matched as f32 / atrium.len().max(lo.len()) as f32;
    (f1, token_match, mean)
}

fn normalize(text: &str) -> String {
    text.trim().to_lowercase()
}

const GRID: usize = 32;

/// Mean absolute difference of average luminance over a normalized grid.
pub fn perceptual_diff(a: &Pixmap, b: &Pixmap) -> f32 {
    let ga = luminance_grid(a);
    let gb = luminance_grid(b);
    let mut total = 0.0;
    for (x, y) in ga.iter().zip(gb.iter()) {
        total += (x - y).abs() / 255.0;
    }
    total / (GRID * GRID) as f32
}

fn luminance_grid(pixmap: &Pixmap) -> Vec<f32> {
    let mut out = vec![0.0; GRID * GRID];
    let width = pixmap.width().max(1);
    let height = pixmap.height().max(1);
    for gy in 0..GRID {
        for gx in 0..GRID {
            let x0 = gx * width as usize / GRID;
            let x1 = ((gx + 1) * width as usize / GRID)
                .max(x0 + 1)
                .min(width as usize);
            let y0 = gy * height as usize / GRID;
            let y1 = ((gy + 1) * height as usize / GRID)
                .max(y0 + 1)
                .min(height as usize);
            let mut sum = 0.0;
            let mut count = 0.0;
            for y in (y0..y1).step_by(2) {
                for x in (x0..x1).step_by(2) {
                    if let Some(pixel) = pixmap.pixel(x as u32, y as u32) {
                        let r = pixel.red() as f32;
                        let g = pixel.green() as f32;
                        let b = pixel.blue() as f32;
                        sum += 0.2126 * r + 0.7152 * g + 0.0722 * b;
                        count += 1.0;
                    }
                }
            }
            out[gy * GRID + gx] = if count > 0.0 { sum / count } else { 255.0 };
        }
    }
    out
}

/// Writes a normalized difference image: red where the pages disagree.
fn write_diff(a: &Pixmap, b: &Pixmap, path: &Path) -> Result<()> {
    const OUT_WIDTH: u32 = 480;
    let aspect = (a.height() as f32 / a.width().max(1) as f32)
        .max(b.height() as f32 / b.width().max(1) as f32);
    let width = OUT_WIDTH;
    let height = ((OUT_WIDTH as f32 * aspect).round() as u32).clamp(1, 4000);
    let Some(mut out) = Pixmap::new(width, height) else {
        return Ok(());
    };
    let ga = luminance_grid(a);
    let gb = luminance_grid(b);
    for y in 0..height {
        for x in 0..width {
            let gx = (x as usize * GRID / width as usize).min(GRID - 1);
            let gy = (y as usize * GRID / height as usize).min(GRID - 1);
            let diff = (ga[gy * GRID + gx] - gb[gy * GRID + gx]).abs() / 255.0;
            let green = (255.0 * (1.0 - diff)).round() as u8;
            let color = tiny_skia::PremultipliedColorU8::from_rgba(255, green, green, 255)
                .expect("red with full alpha");
            let at = y as usize * width as usize + x as usize;
            out.pixels_mut()[at] = color;
        }
    }
    out.save_png(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Color;

    fn canvas(color: Color) -> Pixmap {
        let mut pixmap = Pixmap::new(64, 64).unwrap();
        pixmap.fill(color);
        pixmap
    }

    #[test]
    fn identical_pages_have_no_difference() {
        let a = canvas(Color::from_rgba8(255, 255, 255, 255));
        let b = canvas(Color::from_rgba8(255, 255, 255, 255));
        assert!(perceptual_diff(&a, &b) < 0.001);
    }

    #[test]
    fn a_black_half_differs_from_white() {
        let mut a = canvas(Color::from_rgba8(255, 255, 255, 255));
        let b = canvas(Color::from_rgba8(255, 255, 255, 255));
        for y in 0..64 {
            for x in 0..32 {
                a.pixels_mut()[y * 64 + x] =
                    tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 255)
                        .expect("black with full alpha");
            }
        }
        let diff = perceptual_diff(&a, &b);
        assert!(diff > 0.4 && diff < 0.6, "{diff}");
    }

    #[test]
    fn tokens_match_at_the_same_place() {
        let a = vec![
            Token {
                text: "Hello".into(),
                x: 10.0,
                y: 20.0,
                size: 12.0,
            },
            Token {
                text: "world".into(),
                x: 60.0,
                y: 20.0,
                size: 12.0,
            },
        ];
        let b = a.clone();
        let (f1, _, distance) = token_scores(&a, &b, (200.0, 200.0), (200.0, 200.0));
        assert!((f1 - 1.0).abs() < 0.001, "{f1}");
        assert!(distance.unwrap() < 0.001);
    }

    #[test]
    fn tokens_far_apart_score_lower() {
        let a = vec![Token {
            text: "Hello".into(),
            x: 10.0,
            y: 20.0,
            size: 12.0,
        }];
        let b = vec![Token {
            text: "Hello".into(),
            x: 150.0,
            y: 180.0,
            size: 12.0,
        }];
        let (_, _, distance) = token_scores(&a, &b, (200.0, 200.0), (200.0, 200.0));
        assert!(distance.unwrap() > 0.5);
    }
}
