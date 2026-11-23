//! Slide layout and painting: shapes in z-order, their fills, outlines,
//! pictures, tables, charts and text.

use crate::engine::{Ctx, pt, translate};
use crate::{Item, Layout, Line, Page, Pen, Rect, Seg, Shaper, chart};
use atrium_office::look::Rgb;
use atrium_office::look::{Fill, PathCmd, ShapeKind, VAlign, VText};
use atrium_office::{Marks, Shape, Slide};
use std::collections::HashMap;

const SLIDE_PT: f32 = 18.0;

pub(crate) fn layout(slide: &Slide, index: usize, shaper: &dyn Shaper) -> Layout {
    let defaults = Marks::default();
    let mut cx = Ctx {
        shaper,
        defaults: &defaults,
        base_size: SLIDE_PT,
        counters: HashMap::new(),
        page_number: index as u32 + 1,
        pages_total: 1,
        last_table_height: 0.0,
    };
    let mut page = Page {
        width: slide.size.0 as f32,
        height: slide.size.1 as f32,
        number: index as u32 + 1,
        background: slide.background.or(Some(Rgb::WHITE)),
        items: Vec::new(),
    };
    let mut lines = Vec::new();
    for shape in &slide.shapes {
        shape_items(&mut cx, shape, index, &mut page, &mut lines);
    }
    Layout {
        pages: vec![page],
        lines,
    }
}

fn shape_items(
    cx: &mut Ctx<'_>,
    shape: &Shape,
    index: usize,
    page: &mut Page,
    lines: &mut Vec<Line>,
) {
    let [x, y, w, h] = shape.frame.map(|v| v as f32);
    let rect = Rect {
        x,
        y,
        width: w.max(1.0),
        height: h.max(1.0),
    };
    match &shape.kind {
        ShapeKind::Table(table) => {
            paint_look(shape, rect, page);
            let (rows, items) = cx.table(table, rect.width);
            page.items.extend(items.iter().map(|i| translate(i, x, y)));
            for (row_y, laid) in rows {
                for (ly, line) in laid {
                    let top = y + row_y + ly;
                    page.items
                        .extend(line.items.iter().map(|i| translate(i, x, top)));
                    let furniture = shape
                        .text_paras()
                        .find(|p| p.id == line.para)
                        .is_some_and(|p| p.locked);
                    lines.push(Line {
                        page: index,
                        para: line.para,
                        range: line.range,
                        top,
                        height: line.height,
                        baseline: top + line.baseline,
                        stops: line.stops.iter().map(|(o, sx)| (*o, sx + x)).collect(),
                        furniture,
                    });
                }
            }
        }
        ShapeKind::Chart(c) => {
            paint_look(shape, rect, page);
            page.items.extend(chart::draw(c, rect, cx.shaper));
        }
        ShapeKind::Fallback(path) => {
            if path.is_some() {
                page.items.push(rotate(
                    Item::Picture {
                        rect,
                        path: path.clone(),
                    },
                    shape,
                    rect,
                ));
            }
        }
        ShapeKind::Picture => {
            paint_look(shape, rect, page);
        }
        ShapeKind::Text => {
            let insets = if shape.look.insets == [0; 4] {
                [3.6, 7.2, 3.6, 7.2]
            } else {
                shape.look.insets.map(pt)
            };
            let inner_w = (rect.width - insets[1] - insets[3]).max(1.0);
            let scale = shape
                .look
                .font_scale
                .map(|s| s as f32 / 1000.0)
                .unwrap_or(1.0);
            cx.base_size = SLIDE_PT * scale;
            let mut placed = Vec::new();
            let mut ty = 0.0;
            for p in &shape.paras {
                let b = cx.para(p, inner_w, false);
                ty += b.before;
                for line in b.lines {
                    let lh = line.height;
                    placed.push((ty, line));
                    ty += lh;
                }
                ty += b.after;
            }
            let needed = ty + insets[0] + insets[2];
            let height = if shape.look.auto_grow {
                rect.height.max(needed)
            } else {
                rect.height
            };
            let rect = Rect { height, ..rect };
            paint_look(shape, rect, page);
            let free = height - insets[0] - insets[2] - ty;
            let dy = y
                + insets[0]
                + match shape.look.v_align {
                    VAlign::Top => 0.0,
                    VAlign::Center => free / 2.0,
                    VAlign::Bottom => free,
                };
            let dx = x + insets[3];
            for (ly, line) in placed {
                let furniture = shape.paras.iter().any(|p| p.id == line.para && p.locked);
                for item in &line.items {
                    page.items
                        .push(rotate(translate(item, dx, dy + ly), shape, rect));
                }
                lines.push(Line {
                    page: index,
                    para: line.para,
                    range: line.range,
                    top: dy + ly,
                    height: line.height,
                    baseline: dy + ly + line.baseline,
                    stops: line.stops.iter().map(|(o, sx)| (*o, sx + dx)).collect(),
                    furniture,
                });
            }
        }
    }
}

fn rotate(item: Item, shape: &Shape, rect: Rect) -> Item {
    let rotation = shape.look.rotation as f32
        + match shape.look.vert {
            VText::Horizontal => 0.0,
            VText::Vertical => 90.0,
            VText::Vertical270 => 270.0,
            VText::WordArtVertical => 0.0,
        };
    let center = (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    match item {
        Item::Text {
            x,
            baseline,
            text,
            font,
            color,
            rotation: _,
            center: _,
        } => Item::Text {
            x,
            baseline,
            text,
            font,
            color,
            rotation,
            center,
        },
        Item::Path {
            segments,
            fill,
            stroke,
            rotation: _,
            center: _,
        } => Item::Path {
            segments,
            fill,
            stroke,
            rotation,
            center,
        },
        other => other,
    }
}

fn paint_look(shape: &Shape, rect: Rect, page: &mut Page) {
    let look = &shape.look;
    let segments = path_segments(look.path.as_slice(), rect);
    if let Some(shadow) = look.shadow {
        let angle = shadow.direction as f32 * std::f32::consts::PI / 180.0;
        let dx = shadow.distance as f32 / 20.0 * angle.cos();
        let dy = shadow.distance as f32 / 20.0 * angle.sin();
        page.items.push(Item::Path {
            segments: segments.iter().map(|s| shift(*s, dx, dy)).collect(),
            fill: Some((shadow.color, shadow.alpha)),
            stroke: None,
            rotation: 0.0,
            center: (0.0, 0.0),
        });
    }
    let plain = look.path.is_empty();
    match &look.fill {
        Fill::None => {}
        Fill::Solid(color, 255) if plain => page.items.push(Item::Fill {
            rect,
            color: *color,
        }),
        Fill::Solid(color, alpha) => page.items.push(Item::Path {
            segments: segments.clone(),
            fill: Some((*color, *alpha)),
            stroke: None,
            rotation: 0.0,
            center: (0.0, 0.0),
        }),
        Fill::Gradient { stops, angle, .. } if plain => page.items.push(Item::Gradient {
            rect,
            angle: *angle as f32,
            stops: stops
                .iter()
                .map(|(at, c, a)| (*at as f32 / 1000.0, *c, *a))
                .collect(),
        }),
        Fill::Gradient { stops, .. } => {
            if let Some((_, c, a)) = stops.first() {
                page.items.push(Item::Path {
                    segments: segments.clone(),
                    fill: Some((*c, *a)),
                    stroke: None,
                    rotation: 0.0,
                    center: (0.0, 0.0),
                });
            }
        }
        Fill::Picture { path, .. } => page.items.push(Item::Picture {
            rect,
            path: Some(path.clone()),
        }),
    }
    if let Some(stroke) = look.line {
        page.items.push(Item::Path {
            segments,
            fill: None,
            stroke: Some(Pen {
                width: pt(stroke.width).max(0.5),
                color: stroke.color,
                alpha: stroke.alpha,
                dash: stroke.dash,
            }),
            rotation: 0.0,
            center: (0.0, 0.0),
        });
    }
}

fn path_segments(path: &[PathCmd], rect: Rect) -> Vec<Seg> {
    if path.is_empty() {
        return vec![
            Seg::Move(rect.x, rect.y),
            Seg::Line(rect.x + rect.width, rect.y),
            Seg::Line(rect.x + rect.width, rect.y + rect.height),
            Seg::Line(rect.x, rect.y + rect.height),
            Seg::Close,
        ];
    }
    path.iter()
        .map(|cmd| match *cmd {
            PathCmd::Move(x, y) => Seg::Move(rect.x + pt(x), rect.y + pt(y)),
            PathCmd::Line(x, y) => Seg::Line(rect.x + pt(x), rect.y + pt(y)),
            PathCmd::Quad(a, b, c, d) => Seg::Quad(
                rect.x + pt(a),
                rect.y + pt(b),
                rect.x + pt(c),
                rect.y + pt(d),
            ),
            PathCmd::Cubic(a, b, c, d, e, f) => Seg::Cubic(
                rect.x + pt(a),
                rect.y + pt(b),
                rect.x + pt(c),
                rect.y + pt(d),
                rect.x + pt(e),
                rect.y + pt(f),
            ),
            PathCmd::Close => Seg::Close,
        })
        .collect()
}

fn shift(segment: Seg, dx: f32, dy: f32) -> Seg {
    match segment {
        Seg::Move(x, y) => Seg::Move(x + dx, y + dy),
        Seg::Line(x, y) => Seg::Line(x + dx, y + dy),
        Seg::Quad(a, b, x, y) => Seg::Quad(a + dx, b + dy, x + dx, y + dy),
        Seg::Cubic(a, b, c, d, x, y) => Seg::Cubic(a + dx, b + dy, c + dx, d + dy, x + dx, y + dy),
        Seg::Close => Seg::Close,
    }
}
