//! Lays a Word document or a slide out on pages. Paragraph work lives in
//! [`lines`], tables in [`table`]; this module walks the document, fills
//! pages, numbers them and draws headers, footers, footnotes and floats.

mod lines;
mod table;

use std::collections::HashMap;
use std::collections::HashSet;

use atrium_office::look::{
    Chart, ChartKind, Fill, NumFormat, Origin, PathCmd, Rgb, Section, ShapeKind, Wrap,
};
use atrium_office::{AtomKind, Block, Inline, Marks, NodeId, Shape, Slide, WordDoc};

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

struct Furniture {
    page: usize,
    items: Vec<Item>,
    lines: Vec<Line>,
}

struct Note {
    lines: Vec<(f32, Laid)>,
    items: Vec<Item>,
    height: f32,
}

struct Flow<'a, 'b> {
    cx: &'a mut Ctx<'b>,
    sections: Vec<Section>,
    layout: Layout,
    furniture: Vec<Furniture>,
    section: usize,
    page: usize,
    column: usize,
    y: f32,
    body_top: f32,
    body_bottom: f32,
    area_left: f32,
    area_width: f32,
    col_width: f32,
    columns: usize,
    header_h: f32,
    footer_h: f32,
    number: u32,
    last_section: usize,
    first_in_section: Vec<bool>,
    last_after: f32,
    last_after_at: Option<(usize, usize)>,
    last_style: Option<atrium_office::ParaStyle>,
    floats: Vec<lines::FloatBox>,
    front: Vec<(usize, Item)>,
    notes: Vec<Vec<Note>>,
    base_bottom: Vec<f32>,
    note_height: f32,
    footnotes: HashMap<NodeId, Vec<Block>>,
    previous: Option<&'a Layout>,
    // Incremental relayout (15e) records these for reusing pages; not read yet.
    #[allow(dead_code)]
    changed: HashSet<NodeId>,
    #[allow(dead_code)]
    start_page: usize,
    start_block: usize,
    block_index: usize,
    number_override: Option<u32>,
    stopped: bool,
}

impl Flow<'_, '_> {
    fn column_left(&self, column: usize) -> f32 {
        let gap = pt(self.sections[self.section].page.column_gap);
        self.area_left + column as f32 * (self.col_width + gap)
    }

    fn at_top(&self) -> bool {
        self.y <= self.body_top + 0.01
    }

    fn at_page_top(&self) -> bool {
        self.column == 0 && self.at_top()
    }

    fn geometry(&mut self) {
        let p = &self.sections[self.section].page;
        self.area_left = pt(p.left + p.gutter);
        self.area_width = pt(p.width - p.left - p.right - p.gutter);
        self.columns = p.columns.max(1) as usize;
        let gap = pt(p.column_gap);
        self.col_width =
            ((self.area_width - gap * (self.columns - 1) as f32) / self.columns as f32).max(1.0);
    }

    fn furniture(
        &mut self,
        blocks: &[atrium_office::Block],
        page: usize,
        left: f32,
        top: f32,
    ) -> f32 {
        let (placed, extra, height) = lines::stack(self.cx, blocks, self.area_width);
        let mut items: Vec<Item> = extra.iter().map(|i| translate(i, left, top)).collect();
        let mut lines = Vec::new();
        for (ly, line) in placed {
            items.extend(line.items.iter().map(|i| translate(i, left, top + ly)));
            lines.push(Line {
                page,
                para: line.para,
                range: line.range,
                top: top + ly,
                height: line.height,
                baseline: top + ly + line.baseline,
                stops: line.stops.iter().map(|(o, x)| (*o, x + left)).collect(),
                furniture: true,
            });
        }
        self.furniture.push(Furniture { page, items, lines });
        height
    }

    fn start_page(&mut self) {
        let s = self.section;
        let section = self.sections[s].clone();
        let p = section.page.clone();
        self.number = if let Some(number) = self.number_override.take() {
            number
        } else if s != self.last_section {
            section.page_start.unwrap_or(self.number + 1)
        } else {
            self.number + 1
        };
        self.last_section = s;
        let first = if s < self.first_in_section.len() {
            std::mem::replace(&mut self.first_in_section[s], false)
        } else {
            false
        };
        let (header, footer) = if first && section.title_page {
            (
                section.first_header.as_deref(),
                section.first_footer.as_deref(),
            )
        } else {
            (section.header.as_deref(), section.footer.as_deref())
        };
        let index = self.layout.pages.len();
        self.layout.pages.push(Page {
            width: pt(p.width),
            height: pt(p.height),
            number: self.number,
            section: s,
            background: None,
            items: Vec::new(),
        });
        self.page = index;
        self.column = 0;
        self.floats.clear();
        self.note_height = 0.0;
        self.last_after = 0.0;
        self.last_after_at = None;
        self.geometry();
        self.cx.page_number = self.number;
        let save = std::mem::take(&mut self.cx.counters);
        let header_top = pt(p.header);
        self.header_h = header
            .map(|blocks| self.furniture(blocks, index, self.area_left, header_top))
            .unwrap_or(0.0);
        let footer_bottom = pt(p.height - p.footer);
        self.footer_h = footer
            .map(|blocks| self.furniture(blocks, index, self.area_left, footer_bottom))
            .unwrap_or(0.0);
        // The footer is drawn upwards from its distance to the page's bottom.
        if self.footer_h > 0.0
            && let Some(last) = self.furniture.last_mut()
            && last.page == index
        {
            for item in &mut last.items {
                *item = translate(item, 0.0, -self.footer_h);
            }
            for line in &mut last.lines {
                line.top -= self.footer_h;
                line.baseline -= self.footer_h;
            }
        }
        self.cx.counters = save;
        self.body_top = if header.is_some() {
            pt(p.top).max(header_top + self.header_h)
        } else {
            pt(p.top)
        };
        self.body_bottom = if footer.is_some() {
            pt(p.height - p.bottom).min(footer_bottom - self.footer_h)
        } else {
            pt(p.height - p.bottom)
        };
        if self.body_bottom <= self.body_top {
            self.body_bottom = pt(p.height - p.bottom);
        }
        if self.notes.len() <= index {
            self.notes.resize_with(index + 1, Vec::new);
        }
        if self.base_bottom.len() <= index {
            self.base_bottom.resize(index + 1, self.body_bottom);
        }
        self.base_bottom[index] = self.body_bottom;
        self.y = self.body_top;
    }

    fn next_column(&mut self) {
        if self.column + 1 < self.columns {
            self.column += 1;
            self.y = self.body_top;
            self.last_after = 0.0;
            self.last_after_at = None;
        } else {
            self.start_page();
        }
    }

    fn add_line(&mut self, page: usize, x: f32, y: f32, line: Laid, shading: Option<Rgb>) {
        if let Some(color) = shading {
            self.layout.pages[page].items.insert(
                0,
                Item::Fill {
                    rect: Rect {
                        x,
                        y,
                        width: self.col_width,
                        height: line.height,
                    },
                    color,
                },
            );
        }
        self.layout.pages[page]
            .items
            .extend(line.items.iter().map(|i| translate(i, x, y)));
        self.layout.lines.push(Line {
            page,
            para: line.para,
            range: line.range,
            top: y,
            height: line.height,
            baseline: y + line.baseline,
            stops: line.stops.iter().map(|(o, sx)| (*o, sx + x)).collect(),
            furniture: false,
        });
    }

    fn place_line(&mut self, line: Laid, shading: Option<Rgb>) {
        let (notes, note_height) = self.lay_notes(&line.refs, self.area_width);
        if !notes.is_empty()
            && self.y + line.height > self.body_bottom - note_height + 0.01
            && self.y > self.body_top + 0.01
        {
            self.next_column();
        }
        let had_notes = !notes.is_empty();
        self.commit_notes(notes, note_height);
        if !had_notes
            && self.y + line.height > self.body_bottom + 0.01
            && self.y > self.body_top + 0.01
        {
            self.next_column();
        }
        let page = self.page;
        let x = self.column_left(self.column);
        let y = self.y;
        let height = line.height;
        self.add_line(page, x, y, line, shading);
        self.y += height;
    }

    /// How many of the lines fit in the remaining space.
    fn fit_count(&self, lines: &[Laid], before: f32) -> usize {
        let mut fit = 0;
        for line in lines {
            if self.y + before + line.y + line.height > self.body_bottom + 0.01 {
                break;
            }
            fit += 1;
        }
        fit
    }

    fn first_line_height(&mut self, p: &atrium_office::Para, width: f32) -> f32 {
        let counters = self.cx.counters.clone();
        let footnotes = self.cx.footnote_number;
        let out = lines::para(self.cx, p, width, false);
        self.cx.counters = counters;
        self.cx.footnote_number = footnotes;
        out.lines.first().map(|l| l.height).unwrap_or(0.0)
    }

    fn paragraph_floats(&self, p: &atrium_office::Para) -> Vec<lines::FloatAnchor> {
        let mut out = Vec::new();
        for inline in &p.inlines {
            if let Inline::Atom(atom) = inline
                && let (AtomKind::Image(path), Some(float)) = (&atom.kind, atom.look.float)
            {
                let (w, h) = atom
                    .look
                    .size
                    .map(|(w, h)| (pt(w), pt(h)))
                    .unwrap_or((30.0, 30.0));
                out.push(lines::FloatAnchor {
                    id: atom.id,
                    path: path.clone(),
                    width: w,
                    height: h,
                    float,
                });
            }
        }
        out
    }

    fn anchor_box(&self, anchor: &lines::FloatAnchor, para_top: f32) -> lines::FloatBox {
        let p = &self.sections[self.section].page;
        let x0 = match anchor.float.from_x {
            Origin::Page => 0.0,
            Origin::Margin => self.area_left,
            Origin::Column | Origin::Paragraph => self.column_left(self.column),
        };
        let y0 = match anchor.float.from_y {
            Origin::Page => 0.0,
            Origin::Margin => pt(p.top),
            Origin::Column | Origin::Paragraph => para_top,
        };
        lines::FloatBox {
            rect: Rect {
                x: x0 + pt(anchor.float.x),
                y: y0 + pt(anchor.float.y),
                width: anchor.width,
                height: anchor.height,
            },
            wrap: anchor.float.wrap,
            clear: anchor.float.clear.map(pt),
        }
    }

    fn add_float(&mut self, anchor: &lines::FloatAnchor, placed: lines::FloatBox) {
        self.floats.push(placed);
        let item = Item::Picture {
            rect: placed.rect,
            path: anchor.path.clone(),
        };
        match placed.wrap {
            Wrap::Behind => self.layout.pages[self.page].items.insert(0, item),
            Wrap::InFront => self.front.push((self.page, item)),
            Wrap::Square | Wrap::TopAndBottom => self.layout.pages[self.page].items.push(item),
        }
    }

    fn lay_notes(&mut self, refs: &[NodeId], width: f32) -> (Vec<Note>, f32) {
        let mut out = Vec::new();
        let mut total = 0.0;
        let counters = self.cx.counters.clone();
        let footnotes = self.cx.footnote_number;
        for id in refs {
            let Some(blocks) = self.footnotes.get(id).cloned() else {
                continue;
            };
            let (placed, items, height) = lines::stack(self.cx, &blocks, width);
            total += height + 2.0;
            out.push(Note {
                lines: placed,
                items,
                height,
            });
        }
        self.cx.counters = counters;
        self.cx.footnote_number = footnotes;
        (out, total)
    }

    fn commit_notes(&mut self, notes: Vec<Note>, height: f32) {
        if notes.is_empty() {
            return;
        }
        self.notes[self.page].extend(notes);
        self.note_height += height;
        if let Some(base) = self.base_bottom.get(self.page) {
            self.body_bottom = (base - self.note_height).max(self.body_top + 1.0);
        }
    }

    fn paragraph(&mut self, p: &atrium_office::Para, next: Option<&atrium_office::Block>) {
        if !self.stopped
            && self.block_index > self.start_block
            && self.at_page_top()
            && let Some(previous) = self.previous
            && let Some(page) = matching_page(previous, p.id, self.section, &self.sections)
        {
            self.copy_rest(previous, page);
            return;
        }
        let width = self.col_width;
        let (look, _, _) = lines::hinted(p);
        if look.page_break_before && !self.at_page_top() {
            self.start_page();
        }
        let para_top = self.y;
        let anchors = self.paragraph_floats(p);
        let mut boxes = self.floats.clone();
        let placed: Vec<lines::FloatBox> = anchors
            .iter()
            .map(|a| self.anchor_box(a, para_top))
            .collect();
        boxes.extend(placed.iter().copied());
        let wrap = lines::WrapCtx {
            top: para_top,
            left: self.column_left(self.column),
            width: self.col_width,
            floats: &boxes,
        };
        let b = lines::para_wrapped(self.cx, p, width, false, Some(&wrap));
        for (anchor, placed) in anchors.iter().zip(placed.iter()) {
            self.add_float(anchor, *placed);
        }
        if b.page_break_before && !self.at_page_top() {
            self.start_page();
        }
        let total: f32 = b.lines.last().map(|l| l.y + l.height).unwrap_or(0.0);
        let body_height = self.body_bottom - self.body_top;
        let next_first = if b.keep_next {
            match next {
                Some(atrium_office::Block::Paragraph(n)) => self.first_line_height(n, width),
                _ => 0.0,
            }
        } else {
            0.0
        };
        let mut lines = b.lines;
        loop {
            let contextual = b.contextual && self.last_style.as_ref() == Some(&p.style);
            if contextual && self.last_after_at == Some((self.page, self.column)) {
                self.y = (self.y - self.last_after).max(self.body_top);
                self.last_after = 0.0;
                self.last_after_at = None;
            }
            let mut before = b.before;
            if self.at_top() && !(self.page == 0 && self.column == 0) {
                before = 0.0;
            }
            if contextual {
                before = 0.0;
            }
            let mut fit = self.fit_count(&lines, before);
            if fit < lines.len() {
                if b.keep_lines && total <= body_height && !self.at_top() {
                    self.next_column();
                    continue;
                }
                if b.widow_control {
                    if fit == 1 {
                        fit = 0;
                    }
                    if lines.len() - fit < 2 {
                        fit = lines.len().saturating_sub(2);
                    }
                    if fit < 2 {
                        fit = 0;
                    }
                }
                if fit == 0 {
                    if self.at_top() {
                        fit = 1;
                    } else {
                        self.next_column();
                        continue;
                    }
                }
            }
            if b.keep_next
                && fit == lines.len()
                && next_first > 0.0
                && self.y + before + total + next_first > self.body_bottom + 0.01
                && total <= body_height
                && !self.at_top()
            {
                self.next_column();
                continue;
            }
            self.y += before;
            let para_start = self.y;
            let take = fit.min(lines.len());
            for line in lines.drain(..take) {
                let target = para_start + line.y;
                if target > self.y {
                    self.y = target;
                }
                self.place_line(line, b.shading);
            }
            if lines.is_empty() {
                break;
            }
            self.next_column();
        }
        self.last_after = b.after;
        self.last_after_at = Some((self.page, self.column));
        self.y += b.after;
        self.last_style = Some(p.style.clone());
    }

    fn table(&mut self, t: &atrium_office::Table) {
        let width = self.col_width;
        let (rows, _, _) = table::table(self.cx, t, width);
        let header_count = t.look.header_rows.min(rows.len());
        let headers: Vec<table::Row> = rows[..header_count].to_vec();
        let mut index = 0;
        let mut continuation: Option<table::Row> = None;
        while let Some(row) = continuation.take().or_else(|| {
            let row = rows.get(index).cloned();
            index += usize::from(row.is_some());
            row
        }) {
            let mut force = false;
            loop {
                let available = self.body_bottom - self.y;
                if force || row.height <= available + 0.01 {
                    self.place_row(&row);
                    break;
                }
                if !row.cant_split {
                    let cut = row.cut(available);
                    if cut > 0.01 && cut < row.height - 0.01 {
                        let (first, then) = row.split_at(cut);
                        self.place_row(&first);
                        let before = self.page;
                        self.next_column();
                        if self.page != before && !row.header {
                            self.repeat_headers(&headers);
                        }
                        continuation = Some(then);
                        break;
                    }
                }
                if self.at_top() {
                    self.place_row(&row);
                    break;
                }
                let before = self.page;
                self.next_column();
                if self.page == before {
                    self.place_row(&row);
                    break;
                }
                if !row.header {
                    self.repeat_headers(&headers);
                    let avail = self.body_bottom - self.y;
                    force = avail < row.height && (row.cant_split || row.cut(avail) <= 0.01);
                }
            }
        }
        self.last_style = None;
        self.last_after = 0.0;
        self.last_after_at = None;
    }

    fn place_row(&mut self, row: &table::Row) {
        let page = self.page;
        let x = self.column_left(self.column);
        let y = self.y;
        for (ly, line) in &row.lines {
            self.add_line(page, x, y + ly, line.clone(), None);
        }
        for item in &row.items {
            let moved = translate(item, x, y);
            let page_items = &mut self.layout.pages[page].items;
            if matches!(moved, Item::Fill { .. }) {
                page_items.insert(0, moved);
            } else {
                page_items.push(moved);
            }
        }
        self.y += row.height;
    }

    fn repeat_headers(&mut self, headers: &[table::Row]) {
        for header in headers {
            if self.y + header.height > self.body_bottom + 0.01 && !self.at_top() {
                self.next_column();
            }
            self.place_row(header);
        }
    }

    fn draw_notes(&mut self) {
        for i in 0..self.notes.len() {
            let notes = std::mem::take(&mut self.notes[i]);
            if notes.is_empty() {
                continue;
            }
            let Some(page) = self.layout.pages.get(i) else {
                continue;
            };
            let p = &self.sections[page.section].page;
            let left = pt(p.left + p.gutter);
            let width = pt(p.width - p.left - p.right - p.gutter);
            let total: f32 = notes.iter().map(|n| n.height + 2.0).sum();
            let mut y = (self.base_bottom.get(i).copied().unwrap_or(0.0) - total).max(pt(p.top));
            self.layout.pages[i].items.push(Item::Rule {
                from: (left, y - 3.0),
                to: (left + width / 3.0, y - 3.0),
                width: 0.5,
                color: Rgb::BLACK,
            });
            for note in notes {
                self.layout.pages[i]
                    .items
                    .extend(note.items.iter().map(|item| translate(item, left, y)));
                for (ly, line) in note.lines {
                    self.layout.pages[i]
                        .items
                        .extend(line.items.iter().map(|item| translate(item, left, y + ly)));
                    self.layout.lines.push(Line {
                        page: i,
                        para: line.para,
                        range: line.range,
                        top: y + ly,
                        height: line.height,
                        baseline: y + ly + line.baseline,
                        stops: line.stops.iter().map(|(o, x)| (*o, x + left)).collect(),
                        furniture: false,
                    });
                }
                y += note.height + 2.0;
            }
        }
    }

    fn flush_front(&mut self) {
        for (page, item) in std::mem::take(&mut self.front) {
            if let Some(p) = self.layout.pages.get_mut(page) {
                p.items.push(item);
            }
        }
    }

    fn copy_rest(&mut self, previous: &Layout, from: usize) {
        let current = self.page;
        self.layout.pages.truncate(current);
        self.layout.lines.retain(|line| line.page < current);
        self.furniture.retain(|f| f.page < current);
        self.notes.truncate(current);
        self.base_bottom.truncate(current);
        self.layout
            .pages
            .extend(previous.pages[from..].iter().cloned());
        for line in &previous.lines {
            if line.page < from {
                continue;
            }
            let mut line = line.clone();
            line.page = current + (line.page - from);
            self.layout.lines.push(line);
        }
        self.stopped = true;
    }
}

fn matching_page(
    previous: &Layout,
    para: NodeId,
    section: usize,
    sections: &[Section],
) -> Option<usize> {
    for (i, page) in previous.pages.iter().enumerate() {
        if page.section != section {
            continue;
        }
        if let Some(first) = previous
            .lines
            .iter()
            .find(|line| line.page == i && !line.furniture)
            && first.para == para
            && first.range.start == 0
        {
            let _ = sections;
            return Some(i);
        }
    }
    None
}

fn document_pass(doc: &WordDoc, shaper: &dyn Shaper, pages_total: u32) -> Layout {
    let sections = doc.sections().into_owned();
    let footnotes: HashMap<NodeId, Vec<Block>> = doc
        .footnotes()
        .iter()
        .map(|f| (f.id, f.blocks.clone()))
        .collect();
    let mut cx = Ctx::new(shaper, doc.defaults(), BODY_PT, doc.default_tab_stop());
    cx.pages_total = pages_total;
    let mut flow = Flow {
        cx: &mut cx,
        first_in_section: vec![true; sections.len()],
        sections,
        layout: Layout::default(),
        furniture: Vec::new(),
        section: 0,
        page: 0,
        column: 0,
        y: 0.0,
        body_top: 0.0,
        body_bottom: 0.0,
        area_left: 0.0,
        area_width: 0.0,
        col_width: 0.0,
        columns: 1,
        header_h: 0.0,
        footer_h: 0.0,
        number: 0,
        last_section: usize::MAX,
        last_after: 0.0,
        last_after_at: None,
        last_style: None,
        floats: Vec::new(),
        front: Vec::new(),
        notes: Vec::new(),
        base_bottom: Vec::new(),
        note_height: 0.0,
        footnotes,
        previous: None,
        changed: HashSet::new(),
        start_page: 0,
        start_block: 0,
        block_index: 0,
        number_override: None,
        stopped: false,
    };
    flow.start_page();
    let blocks = doc.blocks();
    for (i, block) in blocks.iter().enumerate() {
        flow.cx.line_pitch = flow.sections[flow.section].page.line_pitch.map(pt);
        match block {
            atrium_office::Block::Paragraph(p) => {
                flow.paragraph(p, blocks.get(i + 1));
            }
            atrium_office::Block::Table(t) => flow.table(t),
            atrium_office::Block::Opaque { .. } => {}
        }
        if let Some(last) = flow.sections[flow.section].last_block
            && last == block.id()
            && flow.section + 1 < flow.sections.len()
        {
            let next = flow.section + 1;
            if flow.sections[next].new_page {
                flow.section = next;
                flow.start_page();
            } else {
                flow.section = next;
                if let Some(start) = flow.sections[next].page_start {
                    flow.number = start;
                    flow.layout.pages[flow.page].number = start;
                }
                flow.last_section = next;
                flow.geometry();
                let p = &flow.sections[next].page;
                flow.body_top = flow.body_top.max(pt(p.top));
                flow.body_bottom =
                    pt(p.height - p.bottom).min(pt(p.height - p.footer) - flow.footer_h);
                flow.y = flow.y.max(flow.body_top);
                if flow.y >= flow.body_bottom {
                    flow.next_column();
                }
            }
        }
    }
    flow.draw_notes();
    flow.flush_front();
    let mut layout = flow.layout;
    for f in flow.furniture {
        if let Some(page) = layout.pages.get_mut(f.page) {
            page.items.extend(f.items);
        }
        layout.lines.extend(f.lines);
    }
    layout
}

pub(crate) fn document(doc: &WordDoc, shaper: &dyn Shaper) -> Layout {
    let mut total = 1;
    for _ in 0..5 {
        let out = document_pass(doc, shaper, total);
        let pages = out.pages.len() as u32;
        if pages == total {
            return out;
        }
        total = pages;
    }
    document_pass(doc, shaper, total)
}

pub(crate) fn slide(slide: &Slide, index: usize, shaper: &dyn Shaper) -> Layout {
    let defaults = Marks::default();
    let mut cx = Ctx::new(shaper, &defaults, SLIDE_PT, 420);
    cx.page_number = index as u32 + 1;
    let mut page = Page {
        width: slide.size.0 as f32,
        height: slide.size.1 as f32,
        number: index as u32 + 1,
        section: 0,
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
            let (rows, _, _) = table::table(cx, table, w);
            let mut ty = y;
            for row in rows {
                page.items
                    .extend(row.items.iter().map(|i| translate(i, x, ty)));
                for (ly, line) in row.lines {
                    let top = ty + ly;
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
                ty += row.height;
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
        let para_start = ty;
        for line in b.lines {
            let lh = line.height;
            let ly = para_start + line.y;
            placed.push((ly, line));
            ty = ly + lh;
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
