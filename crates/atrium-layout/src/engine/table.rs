//! Tables: columns, cells, rows, borders and row pagination.

use atrium_office::look::{Border, CellLook, Rgb, TableLook, VAlign};
use atrium_office::{Block, Table};

use crate::{Item, Rect};

use super::{Ctx, Laid, lines, pt};

pub(crate) struct Row {
    pub y: f32,
    pub height: f32,
    pub lines: Vec<(f32, Laid)>,
    pub items: Vec<Item>,
}

/// The grid, scaled to the text area. Without a grid, columns fit their
/// contents.
pub(crate) fn columns(cx: &mut Ctx<'_>, table: &Table, width: f32) -> Vec<f32> {
    let count = table
        .rows
        .iter()
        .map(|row| row.len())
        .max()
        .unwrap_or(1)
        .max(1);
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
                for k in 0..span {
                    if col + k < count {
                        natural[col + k] = natural[col + k].max(content / span as f32 + 10.8);
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

fn border_of(b: Option<Border>, fallback: Option<Border>) -> Option<Border> {
    b.or(fallback)
}

/// Lays a table at the origin; returns its rows, its fills and borders and
/// its height.
pub(crate) fn table(cx: &mut Ctx<'_>, t: &Table, width: f32) -> (Vec<Row>, Vec<Item>, f32) {
    let cols = columns(cx, t, width);
    let default_look = t.look == TableLook::default();
    let pad = if default_look {
        [0.0, 5.4, 0.0, 5.4]
    } else {
        t.look.padding.map(pt)
    };
    let grid = Border {
        width: 10,
        color: Rgb::BLACK,
        space: 0,
    };
    let fallback = default_look.then_some(grid);
    let mut rows = Vec::new();
    let mut y = 0.0;
    for (r, row) in t.rows.iter().enumerate() {
        let mut items = Vec::new();
        let mut cells = Vec::new();
        let mut height: f32 = 0.0;
        let mut x = 0.0;
        let mut c = 0;
        while c < row.len() {
            let mut span = 1;
            while t.merged.contains(&(r, c + span)) {
                span += 1;
            }
            let w: f32 = cols[c.min(cols.len() - 1)..(c + span).min(cols.len())]
                .iter()
                .sum();
            let inner = (w - pad[1] - pad[3]).max(1.0);
            let (placed, extra, h) = lines::stack(cx, &row[c], inner);
            height = height.max(h + pad[0] + pad[2]);
            cells.push((c, x, w, placed, extra, h));
            x += w;
            c += span;
        }
        if let Some(Some((h, exact))) = t.look.rows.get(r) {
            height = if *exact { pt(*h) } else { height.max(pt(*h)) };
        }
        let mut laid = Vec::new();
        for (c, x, w, placed, extra, h) in cells {
            let look: Option<&CellLook> = t.look.cells.get(r).and_then(|row| row.get(c));
            if let Some(color) = look.and_then(|l| l.shading) {
                items.push(Item::Fill {
                    rect: Rect {
                        x,
                        y,
                        width: w,
                        height,
                    },
                    color,
                });
            }
            let free = height - pad[0] - pad[2] - h;
            let dy = pad[0]
                + match look.map(|l| l.v_align).unwrap_or_default() {
                    VAlign::Top => 0.0,
                    VAlign::Center => free / 2.0,
                    VAlign::Bottom => free,
                };
            for item in extra {
                items.push(super::translate(&item, x + pad[3], y + dy));
            }
            for (ly, mut line) in placed {
                line.items = line
                    .items
                    .iter()
                    .map(|i| super::translate(i, x + pad[3], 0.0))
                    .collect();
                for s in line.stops.iter_mut() {
                    s.1 += x + pad[3];
                }
                laid.push((dy + ly, line));
            }
            let borders = look.and_then(|l| l.borders).unwrap_or(t.look.borders);
            let left = if c == 0 {
                border_of(borders.left, fallback)
            } else {
                border_of(t.look.inside_v.or(borders.left), fallback)
            };
            let top = if r == 0 {
                border_of(borders.top, fallback)
            } else {
                border_of(t.look.inside_h.or(borders.top), fallback)
            };
            let sides = [
                (top, (x, y), (x + w, y)),
                (left, (x, y), (x, y + height)),
                (
                    border_of(borders.right, fallback),
                    (x + w, y),
                    (x + w, y + height),
                ),
                (
                    border_of(borders.bottom, fallback),
                    (x, y + height),
                    (x + w, y + height),
                ),
            ];
            for (b, from, to) in sides {
                if let Some(b) = b {
                    items.push(Item::Rule {
                        from,
                        to,
                        width: pt(b.width).max(0.5),
                        color: b.color,
                    });
                }
            }
        }
        rows.push(Row {
            y,
            height,
            lines: laid,
            items,
        });
        y += height;
    }
    (rows, Vec::new(), y)
}
