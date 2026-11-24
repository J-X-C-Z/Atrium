//! Opens a corpus file with `atrium-office` and lays it out with
//! `atrium-layout`: Word pages, slides, and a simple paged sheet grid.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use atrium_layout::{FontKey, Item, Rect, Seg, Shaper};
use atrium_office::look::{HAlign, Rgb, VAlign};
use atrium_office::{Cell, Office, Value, Workbook};

use crate::render::RenderTarget;
use crate::shaper::{SystemShaper, is_cjk};

/// A word on a page, with its position in points and the run's size.
#[derive(Clone, Debug)]
pub struct Token {
    pub text: String,
    pub x: f32,
    pub y: f32,
    #[allow(dead_code)]
    pub size: f32,
}

/// One laid-out file, ready to rasterize and compare.
pub struct LaidOut {
    pub pages: Vec<RenderTarget>,
    /// One token list per page, in page order.
    pub tokens: Vec<Vec<Token>>,
    /// `word`, `slides` or `sheet`.
    pub kind: &'static str,
    /// Keeps the open document alive: its extracted pictures are files in a
    /// temporary folder owned by the binding. Dropping it would leave every
    /// picture path dangling before the pages are drawn.
    _document: Option<atrium_office::Document>,
}

/// Opens and lays out `path`.
pub fn load(path: &Path, shaper: &SystemShaper) -> Result<LaidOut> {
    let document =
        atrium_office::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut out = match &document.office {
        Office::Word(word) => {
            let out = atrium_layout::layout(word, shaper);
            finish(
                out.pages
                    .into_iter()
                    .map(|page| RenderTarget {
                        width_pt: page.width,
                        height_pt: page.height,
                        background: page.background,
                        items: page.items,
                    })
                    .collect(),
                shaper,
                "word",
            )?
        }
        Office::Slides(deck) => {
            let out = atrium_layout::layout_deck(deck, shaper);
            finish(
                out.pages
                    .into_iter()
                    .map(|page| RenderTarget {
                        width_pt: page.width,
                        height_pt: page.height,
                        background: page.background,
                        items: page.items,
                    })
                    .collect(),
                shaper,
                "slides",
            )?
        }
        Office::Sheet(workbook) => {
            let pages = sheet_pages(workbook, shaper);
            finish(pages, shaper, "sheet")?
        }
    };
    out._document = Some(document);
    Ok(out)
}

fn finish(pages: Vec<RenderTarget>, shaper: &SystemShaper, kind: &'static str) -> Result<LaidOut> {
    if pages.is_empty() {
        return Err(anyhow!("the layout produced no pages"));
    }
    let tokens = pages.iter().map(|p| tokens_of(shaper, &p.items)).collect();
    Ok(LaidOut {
        pages,
        tokens,
        kind,
        _document: None,
    })
}

/// Collects words with positions from Atrium's items. A CJK character is a
/// token of its own, so it lines up with the glyph-by-glyph PDF extraction.
pub fn tokens_of(shaper: &SystemShaper, items: &[Item]) -> Vec<Token> {
    let mut out = Vec::new();
    for item in items {
        let Item::Text {
            x,
            baseline,
            text,
            font,
            ..
        } = item
        else {
            continue;
        };
        if font.size <= 0.0 || text.trim().is_empty() {
            continue;
        }
        let clusters = shaper.shape(text, font);
        let mut current: Option<Token> = None;
        let mut cluster_x = 0.0;
        for (at, c) in text.char_indices() {
            let advance = clusters
                .iter()
                .find(|cluster| cluster.start == at)
                .map(|cluster| cluster.advance)
                .unwrap_or(0.0);
            let char_x = x + cluster_x;
            if c.is_whitespace() {
                if let Some(token) = current.take() {
                    out.push(token);
                }
            } else if is_cjk(c) {
                if let Some(token) = current.take() {
                    out.push(token);
                }
                out.push(Token {
                    text: c.to_string(),
                    x: char_x,
                    y: *baseline,
                    size: font.size,
                });
            } else {
                match &mut current {
                    Some(token) => token.text.push(c),
                    None => {
                        current = Some(Token {
                            text: c.to_string(),
                            x: char_x,
                            y: *baseline,
                            size: font.size,
                        })
                    }
                }
            }
            cluster_x += advance;
        }
        if let Some(token) = current {
            out.push(token);
        }
    }
    out
}

// ---------------------------------------------------------------- sheets

/// Excel's A4 at 100 %, with default margins. The model does not carry print
/// settings yet, so the tool uses the values both apps start from.
const PAGE_WIDTH: f32 = 595.28;
const PAGE_HEIGHT: f32 = 841.89;
const PAGE_MARGIN: f32 = 56.7;

/// Lays a workbook out as paged sheets. Rows and columns break at cell
/// boundaries; hidden rows and columns take no room.
pub fn sheet_pages(workbook: &Workbook, shaper: &SystemShaper) -> Vec<RenderTarget> {
    let mut out = Vec::new();
    for (sheet_index, sheet) in workbook.sheets().iter().enumerate() {
        if sheet.hidden {
            continue;
        }
        let Some((max_row, max_col)) = used_bounds(sheet) else {
            continue;
        };
        let (column_breaks, row_breaks) = page_breaks(sheet, max_row, max_col);
        for (row_band, col_band) in bands(&row_breaks, &column_breaks) {
            let origin_x = column_offset(sheet, col_band.0);
            let origin_y = row_offset(sheet, row_band.0);
            // A drawing prints on the page that holds its anchor cell; do not
            // copy it (or a sliver of it) onto every page.
            let mut band_sheet = sheet.clone();
            band_sheet.look.drawings = sheet
                .look
                .drawings
                .iter()
                .filter(|anchor| {
                    anchor.from.0 >= row_band.0
                        && anchor.from.0 <= row_band.1
                        && anchor.from.1 >= col_band.0
                        && anchor.from.1 <= col_band.1
                })
                .cloned()
                .collect();
            let drawings = atrium_layout::layout_sheet_drawings(&band_sheet, shaper);
            let mut items = Vec::new();
            for (&(r, c), cell) in &sheet.cells {
                if r < row_band.0 || r > row_band.1 || c < col_band.0 || c > col_band.1 {
                    continue;
                }
                if sheet.look.hidden_rows.contains(&r) || sheet.look.hidden_columns.contains(&c) {
                    continue;
                }
                cell_items(
                    sheet,
                    workbook,
                    sheet_index,
                    shaper,
                    &mut items,
                    r,
                    c,
                    cell,
                    origin_x,
                    origin_y,
                );
            }
            let shifted = shift_items(
                drawings.clone(),
                PAGE_MARGIN - origin_x,
                PAGE_MARGIN - origin_y,
            );
            let page = Rect {
                x: 0.0,
                y: 0.0,
                width: PAGE_WIDTH,
                height: PAGE_HEIGHT,
            };
            // Drop anything that still falls wholly outside this page.
            items.extend(
                shifted
                    .into_iter()
                    .filter(|item| item_bounds(item).is_some_and(|b| intersects(b, page))),
            );
            if items.is_empty() {
                continue;
            }
            out.push(RenderTarget {
                width_pt: PAGE_WIDTH,
                height_pt: PAGE_HEIGHT,
                background: None,
                items,
            });
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn cell_items(
    sheet: &atrium_office::Sheet,
    workbook: &Workbook,
    sheet_index: usize,
    shaper: &SystemShaper,
    items: &mut Vec<Item>,
    r: u32,
    c: u32,
    cell: &Cell,
    origin_x: f32,
    origin_y: f32,
) {
    if covered_by_merge(sheet, r, c) {
        return;
    }
    let (x, y, width, height) = cell_rect(sheet, r, c);
    let rect = Rect {
        x: x + PAGE_MARGIN - origin_x,
        y: y + PAGE_MARGIN - origin_y,
        width,
        height,
    };
    let shown = workbook.shown(sheet_index, r, c);
    if let Some(fill) = shown.style.fill {
        items.push(Item::Fill { rect, color: fill });
    }
    if let Some((fraction, color)) = shown.bar {
        items.push(Item::Fill {
            rect: Rect {
                x: rect.x + 1.0,
                y: rect.y + rect.height * 0.2,
                width: ((rect.width - 2.0) * fraction.clamp(0.0, 1.0)).max(0.0),
                height: (rect.height * 0.6).max(0.0),
            },
            color,
        });
    }
    for (border, from, to) in [
        (
            shown.style.borders.top,
            (rect.x, rect.y),
            (rect.x + rect.width, rect.y),
        ),
        (
            shown.style.borders.bottom,
            (rect.x, rect.y + rect.height),
            (rect.x + rect.width, rect.y + rect.height),
        ),
        (
            shown.style.borders.left,
            (rect.x, rect.y),
            (rect.x, rect.y + rect.height),
        ),
        (
            shown.style.borders.right,
            (rect.x + rect.width, rect.y),
            (rect.x + rect.width, rect.y + rect.height),
        ),
    ] {
        if let Some(border) = border {
            items.push(Item::Rule {
                from,
                to,
                width: (border.width as f32 / 20.0).max(0.5),
                color: border.color,
            });
        }
    }
    if sheet.look.gridlines && shown.style.borders.right.is_none() {
        items.push(Item::Rule {
            from: (rect.x + rect.width, rect.y),
            to: (rect.x + rect.width, rect.y + rect.height),
            width: 0.4,
            color: Rgb(0xCCCCCC),
        });
    }
    if shown.text.is_empty() {
        return;
    }
    let size = shown
        .style
        .font
        .size
        .map(|twips| twips as f32 / 20.0)
        .unwrap_or(11.0);
    let font = FontKey {
        family: shown.style.font.fonts.latin.clone(),
        size,
        bold: shown.style.font.bold,
        italic: shown.style.font.italic,
        cjk: false,
    };
    let metrics = shaper.metrics(&font);
    let text = shown.text.replace(['\n', '\r'], " ");
    let width: f32 = shaper.shape(&text, &font).iter().map(|c| c.advance).sum();
    let number = matches!(
        cell.value,
        Value::Number(_) | Value::Bool(_) | Value::Error(_)
    );
    let x = match shown.style.h_align {
        HAlign::Right => rect.x + rect.width - width - 3.0,
        HAlign::Center | HAlign::CenterAcross => rect.x + (rect.width - width) / 2.0,
        HAlign::General if number => rect.x + rect.width - width - 3.0,
        _ => rect.x + 3.0,
    };
    let baseline = match shown.style.v_align {
        VAlign::Top => rect.y + metrics.ascent + 3.0,
        VAlign::Center => rect.y + rect.height / 2.0 + (metrics.ascent - metrics.descent) / 2.0,
        VAlign::Bottom => rect.y + rect.height - metrics.descent - 3.0,
    };
    items.push(Item::Text {
        x,
        baseline,
        text,
        font,
        color: shown.style.font.color.unwrap_or(Rgb::BLACK),
        rotation: 0.0,
        center: (0.0, 0.0),
    });
}

fn covered_by_merge(sheet: &atrium_office::Sheet, r: u32, c: u32) -> bool {
    sheet
        .merged
        .iter()
        .any(|&(r0, c0, r1, c1)| (r, c) != (r0, c0) && r >= r0 && r <= r1 && c >= c0 && c <= c1)
}

fn cell_rect(sheet: &atrium_office::Sheet, r: u32, c: u32) -> (f32, f32, f32, f32) {
    let mut x = column_offset(sheet, c);
    let mut y = row_offset(sheet, r);
    let mut width = column_width(sheet, c);
    let mut height = row_height(sheet, r);
    for &(r0, c0, r1, c1) in &sheet.merged {
        if (r0, c0) == (r, c) {
            width = (c0..=c1).map(|col| column_width(sheet, col)).sum();
            height = (r0..=r1).map(|row| row_height(sheet, row)).sum();
            x = column_offset(sheet, c0);
            y = row_offset(sheet, r0);
            break;
        }
    }
    (x, y, width, height)
}

fn column_width(sheet: &atrium_office::Sheet, c: u32) -> f32 {
    if sheet.look.hidden_columns.contains(&c) {
        0.0
    } else {
        sheet
            .look
            .columns
            .get(&c)
            .copied()
            .unwrap_or(sheet.look.default_width)
            .max(0.0)
    }
}

fn row_height(sheet: &atrium_office::Sheet, r: u32) -> f32 {
    if sheet.look.hidden_rows.contains(&r) {
        0.0
    } else {
        sheet
            .look
            .rows
            .get(&r)
            .copied()
            .unwrap_or(sheet.look.default_height)
            .max(0.0)
    }
}

fn column_offset(sheet: &atrium_office::Sheet, c: u32) -> f32 {
    (0..c).map(|col| column_width(sheet, col)).sum()
}

fn row_offset(sheet: &atrium_office::Sheet, r: u32) -> f32 {
    (0..r).map(|row| row_height(sheet, row)).sum()
}

/// The last row and column that hold anything the sheet shows.
fn used_bounds(sheet: &atrium_office::Sheet) -> Option<(u32, u32)> {
    let mut max_row: Option<u32> = None;
    let mut max_col: Option<u32> = None;
    let mut note = |r: u32, c: u32| {
        max_row = Some(max_row.map_or(r, |m: u32| m.max(r)));
        max_col = Some(max_col.map_or(c, |m: u32| m.max(c)));
    };
    for &(r, c) in sheet.cells.keys() {
        note(r, c);
    }
    for &(r0, c0, r1, c1) in &sheet.merged {
        note(r0, c0);
        note(r1, c1);
    }
    for &r in sheet.look.rows.keys() {
        note(r, 0);
    }
    for &c in sheet.look.columns.keys() {
        note(0, c);
    }
    for anchored in &sheet.look.drawings {
        note(anchored.to.0, anchored.to.1);
    }
    Some((max_row?, max_col?))
}

/// Column or row indexes at which a new page starts (the first index on the
/// next page), plus a terminator one past the last used index.
fn page_breaks(sheet: &atrium_office::Sheet, max_row: u32, max_col: u32) -> (Vec<u32>, Vec<u32>) {
    let content_w = PAGE_WIDTH - 2.0 * PAGE_MARGIN;
    let content_h = PAGE_HEIGHT - 2.0 * PAGE_MARGIN;
    let mut cols = vec![0];
    let mut x = 0.0;
    for c in 0..=max_col {
        let width = column_width(sheet, c);
        if width > 0.0 && x > 0.0 && x + width > content_w {
            cols.push(c);
            x = 0.0;
        }
        x += width;
    }
    cols.push(max_col + 1);
    let mut rows = vec![0];
    let mut y = 0.0;
    for r in 0..=max_row {
        let height = row_height(sheet, r);
        if height > 0.0 && y > 0.0 && y + height > content_h {
            rows.push(r);
            y = 0.0;
        }
        y += height;
    }
    rows.push(max_row + 1);
    (cols, rows)
}

fn bands(rows: &[u32], cols: &[u32]) -> Vec<((u32, u32), (u32, u32))> {
    let rows: Vec<(u32, u32)> = rows
        .windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| (w[0], w[1] - 1))
        .collect();
    let cols: Vec<(u32, u32)> = cols
        .windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| (w[0], w[1] - 1))
        .collect();
    let mut out = Vec::new();
    for row in &rows {
        for col in &cols {
            out.push((*row, *col));
        }
    }
    out
}

fn shift_items(items: Vec<Item>, dx: f32, dy: f32) -> Vec<Item> {
    items
        .into_iter()
        .map(|item| shift_item(item, dx, dy))
        .collect()
}

fn shift_item(item: Item, dx: f32, dy: f32) -> Item {
    match item {
        Item::Text {
            x,
            baseline,
            text,
            font,
            color,
            rotation,
            center,
        } => Item::Text {
            x: x + dx,
            baseline: baseline + dy,
            text,
            font,
            color,
            rotation,
            center: (center.0 + dx, center.1 + dy),
        },
        Item::Fill { rect, color } => Item::Fill {
            rect: shift_rect(rect, dx, dy),
            color,
        },
        Item::Rule {
            from,
            to,
            width,
            color,
        } => Item::Rule {
            from: (from.0 + dx, from.1 + dy),
            to: (to.0 + dx, to.1 + dy),
            width,
            color,
        },
        Item::Picture { rect, path } => Item::Picture {
            rect: shift_rect(rect, dx, dy),
            path,
        },
        Item::Path {
            segments,
            fill,
            stroke,
            rotation,
            center,
        } => Item::Path {
            segments: segments
                .into_iter()
                .map(|seg| match seg {
                    Seg::Move(x, y) => Seg::Move(x + dx, y + dy),
                    Seg::Line(x, y) => Seg::Line(x + dx, y + dy),
                    Seg::Quad(cx, cy, x, y) => Seg::Quad(cx + dx, cy + dy, x + dx, y + dy),
                    Seg::Cubic(c1x, c1y, c2x, c2y, x, y) => {
                        Seg::Cubic(c1x + dx, c1y + dy, c2x + dx, c2y + dy, x + dx, y + dy)
                    }
                    Seg::Close => Seg::Close,
                })
                .collect(),
            fill,
            stroke,
            rotation,
            center: (center.0 + dx, center.1 + dy),
        },
        Item::Gradient { rect, angle, stops } => Item::Gradient {
            rect: shift_rect(rect, dx, dy),
            angle,
            stops,
        },
    }
}

fn shift_rect(rect: Rect, dx: f32, dy: f32) -> Rect {
    Rect {
        x: rect.x + dx,
        y: rect.y + dy,
        ..rect
    }
}

/// A rough box around an item, enough to decide which page it is on.
fn item_bounds(item: &Item) -> Option<Rect> {
    Some(match item {
        Item::Text {
            x,
            baseline,
            text,
            font,
            ..
        } => Rect {
            x: *x,
            y: baseline - font.size,
            // A generous upper bound: the exact width needs the shaper.
            width: font.size * text.chars().count().max(1) as f32,
            height: font.size * 1.4,
        },
        Item::Fill { rect, .. } | Item::Picture { rect, .. } | Item::Gradient { rect, .. } => *rect,
        Item::Rule { from, to, .. } => Rect {
            x: from.0.min(to.0),
            y: from.1.min(to.1),
            width: (to.0 - from.0).abs().max(0.5),
            height: (to.1 - from.1).abs().max(0.5),
        },
        Item::Path { segments, .. } => {
            let mut min_x = f32::MAX;
            let mut min_y = f32::MAX;
            let mut max_x = f32::MIN;
            let mut max_y = f32::MIN;
            for seg in segments {
                let (x, y) = match *seg {
                    Seg::Move(x, y) | Seg::Line(x, y) => (x, y),
                    Seg::Quad(_, _, x, y) | Seg::Cubic(_, _, _, _, x, y) => (x, y),
                    Seg::Close => continue,
                };
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
            if min_x == f32::MAX {
                return None;
            }
            Rect {
                x: min_x,
                y: min_y,
                width: (max_x - min_x).max(0.5),
                height: (max_y - min_y).max(0.5),
            }
        }
    })
}

fn intersects(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}
