//! Tables: columns, cells, merges, borders, rows and row fragmentation.

use atrium_office::look::{Border, CellLook, Rgb, TableLook, VAlign};
use atrium_office::{Block, Table};

use crate::{Item, Rect};

use super::{Ctx, Laid, Seg, lines, pt};

#[derive(Clone)]
pub(crate) struct Row {
    pub height: f32,
    pub lines: Vec<(f32, Laid)>,
    pub items: Vec<Item>,
    pub cant_split: bool,
    pub header: bool,
}

fn grid_count(table: &Table) -> usize {
    let mut widest = 1;
    for (r, row) in table.rows.iter().enumerate() {
        let mut c = 0;
        let mut count = 0;
        while c < row.len() {
            let mut span = 1;
            while table.merged.contains(&(r, c + span)) {
                span += 1;
            }
            c += span;
            count += span;
        }
        widest = widest.max(count);
    }
    widest
}

/// The grid, scaled to the text area. Without a grid, columns fit their
/// contents.
pub(crate) fn columns(cx: &mut Ctx<'_>, table: &Table, width: f32) -> Vec<f32> {
    let count = grid_count(table);
    if table.look.columns.len() == count {
        let cols: Vec<f32> = table.look.columns.iter().map(|c| pt(*c)).collect();
        let total: f32 = cols.iter().sum();
        if total > width && total > 0.0 {
            return cols.iter().map(|c| c * width / total).collect();
        }
        return cols;
    }
    if table.look == TableLook::default() {
        return vec![width / count as f32; count];
    }
    if table.look.columns.is_empty() {
        let mut natural = vec![0.0f32; count];
        for (r, row) in table.rows.iter().enumerate() {
            let mut col = 0;
            let mut c = 0;
            while c < row.len() {
                let mut span = 1;
                while table.merged.contains(&(r, c + span)) {
                    span += 1;
                }
                let content: f32 = cell_natural(cx, &row[c]);
                let padx = pt(table.look.padding[1] + table.look.padding[3]);
                for k in 0..span {
                    if col + k < count {
                        natural[col + k] = natural[col + k].max(content / span as f32 + padx);
                    }
                }
                col += span;
                c += span;
            }
        }
        let total: f32 = natural.iter().sum();
        if total <= width && total > 0.0 {
            return natural;
        }
    }
    vec![width / count as f32; count]
}

fn cell_natural(cx: &mut Ctx<'_>, cell: &[Block]) -> f32 {
    let mut widest: f32 = 0.0;
    for block in cell {
        if let Block::Paragraph(p) = block {
            widest = widest.max(lines::natural_width(cx, p));
        }
    }
    widest
}

struct Spec {
    row: usize,
    col: usize,
    colspan: usize,
    vspan: usize,
    x: f32,
    width: f32,
    content: Vec<(f32, Laid)>,
    extra: Vec<Item>,
    content_h: f32,
    pad: [f32; 4],
    look: CellLook,
}

fn max_border(a: Option<Border>, b: Option<Border>) -> Option<Border> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.width > a.width { b } else { a }),
        (a, b) => a.or(b),
    }
}

fn effective(
    cell: Option<Border>,
    table: Option<Border>,
    fallback: Option<Border>,
) -> Option<Border> {
    cell.or(table).or(fallback)
}

fn clip(item: &Item, from: f32, to: f32, shift: f32) -> Option<Item> {
    match item {
        Item::Fill { rect, color } => {
            let top = rect.y.max(from);
            let bottom = (rect.y + rect.height).min(to);
            if bottom > top {
                Some(Item::Fill {
                    rect: Rect {
                        x: rect.x,
                        y: top - from + shift,
                        width: rect.width,
                        height: bottom - top,
                    },
                    color: *color,
                })
            } else {
                None
            }
        }
        Item::Rule {
            from: a,
            to: b,
            width,
            color,
        } => {
            if (a.0 - b.0).abs() < 0.01 {
                let top = a.1.min(b.1);
                let bottom = a.1.max(b.1);
                let top = top.max(from);
                let bottom = bottom.min(to);
                if bottom > top {
                    Some(Item::Rule {
                        from: (a.0, top - from + shift),
                        to: (a.0, bottom - from + shift),
                        width: *width,
                        color: *color,
                    })
                } else {
                    None
                }
            } else if a.1 >= from - 0.01 && a.1 <= to + 0.01 {
                Some(Item::Rule {
                    from: (a.0, a.1 - from + shift),
                    to: (b.0, b.1 - from + shift),
                    width: *width,
                    color: *color,
                })
            } else {
                None
            }
        }
        Item::Picture { rect, path } => (rect.y >= from && rect.y < to).then(|| Item::Picture {
            rect: Rect {
                y: rect.y - from + shift,
                ..*rect
            },
            path: path.clone(),
        }),
        Item::Gradient { rect, angle, stops } => {
            (rect.y >= from && rect.y < to).then(|| Item::Gradient {
                rect: Rect {
                    y: rect.y - from + shift,
                    ..*rect
                },
                angle: *angle,
                stops: stops.clone(),
            })
        }
        Item::Path {
            segments,
            fill,
            stroke,
            rotation,
            center,
        } => {
            let first = segments.iter().find_map(|s| match s {
                Seg::Move(_, y) | Seg::Line(_, y) => Some(*y),
                _ => None,
            });
            first.filter(|y| *y >= from && *y < to).map(|_| Item::Path {
                segments: segments.clone(),
                fill: *fill,
                stroke: *stroke,
                rotation: *rotation,
                center: *center,
            })
        }
        Item::Text {
            x,
            baseline,
            text,
            font,
            color,
            rotation,
            center,
        } => (*baseline >= from && *baseline < to).then(|| Item::Text {
            x: *x,
            baseline: baseline - from + shift,
            text: text.clone(),
            font: font.clone(),
            color: *color,
            rotation: *rotation,
            center: (center.0, center.1 - from + shift),
        }),
    }
}

impl Row {
    /// The largest line boundary that fits in `available`.
    pub fn cut(&self, available: f32) -> f32 {
        let mut cut: f32 = 0.0;
        for (y, line) in &self.lines {
            if *y + line.height <= available + 0.01 {
                cut = cut.max(*y + line.height);
            }
        }
        cut
    }

    pub fn split_at(&self, cut: f32) -> (Row, Row) {
        let mut first_lines = Vec::new();
        let mut then_lines = Vec::new();
        for (y, line) in &self.lines {
            if *y + line.height <= cut + 0.01 {
                first_lines.push((*y, line.clone()));
            } else {
                then_lines.push((*y - cut, line.clone()));
            }
        }
        let first_items = self
            .items
            .iter()
            .filter_map(|i| clip(i, 0.0, cut, 0.0))
            .collect();
        let then_items = self
            .items
            .iter()
            .filter_map(|i| clip(i, cut, self.height, -cut))
            .collect();
        (
            Row {
                height: cut,
                lines: first_lines,
                items: first_items,
                cant_split: self.cant_split,
                header: self.header,
            },
            Row {
                height: (self.height - cut).max(0.0),
                lines: then_lines,
                items: then_items,
                cant_split: self.cant_split,
                header: false,
            },
        )
    }
}

/// Lays a table at the origin; returns local-coordinate rows and its height.
pub(crate) fn table(cx: &mut Ctx<'_>, t: &Table, width: f32) -> (Vec<Row>, Vec<Item>, f32) {
    let cols = columns(cx, t, width);
    let grid = cols.len().max(1);
    let default_look = t.look == TableLook::default();
    let pad = if default_look {
        [0.0, 5.4, 0.0, 5.4]
    } else {
        t.look.padding.map(pt)
    };
    let fallback = default_look.then_some(Border {
        width: 10,
        color: Rgb::BLACK,
        space: 0,
    });
    let rows_n = t.rows.len();
    let mut heights = vec![0.0f32; rows_n];
    let mut cover: Vec<Vec<Option<usize>>> = vec![vec![None; grid]; rows_n];
    let mut specs: Vec<Spec> = Vec::new();
    let mut rows_out: Vec<Row> = (0..rows_n)
        .map(|r| Row {
            height: 0.0,
            lines: Vec::new(),
            items: Vec::new(),
            cant_split: t.look.cant_split.get(r).copied().unwrap_or(false),
            header: r < t.look.header_rows,
        })
        .collect();
    let mut row_lines: Vec<Vec<(f32, Laid)>> = vec![Vec::new(); rows_n];
    let mut row_items: Vec<Vec<Item>> = vec![Vec::new(); rows_n];

    for (r, row) in t.rows.iter().enumerate() {
        let mut c = 0;
        while c < row.len() {
            let mut colspan = 1;
            while t.merged.contains(&(r, c + colspan)) {
                colspan += 1;
            }
            let look = t
                .look
                .cells
                .get(r)
                .and_then(|cells| cells.get(c))
                .cloned()
                .unwrap_or_default();
            if look.merged_up {
                c += colspan;
                continue;
            }
            let mut vspan = 1;
            while t
                .rows
                .get(r + vspan)
                .and_then(|row| row.get(c))
                .and_then(|_| t.look.cells.get(r + vspan))
                .and_then(|cells| cells.get(c))
                .is_some_and(|cell| cell.merged_up)
            {
                vspan += 1;
            }
            let width: f32 = cols[c.min(cols.len() - 1)..(c + colspan).min(cols.len())]
                .iter()
                .sum();
            let x: f32 = cols[..c.min(cols.len())].iter().sum();
            let cell_pad = if look.padding == [0; 4] {
                pad
            } else {
                look.padding.map(pt)
            };
            let inner = (width - cell_pad[1] - cell_pad[3]).max(1.0);
            // Rows grow with their content. LibreOffice (and Word's compact
            // table rows) do not stretch every cell line to the section's
            // document grid, which would make a 15.6 pt grid give 31.2 pt
            // rows whenever the CJK face is slightly taller than the pitch.
            let line_pitch = cx.line_pitch.take();
            let (placed, extra, content_h) = lines::stack(cx, &row[c], inner);
            cx.line_pitch = line_pitch;
            let index = specs.len();
            for row in &mut cover[r..(r + vspan).min(rows_n)] {
                for cell in &mut row[c..(c + colspan).min(grid)] {
                    *cell = Some(index);
                }
            }
            specs.push(Spec {
                row: r,
                col: c,
                colspan,
                vspan,
                x,
                width,
                content: placed,
                extra,
                content_h,
                pad: cell_pad,
                look,
            });
            c += colspan;
        }
    }

    // Row heights: single-row cells first, then make room for merged spans.
    for spec in &specs {
        if spec.vspan == 1 {
            heights[spec.row] = heights[spec.row].max(spec.content_h + spec.pad[0] + spec.pad[2]);
        }
    }
    for (r, height) in heights.iter_mut().enumerate() {
        if let Some(Some((h, exact))) = t.look.rows.get(r) {
            *height = if *exact { pt(*h) } else { height.max(pt(*h)) };
        }
    }
    for spec in &specs {
        if spec.vspan <= 1 {
            continue;
        }
        let need = spec.content_h + spec.pad[0] + spec.pad[2];
        let last = (spec.row + spec.vspan - 1).min(rows_n - 1);
        let total: f32 = heights[spec.row..=last].iter().sum();
        if total < need {
            heights[last] += need - total;
        }
    }

    // Draw each cell once, at the top-left of its merged span.
    for spec in &specs {
        let y0: f32 = heights[..spec.row].iter().sum();
        let cell_h: f32 = heights[spec.row..(spec.row + spec.vspan).min(rows_n)]
            .iter()
            .sum();
        let items = &mut row_items[spec.row];
        if let Some(color) = spec.look.shading {
            items.push(Item::Fill {
                rect: Rect {
                    x: spec.x,
                    y: y0,
                    width: spec.width,
                    height: cell_h,
                },
                color,
            });
        }
        for item in &spec.extra {
            items.push(super::translate(
                item,
                spec.x + spec.pad[3],
                y0 + spec.pad[0],
            ));
        }
        let free = cell_h - spec.pad[0] - spec.pad[2] - spec.content_h;
        // Lines sit relative to the row that holds the cell, like the items
        // (which are shifted back by the row's top below).
        let dy = spec.pad[0]
            + match spec.look.v_align {
                VAlign::Top => 0.0,
                VAlign::Center => free / 2.0,
                VAlign::Bottom => free,
            };
        for (ly, mut line) in spec.content.iter().cloned() {
            line.items = line
                .items
                .iter()
                .map(|i| super::translate(i, spec.x + spec.pad[3], 0.0))
                .collect();
            for s in line.stops.iter_mut() {
                s.1 += spec.x + spec.pad[3];
            }
            row_lines[spec.row].push((dy + ly, line));
        }

        if default_look {
            let b = fallback.unwrap();
            for (from, to) in [
                ((spec.x, y0), (spec.x + spec.width, y0)),
                ((spec.x, y0), (spec.x, y0 + cell_h)),
                (
                    (spec.x + spec.width, y0),
                    (spec.x + spec.width, y0 + cell_h),
                ),
                ((spec.x, y0 + cell_h), (spec.x + spec.width, y0 + cell_h)),
            ] {
                items.push(Item::Rule {
                    from,
                    to,
                    width: pt(b.width).max(0.5),
                    color: b.color,
                });
            }
            continue;
        }

        let domain = t.look.borders;
        let cell = spec.look.borders;
        let above = (spec.row > 0)
            .then(|| cover[spec.row - 1][spec.col])
            .flatten();
        let right = ((spec.col + spec.colspan) < grid)
            .then(|| cover[spec.row][spec.col + spec.colspan])
            .flatten();
        let same_above = above.is_some_and(|i| {
            let s = &specs[i];
            s.row + s.vspan > spec.row && s.col <= spec.col && s.col + s.colspan > spec.col
        });
        let top = if spec.row == 0 {
            effective(cell.and_then(|b| b.top), domain.top, fallback)
        } else if same_above {
            None
        } else {
            let at = above.map(|i| {
                let s = &specs[i];
                effective(
                    s.look.borders.and_then(|b| b.bottom),
                    if s.row == 0 {
                        domain.top
                    } else {
                        t.look.inside_h
                    },
                    fallback,
                )
            });
            max_border(
                effective(cell.and_then(|b| b.top), t.look.inside_h, fallback),
                at.flatten(),
            )
        };
        let bottom = if spec.row + spec.vspan >= rows_n {
            effective(cell.and_then(|b| b.bottom), domain.bottom, fallback)
        } else {
            None
        };
        let left_border = if spec.col == 0 {
            effective(cell.and_then(|b| b.left), domain.left, fallback)
        } else {
            None
        };
        let right_border = if spec.col + spec.colspan >= grid {
            effective(cell.and_then(|b| b.right), domain.right, fallback)
        } else {
            let other = right.map(|i| {
                let s = &specs[i];
                let table = if s.col == 0 {
                    domain.left
                } else {
                    t.look.inside_v
                };
                effective(s.look.borders.and_then(|b| b.left), table, fallback)
            });
            max_border(
                effective(cell.and_then(|b| b.right), t.look.inside_v, fallback),
                other.flatten(),
            )
        };
        if let Some(b) = left_border {
            items.push(Item::Rule {
                from: (spec.x, y0),
                to: (spec.x, y0 + cell_h),
                width: pt(b.width).max(0.5),
                color: b.color,
            });
        }
        if let Some(b) = right_border {
            items.push(Item::Rule {
                from: (spec.x + spec.width, y0),
                to: (spec.x + spec.width, y0 + cell_h),
                width: pt(b.width).max(0.5),
                color: b.color,
            });
        }
        if let Some(b) = top {
            items.push(Item::Rule {
                from: (spec.x, y0),
                to: (spec.x + spec.width, y0),
                width: pt(b.width).max(0.5),
                color: b.color,
            });
        }
        if let Some(b) = bottom {
            items.push(Item::Rule {
                from: (spec.x, y0 + cell_h),
                to: (spec.x + spec.width, y0 + cell_h),
                width: pt(b.width).max(0.5),
                color: b.color,
            });
        }
    }

    let mut y = 0.0;
    for (r, height) in heights.iter().enumerate() {
        rows_out[r].height = *height;
        rows_out[r].lines = std::mem::take(&mut row_lines[r]);
        rows_out[r].items = std::mem::take(&mut row_items[r])
            .into_iter()
            .map(|i| super::translate(&i, 0.0, 0.0 - y))
            .collect();
        y += height;
    }
    (rows_out, Vec::new(), y)
}
