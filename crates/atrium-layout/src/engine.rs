//! Lays a Word document or a slide out on pages. Paragraph work lives in
//! [`lines`], tables in [`table`]; this module walks the document, fills
//! pages, numbers them and draws headers, footers, footnotes and floats.

mod lines;
mod table;

use std::collections::HashMap;

use atrium_office::look::{Chart, ChartKind, Fill, NumFormat, PathCmd, Rgb, Section, ShapeKind};
use atrium_office::{Marks, Shape, Slide, WordDoc};

use crate::{FontKey, Item, Layout, Line, Metrics, Page, Pen, Rect, Seg, Shaper};

pub(crate) use lines::Laid;

pub(crate) fn pt(twips: i32) -> f32 {
    twips as f32 / atrium_office::look::TWIPS_PER_PT as f32
}

/// Word's default body size for Chinese documents: 五号, 10.5 pt.
pub(crate) const BODY_PT: f32 = 10.5;
pub(crate) const SLIDE_PT: f32 = 18.0;
/// Lists the engine makes up for list paragraphs that have no numbering
/// look: they restart after any other paragraph.
pub(crate) const HINTED_LIST: u32 = u32::MAX - 1;

type ShapeKey = (String, String, u32, bool, bool, bool);
type MetricsKey = (String, u32, bool, bool, bool);

pub(crate) struct Ctx<'a> {
    pub shaper: &'a dyn Shaper,
    pub defaults: &'a Marks,
    pub base_size: f32,
    pub counters: HashMap<u32, Vec<u32>>,
    pub page_number: u32,
    pub pages_total: u32,
    pub default_tab: f32,
    pub footnote_number: u32,
    pub line_pitch: Option<f32>,
    pub shapes: HashMap<ShapeKey, Vec<crate::Cluster>>,
    pub metrics_cache: HashMap<MetricsKey, Metrics>,
}

impl<'a> Ctx<'a> {
    fn new(shaper: &'a dyn Shaper, defaults: &'a Marks, base_size: f32, tab: i32) -> Self {
        Self {
            shaper,
            defaults,
            base_size,
            counters: HashMap::new(),
            page_number: 1,
            pages_total: 1,
            default_tab: pt(tab),
            footnote_number: 0,
            line_pitch: None,
            shapes: HashMap::new(),
            metrics_cache: HashMap::new(),
        }
    }
}

pub(crate) fn translate(item: &Item, dx: f32, dy: f32) -> Item {
    match item.clone() {
        Item::Text {
            x,
            baseline,
            text,
            font,
            color,
        } => Item::Text {
            x: x + dx,
            baseline: baseline + dy,
            text,
            font,
            color,
        },
        Item::Fill { rect, color } => Item::Fill {
            rect: Rect {
                x: rect.x + dx,
                y: rect.y + dy,
                ..rect
            },
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
            rect: Rect {
                x: rect.x + dx,
                y: rect.y + dy,
                ..rect
            },
            path,
        },
        Item::Path {
            segments,
            fill,
            stroke,
        } => Item::Path {
            segments: segments
                .into_iter()
                .map(|s| match s {
                    Seg::Move(x, y) => Seg::Move(x + dx, y + dy),
                    Seg::Line(x, y) => Seg::Line(x + dx, y + dy),
                    Seg::Quad(a, b, x, y) => Seg::Quad(a + dx, b + dy, x + dx, y + dy),
                    Seg::Cubic(a, b, c, d, x, y) => {
                        Seg::Cubic(a + dx, b + dy, c + dx, d + dy, x + dx, y + dy)
                    }
                    Seg::Close => Seg::Close,
                })
                .collect(),
            fill,
            stroke,
        },
        Item::Gradient { rect, angle, stops } => Item::Gradient {
            rect: Rect {
                x: rect.x + dx,
                y: rect.y + dy,
                ..rect
            },
            angle,
            stops,
        },
    }
}

pub(crate) fn number(n: u32, format: &NumFormat) -> String {
    match format {
        NumFormat::Decimal => n.to_string(),
        NumFormat::DecimalZero => format!("{n:02}"),
        NumFormat::LowerLetter | NumFormat::UpperLetter => {
            let c = (b'a' + ((n.max(1) - 1) % 26) as u8) as char;
            let s = c.to_string().repeat(((n.max(1) - 1) / 26 + 1) as usize);
            if matches!(format, NumFormat::UpperLetter) {
                s.to_uppercase()
            } else {
                s
            }
        }
        NumFormat::LowerRoman => roman(n),
        NumFormat::UpperRoman => roman(n).to_uppercase(),
        NumFormat::Chinese => chinese(n, false),
        NumFormat::ChineseLegal => chinese(n, true),
        NumFormat::Circled => char::from_u32(0x2460 + n.saturating_sub(1))
            .filter(|_| (1..=20).contains(&n))
            .map(String::from)
            .unwrap_or_else(|| n.to_string()),
        NumFormat::Bullet(mark) => mark.clone(),
        NumFormat::None => String::new(),
    }
}

fn roman(mut n: u32) -> String {
    let table = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (v, s) in table {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

fn chinese(n: u32, legal: bool) -> String {
    let digits: Vec<char> = if legal {
        "零壹贰叁肆伍陆柒捌玖".chars().collect()
    } else {
        "〇一二三四五六七八九".chars().collect()
    };
    let ten = if legal { '拾' } else { '十' };
    match n {
        0..=9 => digits[n as usize].to_string(),
        10..=19 => format!(
            "{ten}{}",
            if n == 10 {
                String::new()
            } else {
                digits[(n - 10) as usize].to_string()
            }
        ),
        20..=99 => format!(
            "{}{ten}{}",
            digits[(n / 10) as usize],
            if n.is_multiple_of(10) {
                String::new()
            } else {
                digits[(n % 10) as usize].to_string()
            }
        ),
        _ => n.to_string(),
    }
}

/// Where the next line goes while filling pages.
struct Cursor {
    layout: Layout,
    section: usize,
    page: Option<usize>,
    y: f32,
    top: f32,
    bottom: f32,
    left: f32,
    /// Each page's section, for headers and footers.
    sections: Vec<usize>,
}

impl Cursor {
    fn new_page(&mut self, sections: &[Section]) {
        let s = &sections[self.section].page;
        self.layout.pages.push(Page {
            width: pt(s.width),
            height: pt(s.height),
            number: 0,
            background: None,
            items: Vec::new(),
        });
        self.sections.push(self.section);
        self.page = Some(self.layout.pages.len() - 1);
        self.top = pt(s.top);
        self.bottom = pt(s.height - s.bottom);
        self.left = pt(s.left + s.gutter);
        self.y = self.top;
    }

    fn at_top(&self) -> bool {
        self.y <= self.top + 0.01
    }

    fn place(&mut self, line: Laid, sections: &[Section]) {
        if self.y + line.height > self.bottom && !self.at_top() {
            self.new_page(sections);
        }
        let page = self.page.unwrap_or(0);
        let (dx, dy) = (self.left, self.y);
        let items = line.items.iter().map(|i| translate(i, dx, dy));
        self.layout.pages[page].items.extend(items);
        self.layout.lines.push(Line {
            page,
            para: line.para,
            range: line.range,
            top: dy,
            height: line.height,
            baseline: dy + line.baseline,
            stops: line.stops.iter().map(|(o, x)| (*o, x + dx)).collect(),
            furniture: false,
        });
        self.y += line.height;
    }
}

pub(crate) fn document(doc: &WordDoc, shaper: &dyn Shaper) -> Layout {
    let sections = doc.sections().into_owned();
    let mut cx = Ctx::new(shaper, doc.defaults(), BODY_PT, doc.default_tab_stop());
    let mut cur = Cursor {
        layout: Layout::default(),
        section: 0,
        page: None,
        y: 0.0,
        top: 0.0,
        bottom: 0.0,
        left: 0.0,
        sections: Vec::new(),
    };
    cur.new_page(&sections);
    for block in doc.blocks() {
        let s = &sections[cur.section].page;
        let width = pt(s.width - s.left - s.right - s.gutter);
        cx.line_pitch = sections[cur.section].page.line_pitch.map(pt);
        match block {
            atrium_office::Block::Paragraph(p) => {
                let b = lines::para(&mut cx, p, width, false);
                if b.page_break_before && !cur.at_top() {
                    cur.new_page(&sections);
                }
                if !cur.at_top() {
                    cur.y += b.before;
                }
                let start = (cur.page, cur.y);
                let mut height = 0.0;
                for line in b.lines {
                    height += line.height;
                    cur.place(line, &sections);
                }
                if let (Some(color), (Some(page), y)) = (b.shading, start)
                    && cur.page == Some(page)
                {
                    let fill = Item::Fill {
                        rect: Rect {
                            x: cur.left,
                            y,
                            width,
                            height,
                        },
                        color,
                    };
                    cur.layout.pages[page].items.insert(0, fill);
                }
                cur.y += b.after;
            }
            atrium_office::Block::Table(t) => {
                let (rows, items, _) = table::table(&mut cx, t, width);
                let mut row_tops: Vec<(f32, f32, f32, usize)> = Vec::new();
                for row in rows {
                    let row_y = row.y;
                    let h = row.height;
                    if cur.y + h > cur.bottom && !cur.at_top() {
                        cur.new_page(&sections);
                    }
                    let page = cur.page.unwrap_or(0);
                    let dy = cur.y - row_y;
                    row_tops.push((row_y, row_y + h, dy, page));
                    for (ly, line) in row.lines {
                        let top = cur.y + ly;
                        cur.layout.pages[page]
                            .items
                            .extend(line.items.iter().map(|i| translate(i, cur.left, top)));
                        cur.layout.lines.push(Line {
                            page,
                            para: line.para,
                            range: line.range,
                            top,
                            height: line.height,
                            baseline: top + line.baseline,
                            stops: line.stops.iter().map(|(o, x)| (*o, x + cur.left)).collect(),
                            furniture: false,
                        });
                    }
                    cur.y += h;
                }
                for item in items {
                    let y = item_y(&item);
                    let row = row_tops
                        .iter()
                        .position(|(a, b, _, _)| y >= *a - 0.01 && y < *b - 0.01)
                        .unwrap_or(row_tops.len().saturating_sub(1));
                    if let Some((_, _, dy, page)) = row_tops.get(row) {
                        let moved = translate(&item, cur.left, *dy);
                        let items = &mut cur.layout.pages[*page].items;
                        if matches!(moved, Item::Fill { .. }) {
                            items.insert(0, moved);
                        } else {
                            items.push(moved);
                        }
                    }
                }
            }
            atrium_office::Block::Opaque { .. } => {}
        }
        if let Some(last) = sections[cur.section].last_block
            && last == block.id()
            && cur.section + 1 < sections.len()
        {
            cur.section += 1;
            if sections[cur.section].new_page {
                cur.new_page(&sections);
            }
        }
    }

    // Number the pages, then draw headers and footers.
    let total = cur.layout.pages.len() as u32;
    let mut number = 0;
    let mut last_section = usize::MAX;
    for (i, page) in cur.layout.pages.iter_mut().enumerate() {
        let s = cur.sections[i];
        number = match (s != last_section, sections[s].page_start) {
            (true, Some(start)) => start,
            _ => number + 1,
        };
        last_section = s;
        page.number = number;
    }
    cx.pages_total = total;
    let mut first_in_section = vec![true; sections.len()];
    for i in 0..cur.layout.pages.len() {
        let s = cur.sections[i];
        let section = &sections[s];
        let first = std::mem::replace(&mut first_in_section[s], false);
        let (header, footer) = if first && section.title_page {
            (&section.first_header, &section.first_footer)
        } else {
            (&section.header, &section.footer)
        };
        cx.page_number = cur.layout.pages[i].number;
        let p = &section.page;
        let width = pt(p.width - p.left - p.right - p.gutter);
        let left = pt(p.left + p.gutter);
        let save = std::mem::take(&mut cx.counters);
        for (blocks, top) in [(header, true), (footer, false)] {
            let Some(blocks) = blocks else { continue };
            let (placed, extra, h) = lines::stack(&mut cx, blocks, width);
            let y0 = if top {
                pt(p.header)
            } else {
                pt(p.height - p.footer) - h
            };
            let page = &mut cur.layout.pages[i];
            page.items
                .extend(extra.iter().map(|it| translate(it, left, y0)));
            for (ly, line) in placed {
                page.items
                    .extend(line.items.iter().map(|it| translate(it, left, y0 + ly)));
                cur.layout.lines.push(Line {
                    page: i,
                    para: line.para,
                    range: line.range,
                    top: y0 + ly,
                    height: line.height,
                    baseline: y0 + ly + line.baseline,
                    stops: line.stops.iter().map(|(o, x)| (*o, x + left)).collect(),
                    furniture: true,
                });
            }
        }
        cx.counters = save;
    }
    cur.layout
}

fn item_y(item: &Item) -> f32 {
    match item {
        Item::Fill { rect, .. } | Item::Picture { rect, .. } | Item::Gradient { rect, .. } => {
            rect.y
        }
        Item::Path { segments, .. } => segments
            .iter()
            .filter_map(|s| match s {
                Seg::Move(_, y) | Seg::Line(_, y) => Some(*y),
                _ => None,
            })
            .fold(f32::MAX, f32::min),
        Item::Rule { from, to, .. } => from.1.min(to.1),
        Item::Text { baseline, .. } => *baseline,
    }
}

pub(crate) fn slide(slide: &Slide, index: usize, shaper: &dyn Shaper) -> Layout {
    let defaults = Marks::default();
    let mut cx = Ctx::new(shaper, &defaults, SLIDE_PT, 420);
    cx.page_number = index as u32 + 1;
    let mut page = Page {
        width: slide.size.0 as f32,
        height: slide.size.1 as f32,
        number: index as u32 + 1,
        background: slide.background.or(Some(Rgb::WHITE)),
        items: Vec::new(),
    };
    let mut lines = Vec::new();
    let (w, h) = (page.width, page.height);
    let mut next_y = h * 0.08;
    let mut first = true;
    for shape in &slide.shapes {
        if shape.frame[2] > 1 && shape.frame[3] > 1 {
            shape_items(&mut cx, shape, index, &mut page, &mut lines, None);
            continue;
        }
        let size = if first { 32.0 } else { SLIDE_PT };
        let frame = [w * 0.07, next_y, w * 0.86, 0.0];
        let used = shape_items(
            &mut cx,
            shape,
            index,
            &mut page,
            &mut lines,
            Some((frame, size)),
        );
        next_y += used + h * 0.04;
        first = false;
    }
    Layout {
        pages: vec![page],
        lines,
    }
}

/// Draws a shape and its text; returns the height its text took.
fn shape_items(
    cx: &mut Ctx,
    shape: &Shape,
    index: usize,
    page: &mut Page,
    lines: &mut Vec<Line>,
    auto: Option<([f32; 4], f32)>,
) -> f32 {
    let [x, y, w, h] = auto.map(|p| p.0).unwrap_or(shape.frame.map(|v| v as f32));
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    let look = &shape.look;
    shape_paint(shape, rect, page);
    match &shape.kind {
        ShapeKind::Table(table) => {
            let (rows, items, _) = table::table(cx, table, w);
            page.items.extend(items.iter().map(|i| translate(i, x, y)));
            for row in rows {
                let row_y = row.y;
                for (ly, line) in row.lines {
                    let top = y + row_y + ly;
                    page.items
                        .extend(line.items.iter().map(|i| translate(i, x, top)));
                    lines.push(Line {
                        page: index,
                        para: line.para,
                        range: line.range,
                        top,
                        height: line.height,
                        baseline: top + line.baseline,
                        stops: line.stops.iter().map(|(o, sx)| (*o, sx + x)).collect(),
                        furniture: false,
                    });
                }
            }
            return h;
        }
        ShapeKind::Chart(c) => {
            page.items.extend(chart(c, rect, cx.shaper));
            return h;
        }
        ShapeKind::Fallback(path) => {
            page.items.push(Item::Picture {
                rect,
                path: path.clone(),
            });
            return h;
        }
        ShapeKind::Text | ShapeKind::Picture => {}
    }
    let insets = if look.insets == [0; 4] {
        [3.6, 7.2, 3.6, 7.2]
    } else {
        look.insets.map(pt)
    };
    let inner = (w - insets[1] - insets[3]).max(1.0);
    let scale = look.font_scale.map(|s| s as f32 / 1000.0).unwrap_or(1.0);
    cx.base_size = auto.map(|p| p.1).unwrap_or(SLIDE_PT) * scale;
    cx.line_pitch = None;
    let mut placed = Vec::new();
    let mut ty = 0.0;
    for p in &shape.paras {
        let b = lines::para(cx, p, inner, auto.is_none());
        ty += b.before;
        for line in b.lines {
            let lh = line.height;
            placed.push((ty, line));
            ty += lh;
        }
        ty += b.after;
    }
    let h = if auto.is_some() {
        ty + insets[0] + insets[2]
    } else {
        h
    };
    let free = h - insets[0] - insets[2] - ty;
    let dy = y
        + insets[0]
        + match look.v_align {
            atrium_office::look::VAlign::Top => 0.0,
            atrium_office::look::VAlign::Center => free / 2.0,
            atrium_office::look::VAlign::Bottom => free,
        };
    let dx = x + insets[3];
    for (ly, line) in placed {
        page.items
            .extend(line.items.iter().map(|i| translate(i, dx, dy + ly)));
        lines.push(Line {
            page: index,
            para: line.para,
            range: line.range,
            top: dy + ly,
            height: line.height,
            baseline: dy + ly + line.baseline,
            stops: line.stops.iter().map(|(o, sx)| (*o, sx + dx)).collect(),
            furniture: false,
        });
    }
    h
}

/// A shape's fill and outline: its own path, or its frame.
fn shape_paint(shape: &Shape, rect: Rect, page: &mut Page) {
    let look = &shape.look;
    let (x, y) = (rect.x, rect.y);
    let segments: Vec<Seg> = if look.path.is_empty() {
        vec![
            Seg::Move(x, y),
            Seg::Line(x + rect.width, y),
            Seg::Line(x + rect.width, y + rect.height),
            Seg::Line(x, y + rect.height),
            Seg::Close,
        ]
    } else {
        look.path
            .iter()
            .map(|c| match *c {
                PathCmd::Move(a, b) => Seg::Move(x + pt(a), y + pt(b)),
                PathCmd::Line(a, b) => Seg::Line(x + pt(a), y + pt(b)),
                PathCmd::Quad(a, b, c, d) => Seg::Quad(x + pt(a), y + pt(b), x + pt(c), y + pt(d)),
                PathCmd::Cubic(a, b, c, d, e, f) => Seg::Cubic(
                    x + pt(a),
                    y + pt(b),
                    x + pt(c),
                    y + pt(d),
                    x + pt(e),
                    y + pt(f),
                ),
                PathCmd::Close => Seg::Close,
            })
            .collect()
    };
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
        });
    }
}

/// The reference chart: a frame, the title, and columns, bars or lines for
/// the series; other kinds show their name. The full engine replaces it.
pub(crate) fn chart(c: &Chart, rect: Rect, shaper: &dyn Shaper) -> Vec<Item> {
    const PALETTE: [u32; 6] = [0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47];
    let mut out = vec![Item::Rule {
        from: (rect.x, rect.y + rect.height),
        to: (rect.x + rect.width, rect.y + rect.height),
        width: 0.75,
        color: Rgb(0x808080),
    }];
    let font = FontKey {
        family: String::new(),
        size: 12.0,
        bold: false,
        italic: false,
        cjk: true,
    };
    let mut top = rect.y + 4.0;
    if let Some(title) = &c.title {
        let width: f32 = shaper.shape(title, &font).iter().map(|c| c.advance).sum();
        out.push(Item::Text {
            x: rect.x + (rect.width - width) / 2.0,
            baseline: top + 12.0,
            text: title.clone(),
            font: font.clone(),
            color: Rgb(0x404040),
        });
        top += 20.0;
    }
    let plot = Rect {
        x: rect.x + 8.0,
        y: top,
        width: rect.width - 16.0,
        height: (rect.y + rect.height - top - 4.0).max(1.0),
    };
    let max = c
        .series
        .iter()
        .flat_map(|s| s.values.iter().flatten())
        .fold(0.0f64, |m, v| m.max(*v))
        .max(f64::EPSILON);
    let count = c
        .series
        .iter()
        .map(|s| s.values.len())
        .max()
        .unwrap_or(0)
        .max(1);
    let color = |i: usize, s: &atrium_office::look::Series| {
        s.color.unwrap_or(Rgb(PALETTE[i % PALETTE.len()]))
    };
    match c.kind {
        ChartKind::Column | ChartKind::Bar => {
            let groups = c.series.len().max(1);
            let slot = plot.width / count as f32;
            let bar = slot * 0.7 / groups as f32;
            for (si, s) in c.series.iter().enumerate() {
                for (k, v) in s.values.iter().enumerate() {
                    let Some(v) = v else { continue };
                    let h = (*v / max) as f32 * plot.height;
                    out.push(Item::Fill {
                        rect: Rect {
                            x: plot.x + slot * k as f32 + slot * 0.15 + bar * si as f32,
                            y: plot.y + plot.height - h,
                            width: bar,
                            height: h,
                        },
                        color: color(si, s),
                    });
                }
            }
        }
        ChartKind::Line | ChartKind::Area => {
            let step = plot.width / count as f32;
            for (si, s) in c.series.iter().enumerate() {
                let mut segments = Vec::new();
                for (k, v) in s.values.iter().enumerate() {
                    let Some(v) = v else { continue };
                    let px = plot.x + step * (k as f32 + 0.5);
                    let py = plot.y + plot.height - (*v / max) as f32 * plot.height;
                    segments.push(if segments.is_empty() {
                        Seg::Move(px, py)
                    } else {
                        Seg::Line(px, py)
                    });
                }
                out.push(Item::Path {
                    segments,
                    fill: None,
                    stroke: Some(Pen {
                        width: 2.0,
                        color: color(si, s),
                        alpha: 255,
                        dash: Default::default(),
                    }),
                });
            }
        }
        _ => {
            let label = match &c.kind {
                ChartKind::Other(name) => name.clone(),
                other => format!("{other:?}"),
            };
            out.push(Item::Text {
                x: plot.x,
                baseline: plot.y + 14.0,
                text: label,
                font,
                color: Rgb(0x808080),
            });
        }
    }
    out
}
