//! The reference engine: plain, correct for ordinary documents, and the
//! behaviour the full engine keeps (docs/layout-engine-implementation.md).
//! It breaks lines greedily with Chinese line-start and line-end rules,
//! paginates without keep or widow rules, lays tables out row by row
//! without splitting a row, and treats floating pictures as inline.

use std::collections::HashMap;

use atrium_office::look::{
    Border, LabelSuffix, LineSpacing, NumFormat, ParaLook, Rgb, Section, TWIPS_PER_PT, TabKind,
    VAlign,
};
use atrium_office::look::{Chart, ChartKind, Fill, PathCmd, ShapeKind};
use atrium_office::{
    Align, AtomKind, Block, Inline, Marks, NodeId, Para, ParaStyle, Shape, Slide, Table, WordDoc,
};

use crate::{FontKey, Item, Layout, Line, Metrics, Page, Pen, Rect, Seg, Shaper};

fn pt(twips: i32) -> f32 {
    twips as f32 / TWIPS_PER_PT as f32
}

/// Word's default body size for Chinese documents: 五号, 10.5 pt.
const BODY_PT: f32 = 10.5;
const SLIDE_PT: f32 = 18.0;
/// Lists the engine makes up for list paragraphs that have no numbering
/// look: they restart after any other paragraph.
const HINTED_LIST: u32 = u32::MAX - 1;
/// Default tab stops: two characters of body text.
const DEFAULT_TAB: f32 = 21.0;

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2FFF | 0x3000..=0x30FF | 0x3100..=0x31FF
        | 0x3200..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F
        | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// May not start a line.
const NO_START: &str = "、。，．：；？！）〕］｝〉》」』】〙〗〟’”｠»ヽヾーァィゥェォッャュョヮヵヶぁぃぅぇぉっゃゅょゎ々〻‐゠–〜・,.:;?!)]}%";
/// May not end a line.
const NO_END: &str = "（〔［｛〈《「『【〘〖〝‘“｟«([{";

#[derive(Clone, Debug)]
enum Kind {
    Glyph,
    Space,
    Tab,
    Break,
    Picture(Option<std::path::PathBuf>, f32, f32),
    /// A list label: drawn, but no caret stops in it.
    Label,
}

#[derive(Clone, Debug)]
struct Unit {
    start: usize,
    end: usize,
    width: f32,
    kind: Kind,
    text: String,
    font: FontKey,
    color: Rgb,
    marks: Marks,
    metrics: Metrics,
    cjk: bool,
    no_start: bool,
    no_end: bool,
}

/// A line laid out at the origin: x from the text area's left edge, y
/// from the line's top.
#[derive(Clone, Debug)]
struct Laid {
    height: f32,
    baseline: f32,
    items: Vec<Item>,
    para: NodeId,
    range: std::ops::Range<usize>,
    stops: Vec<(usize, f32)>,
}

struct ParaBox {
    before: f32,
    after: f32,
    page_break_before: bool,
    lines: Vec<Laid>,
    shading: Option<Rgb>,
}

struct Ctx<'a> {
    shaper: &'a dyn Shaper,
    defaults: &'a Marks,
    base_size: f32,
    /// Each list's count per level.
    counters: HashMap<u32, Vec<u32>>,
    page_number: u32,
    pages_total: u32,
    last_table_height: f32,
}

fn translate(item: &Item, dx: f32, dy: f32) -> Item {
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

/// Sizes and spacing for a paragraph whose file said nothing about them,
/// so a document still reads as headings and body.
fn hinted(para: &Para) -> (ParaLook, Option<f32>, bool) {
    let mut look = para.look.clone();
    if look != ParaLook::default() {
        return (look, None, false);
    }
    let (size, bold) = match &para.style {
        ParaStyle::Title => (Some(22.0), true),
        ParaStyle::Heading(1) => (Some(16.0), true),
        ParaStyle::Heading(2) => (Some(14.0), true),
        ParaStyle::Heading(3) => (Some(12.0), true),
        ParaStyle::Heading(_) => (Some(BODY_PT), true),
        _ => (None, false),
    };
    if matches!(para.style, ParaStyle::Title | ParaStyle::Heading(_)) {
        look.before = 240;
        look.after = 120;
        look.keep_next = true;
    }
    if let ParaStyle::ListItem { level, ordered } = para.style {
        look.left = 420 * (level as i32 + 1);
        look.first_line = -420;
        look.list = Some(atrium_office::look::ListLook {
            list: HINTED_LIST + ordered as u32,
            level,
            pattern: format!("%{}.", level + 1),
            formats: (0..=level)
                .map(|_| {
                    if ordered {
                        NumFormat::Decimal
                    } else {
                        NumFormat::Bullet("•".into())
                    }
                })
                .collect(),
            starts: vec![1; level as usize + 1],
            marks: None,
            suffix: LabelSuffix::Tab,
        });
    }
    if para.style == ParaStyle::Quote {
        look.left = 420;
    }
    (look, size, bold)
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

fn number(n: u32, format: &NumFormat) -> String {
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

impl Ctx<'_> {
    fn font(&self, marks: &Marks, cjk: bool, size: Option<f32>, bold: bool) -> FontKey {
        let pick = |own: &str, default: &str| {
            if own.is_empty() {
                default.to_string()
            } else {
                own.to_string()
            }
        };
        let family = if cjk {
            pick(&marks.fonts.east_asia, &self.defaults.fonts.east_asia)
        } else {
            pick(&marks.fonts.latin, &self.defaults.fonts.latin)
        };
        let mut size = marks
            .size
            .or(self.defaults.size)
            .map(pt)
            .or(size)
            .unwrap_or(self.base_size);
        if marks.baseline != atrium_office::look::Baseline::Normal {
            size *= 0.65;
        }
        FontKey {
            family,
            size,
            bold: marks.bold || bold,
            italic: marks.italic,
            cjk,
        }
    }

    fn color(&self, marks: &Marks) -> Rgb {
        marks.color.or(self.defaults.color).unwrap_or(Rgb::BLACK)
    }

    fn label(&mut self, look: &ParaLook) -> Option<String> {
        let Some(list) = look.list.as_ref() else {
            self.counters.retain(|id, _| *id < HINTED_LIST);
            return None;
        };
        if list.list >= HINTED_LIST {
            let other = HINTED_LIST + (list.list == HINTED_LIST) as u32;
            self.counters.remove(&other);
        }
        let level = list.level as usize;
        let counts = self.counters.entry(list.list).or_default();
        counts.resize(level + 1, 0);
        counts[level] += 1;
        counts.truncate(level + 1);
        let mut text = list.pattern.clone();
        for (i, count) in counts.iter().enumerate() {
            let start = list.starts.get(i).copied().unwrap_or(1);
            let format = list.formats.get(i).unwrap_or(&NumFormat::Decimal);
            let n = count + start.saturating_sub(1);
            text = text.replace(&format!("%{}", i + 1), &number(n, format));
        }
        if let Some(NumFormat::Bullet(mark)) = list.formats.get(level) {
            text = mark.clone();
        }
        Some(text)
    }

    /// The paragraph cut into units: clusters, spaces, tabs, breaks, pictures.
    fn units(&mut self, para: &Para, size: Option<f32>, bold: bool) -> Vec<Unit> {
        let mut out = Vec::new();
        let mut at = 0usize;
        for inline in &para.inlines {
            match inline {
                Inline::Text { text, marks } => {
                    let mut segments: Vec<(usize, usize, bool)> = Vec::new();
                    for (i, c) in text.char_indices() {
                        let cjk = is_cjk(c);
                        match segments.last_mut() {
                            Some(last) if last.2 == cjk || c == ' ' => last.1 = i + c.len_utf8(),
                            _ => segments.push((i, i + c.len_utf8(), cjk)),
                        }
                    }
                    for (from, to, cjk) in segments {
                        let piece = &text[from..to];
                        let font = self.font(marks, cjk, size, bold);
                        let metrics = self.shaper.metrics(&font);
                        let clusters = self.shaper.shape(piece, &font);
                        for (k, cluster) in clusters.iter().enumerate() {
                            let end = clusters.get(k + 1).map(|c| c.start).unwrap_or(piece.len());
                            let s = &piece[cluster.start..end];
                            let first = s.chars().next().unwrap_or(' ');
                            out.push(Unit {
                                start: at + from + cluster.start,
                                end: at + from + end,
                                width: cluster.advance,
                                kind: if s.chars().all(char::is_whitespace) {
                                    Kind::Space
                                } else {
                                    Kind::Glyph
                                },
                                text: s.to_string(),
                                font: font.clone(),
                                color: self.color(marks),
                                marks: marks.clone(),
                                metrics,
                                cjk: is_cjk(first),
                                no_start: NO_START.contains(first),
                                no_end: s.chars().last().is_some_and(|c| NO_END.contains(c)),
                            });
                        }
                    }
                    at += text.len();
                }
                Inline::Break | Inline::Tab => {
                    let marks = Marks::default();
                    let font = self.font(&marks, false, size, bold);
                    out.push(Unit {
                        start: at,
                        end: at + 1,
                        width: 0.0,
                        kind: if matches!(inline, Inline::Break) {
                            Kind::Break
                        } else {
                            Kind::Tab
                        },
                        text: String::new(),
                        metrics: self.shaper.metrics(&font),
                        font,
                        color: Rgb::BLACK,
                        marks,
                        cjk: false,
                        no_start: false,
                        no_end: false,
                    });
                    at += 1;
                }
                Inline::Atom(atom) => {
                    let len = '\u{FFFC}'.len_utf8();
                    let marks = Marks::default();
                    let font = self.font(&marks, false, size, bold);
                    let metrics = self.shaper.metrics(&font);
                    let shown = match &atom.kind {
                        AtomKind::Field(code) if code.trim_start().starts_with("NUMPAGES") => {
                            Some(self.pages_total.max(1).to_string())
                        }
                        AtomKind::Field(code) if code.trim_start().starts_with("PAGE") => {
                            Some(self.page_number.to_string())
                        }
                        _ => atom.look.shown.clone(),
                    };
                    let (kind, width, text) = match (&atom.kind, atom.look.size, shown) {
                        (AtomKind::Image(path), size, _) => {
                            let (w, h) = size
                                .map(|(w, h)| (pt(w), pt(h)))
                                .unwrap_or((metrics.ascent * 3.0, metrics.ascent * 3.0));
                            (Kind::Picture(path.clone(), w, h), w, String::new())
                        }
                        (_, _, Some(text)) => {
                            let width = self
                                .shaper
                                .shape(&text, &font)
                                .iter()
                                .map(|c| c.advance)
                                .sum();
                            (Kind::Glyph, width, text)
                        }
                        _ => (Kind::Glyph, 0.0, String::new()),
                    };
                    out.push(Unit {
                        start: at,
                        end: at + len,
                        width,
                        kind,
                        text,
                        color: Rgb::BLACK,
                        font,
                        marks,
                        metrics,
                        cjk: false,
                        no_start: false,
                        no_end: false,
                    });
                    at += len;
                }
            }
        }
        out
    }

    fn label_unit(&self, text: String, para: &Para, size: Option<f32>, bold: bool) -> Unit {
        let marks = para
            .look
            .list
            .as_ref()
            .and_then(|l| l.marks.as_deref().cloned())
            .or_else(|| {
                para.inlines.iter().find_map(|i| match i {
                    Inline::Text { marks, .. } => Some(marks.clone()),
                    _ => None,
                })
            })
            .unwrap_or_default();
        let cjk = text.chars().any(is_cjk);
        let font = self.font(&marks, cjk, size, bold);
        let width = self
            .shaper
            .shape(&text, &font)
            .iter()
            .map(|c| c.advance)
            .sum();
        Unit {
            start: 0,
            end: 0,
            width,
            kind: Kind::Label,
            metrics: self.shaper.metrics(&font),
            color: self.color(&marks),
            text,
            font,
            marks: Marks {
                underline: false,
                strike: false,
                ..marks
            },
            cjk,
            no_start: false,
            no_end: true,
        }
    }

    fn para(&mut self, para: &Para, width: f32, slide: bool) -> ParaBox {
        let (look, size, bold) = hinted(para);
        let size = size.or(slide.then_some(SLIDE_PT));
        let mut units = Vec::new();
        if let Some(label) = self.label(&look) {
            units.push(self.label_unit(label, para, size, bold));
            if let Some(list) = &look.list {
                let suffix = match list.suffix {
                    LabelSuffix::Tab => Some(Kind::Tab),
                    LabelSuffix::Space => Some(Kind::Space),
                    LabelSuffix::Nothing => None,
                };
                if let Some(kind) = suffix {
                    let mut gap = units[0].clone();
                    gap.text = if matches!(kind, Kind::Space) {
                        " ".into()
                    } else {
                        String::new()
                    };
                    gap.width = gap.font.size / 2.0;
                    gap.kind = if matches!(kind, Kind::Tab) {
                        Kind::Tab
                    } else {
                        Kind::Label
                    };
                    units.push(gap);
                }
            }
        }
        let labels = units.len();
        units.extend(self.units(para, size, bold));
        let mark_font = self.font(&look.mark, false, size, bold);
        let mark_metrics = self.shaper.metrics(&mark_font);
        let end = para.text().len();

        let left = pt(look.left);
        let right = pt(look.right);
        let first = pt(look.first_line);
        let tab_stops: Vec<(f32, TabKind)> = look.tabs.iter().map(|t| (pt(t.at), t.kind)).collect();

        let mut lines = Vec::new();
        let mut i = 0;
        let mut first_line = true;
        loop {
            let indent = left + if first_line { first } else { 0.0 };
            let avail = (width - indent - right).max(1.0);
            let mut x = 0.0;
            let mut j = i;
            let mut last_break: Option<usize> = None;
            let mut forced = false;
            while j < units.len() {
                let unit = &units[j];
                if matches!(unit.kind, Kind::Break) {
                    j += 1;
                    forced = true;
                    break;
                }
                let w = if matches!(unit.kind, Kind::Tab) {
                    let abs = indent + x;
                    // A label's tab goes to the hanging indent when that is ahead.
                    let next = if j < labels && left > abs + 0.5 {
                        left
                    } else {
                        tab_stops
                            .iter()
                            .map(|t| t.0)
                            .find(|t| *t > abs + 0.5)
                            .unwrap_or(((abs / DEFAULT_TAB).floor() + 1.0) * DEFAULT_TAB)
                    };
                    next - abs
                } else {
                    unit.width
                };
                if x + w > avail && !matches!(unit.kind, Kind::Space) && j > i {
                    if let Some(b) = last_break {
                        j = b + 1;
                    }
                    break;
                }
                units[j].width = w;
                x += w;
                let next = units.get(j + 1);
                let breakable = next.is_none_or(|n| {
                    let opportunity = matches!(units[j].kind, Kind::Space | Kind::Picture(..))
                        || units[j].cjk
                        || n.cjk
                        || matches!(n.kind, Kind::Picture(..));
                    opportunity && !n.no_start && !units[j].no_end
                });
                if breakable && j >= labels {
                    last_break = Some(j);
                }
                j += 1;
            }
            let slice = &units[i..j];
            lines.push(self.line(
                para,
                &look,
                slice,
                indent,
                avail,
                mark_metrics,
                j >= units.len() || forced,
                if j >= units.len() {
                    end
                } else {
                    units[j].start
                },
                if i >= units.len() {
                    end
                } else {
                    units[i..j]
                        .iter()
                        .find(|u| !matches!(u.kind, Kind::Label) && u.end > u.start)
                        .map(|u| u.start)
                        .unwrap_or(if j < units.len() { units[j].start } else { end })
                },
            ));
            i = j;
            first_line = false;
            if i >= units.len() && !forced {
                break;
            }
            if i >= units.len() && forced {
                // A break at the very end leaves an empty last line.
                lines.push(self.line(para, &look, &[], left, avail, mark_metrics, true, end, end));
                break;
            }
        }
        ParaBox {
            before: pt(look.before),
            after: pt(look.after),
            page_break_before: look.page_break_before,
            lines,
            shading: look.shading,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn line(
        &self,
        para: &Para,
        look: &ParaLook,
        units: &[Unit],
        indent: f32,
        avail: f32,
        mark: Metrics,
        last: bool,
        end: usize,
        start: usize,
    ) -> Laid {
        let (mut ascent, mut descent, mut gap) = (0.0f32, 0.0f32, 0.0f32);
        for u in units {
            match u.kind {
                Kind::Picture(_, _, h) => ascent = ascent.max(h),
                Kind::Break => {}
                _ => {
                    ascent = ascent.max(u.metrics.ascent);
                    descent = descent.max(u.metrics.descent);
                    gap = gap.max(u.metrics.line_gap);
                }
            }
        }
        if ascent == 0.0 && descent == 0.0 {
            ascent = mark.ascent;
            descent = mark.descent;
            gap = mark.line_gap;
        }
        let natural = ascent + descent + gap;
        let (height, above) = match look.line {
            LineSpacing::Auto(n) => (natural * n as f32 / 240.0, 0.0),
            LineSpacing::AtLeast(t) => {
                let h = natural.max(pt(t));
                (h, h - natural)
            }
            LineSpacing::Exact(t) => {
                let h = pt(t);
                (h, (h - natural) * 0.8)
            }
        };
        let baseline = above + ascent + gap / 2.0;

        // Trailing spaces hang past the edge and do not count for alignment.
        let content: f32 = units
            .iter()
            .rev()
            .skip_while(|u| matches!(u.kind, Kind::Space | Kind::Break))
            .map(|u| u.width)
            .sum();
        let slack = (avail - content).max(0.0);
        let mut shift = match para.align {
            Align::Center => slack / 2.0,
            Align::Right => slack,
            _ => 0.0,
        };
        let justify = para.align == Align::Justify && !last;
        let gaps = if justify {
            units
                .iter()
                .rev()
                .skip_while(|u| matches!(u.kind, Kind::Space | Kind::Break))
                .count()
                .saturating_sub(1)
        } else {
            0
        };
        let spread = if gaps > 0 { slack / gaps as f32 } else { 0.0 };
        if justify {
            shift = 0.0;
        }

        let mut items = Vec::new();
        let mut stops = Vec::new();
        let mut x = indent + shift;
        let mut run: Option<(f32, String, FontKey, Rgb)> = None;
        let flush = |run: &mut Option<(f32, String, FontKey, Rgb)>, items: &mut Vec<Item>| {
            if let Some((x, text, font, color)) = run.take()
                && !text.trim().is_empty()
            {
                items.push(Item::Text {
                    x,
                    baseline,
                    text,
                    font,
                    color,
                });
            }
        };
        for (k, u) in units.iter().enumerate() {
            // Labels and their tab are drawn but hold no text: no stops.
            if !matches!(u.kind, Kind::Label) && u.end > u.start {
                stops.push((u.start, x));
            }
            if let Some(color) = u.marks.highlight {
                items.push(Item::Fill {
                    rect: Rect {
                        x,
                        y: baseline - u.metrics.ascent,
                        width: u.width,
                        height: u.metrics.ascent + u.metrics.descent,
                    },
                    color,
                });
            }
            match &u.kind {
                Kind::Glyph | Kind::Label | Kind::Space if !u.text.is_empty() => {
                    let joins = !justify
                        && run.as_ref().is_some_and(|(_, _, font, color)| {
                            *font == u.font && *color == u.color
                        });
                    if joins {
                        if let Some(r) = run.as_mut() {
                            r.1.push_str(&u.text);
                        }
                    } else {
                        flush(&mut run, &mut items);
                        run = Some((x, u.text.clone(), u.font.clone(), u.color));
                    }
                }
                Kind::Picture(path, w, h) => {
                    flush(&mut run, &mut items);
                    items.push(Item::Picture {
                        rect: Rect {
                            x,
                            y: baseline - h,
                            width: *w,
                            height: *h,
                        },
                        path: path.clone(),
                    });
                }
                _ => flush(&mut run, &mut items),
            }
            let lines_y = [
                (u.marks.underline, baseline + u.metrics.descent * 0.5),
                (u.marks.strike, baseline - u.metrics.ascent * 0.3),
            ];
            for (on, y) in lines_y {
                if on && !matches!(u.kind, Kind::Break | Kind::Label) {
                    items.push(Item::Rule {
                        from: (x, y),
                        to: (x + u.width, y),
                        width: (u.font.size / 18.0).max(0.5),
                        color: u.color,
                    });
                }
            }
            x += u.width;
            if justify && k + 1 < units.len() {
                x += spread;
            }
        }
        flush(&mut run, &mut items);
        let ends_with_break = units.last().is_some_and(|u| matches!(u.kind, Kind::Break));
        if !ends_with_break {
            stops.push((end, x));
        }
        if stops.is_empty() {
            stops.push((start, indent + shift));
        }
        Laid {
            height,
            baseline,
            items,
            para: para.id,
            range: start..end,
            stops,
        }
    }

    /// Blocks stacked without pages: a table cell, a header, a text box.
    fn stack(&mut self, blocks: &[Block], width: f32) -> (Vec<(f32, Laid)>, Vec<Item>, f32) {
        let mut placed = Vec::new();
        let mut extra = Vec::new();
        let mut y = 0.0;
        let mut first = true;
        for block in blocks {
            match block {
                Block::Paragraph(p) => {
                    let b = self.para(p, width, false);
                    if !first {
                        y += b.before;
                    }
                    for line in b.lines {
                        let h = line.height;
                        placed.push((y, line));
                        y += h;
                    }
                    y += b.after;
                }
                Block::Table(t) => {
                    let (rows, items) = self.table(t, width);
                    for (row_y, laid) in rows {
                        placed.extend(laid.into_iter().map(|(ly, l)| (y + row_y + ly, l)));
                    }
                    for item in items.into_iter() {
                        extra.push(translate(&item, 0.0, y));
                    }
                    y += self.last_table_height;
                }
                Block::Opaque { .. } => {}
            }
            first = false;
        }
        (placed, extra, y)
    }
}

/// Rows of a laid-out table: each row's top, the lines in it, its height.
type Rows = Vec<(f32, Vec<(f32, Laid)>)>;

impl Ctx<'_> {
    fn columns(&self, t: &Table, width: f32) -> Vec<f32> {
        let count = t.rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
        if t.look.columns.len() == count {
            let cols: Vec<f32> = t.look.columns.iter().map(|c| pt(*c)).collect();
            let total: f32 = cols.iter().sum();
            if total > width && total > 0.0 {
                return cols.iter().map(|c| c * width / total).collect();
            }
            return cols;
        }
        vec![width / count as f32; count]
    }

    /// Lays out a table at the origin; returns its rows and the fills and
    /// borders, and leaves its height in `last_table_height`.
    fn table(&mut self, t: &Table, width: f32) -> (Rows, Vec<Item>) {
        let cols = self.columns(t, width);
        let default_look = t.look == Default::default();
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
        let border_of = |b: Option<Border>| b.or(default_look.then_some(grid));
        let mut rows = Vec::new();
        let mut items = Vec::new();
        let mut y = 0.0;
        for (r, row) in t.rows.iter().enumerate() {
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
                let (placed, extra, h) = self.stack(&row[c], inner);
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
                let look = t.look.cells.get(r).and_then(|row| row.get(c));
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
                    items.push(translate(&item, x + pad[3], y + dy));
                }
                for (ly, mut line) in placed {
                    line.items = line
                        .items
                        .iter()
                        .map(|i| translate(i, x + pad[3], 0.0))
                        .collect();
                    for s in line.stops.iter_mut() {
                        s.1 += x + pad[3];
                    }
                    laid.push((dy + ly, line));
                }
                let borders = look.and_then(|l| l.borders).unwrap_or(t.look.borders);
                let left = if c == 0 {
                    border_of(borders.left)
                } else {
                    border_of(t.look.inside_v.or(borders.left))
                };
                let top = if r == 0 {
                    border_of(borders.top)
                } else {
                    border_of(t.look.inside_h.or(borders.top))
                };
                let sides = [
                    (top, (x, y), (x + w, y)),
                    (left, (x, y), (x, y + height)),
                    (border_of(borders.right), (x + w, y), (x + w, y + height)),
                    (
                        border_of(borders.bottom),
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
            rows.push((y, laid));
            y += height;
        }
        self.last_table_height = y;
        (rows, items)
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
    let mut cx = Ctx {
        shaper,
        defaults: doc.defaults(),
        base_size: BODY_PT,
        counters: HashMap::new(),
        page_number: 1,
        pages_total: 1,
        last_table_height: 0.0,
    };
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
        match block {
            Block::Paragraph(p) => {
                let b = cx.para(p, width, false);
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
            Block::Table(t) => {
                let (rows, items) = cx.table(t, width);
                // Rows move to the next page whole; fills and borders follow their row.
                let mut row_tops: Vec<(f32, f32, f32, usize)> = Vec::new();
                let bounds: Vec<f32> = rows
                    .iter()
                    .map(|r| r.0)
                    .chain([cx.last_table_height])
                    .collect();
                for (k, (row_y, laid)) in rows.into_iter().enumerate() {
                    let h = bounds[k + 1] - bounds[k];
                    if cur.y + h > cur.bottom && !cur.at_top() {
                        cur.new_page(&sections);
                    }
                    let page = cur.page.unwrap_or(0);
                    let dy = cur.y - row_y;
                    row_tops.push((row_y, row_y + h, dy, page));
                    for (ly, line) in laid {
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
                // Place each fill or border with the row it starts in.
                for item in items {
                    let y = match &item {
                        Item::Fill { rect, .. }
                        | Item::Picture { rect, .. }
                        | Item::Gradient { rect, .. } => rect.y,
                        Item::Path { segments, .. } => segments
                            .iter()
                            .filter_map(|s| match s {
                                Seg::Move(_, y) | Seg::Line(_, y) => Some(*y),
                                _ => None,
                            })
                            .fold(f32::MAX, f32::min),
                        Item::Rule { from, to, .. } => from.1.min(to.1),
                        Item::Text { baseline, .. } => *baseline,
                    };
                    let row = row_tops
                        .iter()
                        .position(|(a, b, _, _)| y >= *a - 0.01 && y < *b - 0.01)
                        .unwrap_or(row_tops.len().saturating_sub(1));
                    if let Some((_, _, dy, page)) = row_tops.get(row) {
                        let moved = translate(&item, cur.left, *dy);
                        let items = &mut cur.layout.pages[*page].items;
                        // Fills go under the text, borders over it.
                        if matches!(moved, Item::Fill { .. }) {
                            items.insert(0, moved);
                        } else {
                            items.push(moved);
                        }
                    }
                }
            }
            Block::Opaque { .. } => {}
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
            let (placed, extra, h) = cx.stack(blocks, width);
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

pub(crate) fn slide(slide: &Slide, index: usize, shaper: &dyn Shaper) -> Layout {
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
    // Placeholders whose place comes from the slide's layout have no frame
    // until the binding resolves it: stack them down the slide, the first
    // as the title.
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
            let (rows, items) = cx.table(table, w);
            page.items.extend(items.iter().map(|i| translate(i, x, y)));
            for (row_y, laid) in rows {
                for (ly, line) in laid {
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
    let mut placed = Vec::new();
    let mut ty = 0.0;
    for (k, p) in shape.paras.iter().enumerate() {
        let b = cx.para(p, inner, auto.is_none());
        if k > 0 {
            ty += b.before;
        }
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
            VAlign::Top => 0.0,
            VAlign::Center => free / 2.0,
            VAlign::Bottom => free,
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
        // A gradient in a drawn outline: its first colour, until the painter does more.
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
