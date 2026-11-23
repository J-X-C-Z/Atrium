//! Paragraph layout: tokens, UAX #14 line breaking with Word's Chinese
//! rules, justification, tabs and line height.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;

use atrium_office::look::{
    Baseline, Float, LabelSuffix, LineSpacing, NumFormat, ParaLook, Rgb, TabKind, Wrap,
};
use atrium_office::{Align, AtomKind, Block, Inline, Marks, NodeId, Para, ParaStyle};
use unicode_linebreak::{BreakOpportunity, linebreaks};

use crate::{Cluster, FontKey, Item, Metrics, Rect};

use super::{Ctx, pt};

pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2FFF | 0x3000..=0x30FF | 0x3100..=0x31FF
        | 0x3200..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F
        | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// May not start a line.
const NO_START: &str = "、。，．：；？！）〕］｝〉》」』】〙〗〟’”｠»ヽヾーァィゥェォッャュョヮヵヶぁぃぅぇぉっゃゅょゎ々〻‐゠–〜・,.:;?!)]}%";
/// May not end a line.
const NO_END: &str = "（〔［｛〈《「『【〘〖〝‘“｟«([{";

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Glyph,
    Space,
    Tab,
    Break,
    Picture(Option<PathBuf>, f32, f32),
    /// A list label: drawn, but no caret stops in it.
    Label,
    /// Word's `autoSpaceDE`/`autoSpaceDN` quarter em.
    Gap,
    /// A floating picture: takes no room in the line.
    Float,
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
    break_after: bool,
    /// Shift from the line's baseline, positive up.
    raise: f32,
    leader: Option<char>,
    /// The footnote this unit refers to, if any.
    foot: Option<NodeId>,
}

/// A picture that leaves the line and is placed on the page.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct FloatAnchor {
    pub id: NodeId,
    pub path: Option<PathBuf>,
    pub width: f32,
    pub height: f32,
    pub float: Float,
}

/// A placed float, in page coordinates, for wrapping the text beside it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FloatBox {
    pub rect: Rect,
    pub wrap: Wrap,
    pub clear: [f32; 4],
}

pub(crate) struct WrapCtx<'a> {
    /// The paragraph's first line's page y.
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub floats: &'a [FloatBox],
}

/// A line laid out at the text area's origin: x from its left edge, y from
/// the line's top.
#[derive(Clone, Debug)]
pub(crate) struct Laid {
    /// The line's top within its paragraph or stacked block.
    pub y: f32,
    pub height: f32,
    pub baseline: f32,
    pub items: Vec<Item>,
    pub para: NodeId,
    pub range: Range<usize>,
    pub stops: Vec<(usize, f32)>,
    #[allow(dead_code)]
    pub refs: Vec<NodeId>,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct ParaOut {
    pub before: f32,
    pub after: f32,
    pub page_break_before: bool,
    pub keep_next: bool,
    pub keep_lines: bool,
    pub widow_control: bool,
    pub contextual: bool,
    pub lines: Vec<Laid>,
    pub shading: Option<Rgb>,
    pub floats: Vec<FloatAnchor>,
}

/// Sizes and spacing for a paragraph whose file said nothing about them,
/// so a document still reads as headings and body.
pub(crate) fn hinted(para: &Para) -> (ParaLook, Option<f32>, bool) {
    let mut look = para.look.clone();
    if look != ParaLook::default() {
        return (look, None, false);
    }
    let (size, bold) = match &para.style {
        ParaStyle::Title => (Some(22.0), true),
        ParaStyle::Heading(1) => (Some(16.0), true),
        ParaStyle::Heading(2) => (Some(14.0), true),
        ParaStyle::Heading(3) => (Some(12.0), true),
        ParaStyle::Heading(_) => (Some(10.5), true),
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
            list: super::HINTED_LIST + ordered as u32,
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

fn is_digit(u: &Unit) -> bool {
    u.text.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn is_latin(u: &Unit) -> bool {
    u.text
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
}

fn plain_unit(u: &Unit) -> bool {
    matches!(u.kind, Kind::Glyph | Kind::Space)
}

impl Ctx<'_> {
    pub(crate) fn font(&self, marks: &Marks, cjk: bool, size: Option<f32>, bold: bool) -> FontKey {
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
        if marks.baseline != Baseline::Normal {
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

    fn effective_size(&self, marks: &Marks, size: Option<f32>) -> f32 {
        marks
            .size
            .or(self.defaults.size)
            .map(pt)
            .or(size)
            .unwrap_or(self.base_size)
    }

    pub(crate) fn color(&self, marks: &Marks) -> Rgb {
        marks.color.or(self.defaults.color).unwrap_or(Rgb::BLACK)
    }

    pub(crate) fn shape(&mut self, text: &str, font: &FontKey) -> Vec<Cluster> {
        let key = (
            text.to_owned(),
            font.family.clone(),
            font.size.to_bits(),
            font.bold,
            font.italic,
            font.cjk,
        );
        if let Some(found) = self.shapes.get(&key) {
            return found.clone();
        }
        let out = self.shaper.shape(text, font);
        self.shapes.insert(key, out.clone());
        out
    }

    pub(crate) fn metrics(&mut self, font: &FontKey) -> Metrics {
        let key = (
            font.family.clone(),
            font.size.to_bits(),
            font.bold,
            font.italic,
            font.cjk,
        );
        if let Some(found) = self.metrics_cache.get(&key) {
            return *found;
        }
        let out = self.shaper.metrics(font);
        self.metrics_cache.insert(key, out);
        out
    }

    fn width(&mut self, text: &str, font: &FontKey) -> f32 {
        self.shape(text, font).iter().map(|c| c.advance).sum()
    }

    pub(crate) fn label(&mut self, look: &ParaLook) -> Option<String> {
        let Some(list) = look.list.as_ref() else {
            self.counters.retain(|id, _| *id < super::HINTED_LIST);
            return None;
        };
        if list.list >= super::HINTED_LIST {
            let other = super::HINTED_LIST + (list.list == super::HINTED_LIST) as u32;
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
            text = text.replace(&format!("%{}", i + 1), &super::number(n, format));
        }
        if let Some(NumFormat::Bullet(mark)) = list.formats.get(level) {
            text = mark.clone();
        }
        Some(text)
    }

    fn text_units(
        &mut self,
        text: &str,
        at: usize,
        marks: &Marks,
        size: Option<f32>,
        bold: bool,
        out: &mut Vec<Unit>,
    ) {
        enum Piece {
            Text(bool),
            Tab,
            Break,
        }
        let mut pieces: Vec<(usize, usize, Piece)> = Vec::new();
        let mut start = 0usize;
        let mut current: Option<bool> = None;
        for (i, c) in text.char_indices() {
            if c == '\t' || c == '\n' {
                if let Some(cjk) = current
                    && start < i
                {
                    pieces.push((start, i, Piece::Text(cjk)));
                }
                pieces.push((i, i + 1, if c == '\t' { Piece::Tab } else { Piece::Break }));
                start = i + 1;
                current = None;
                continue;
            }
            let cjk = is_cjk(c);
            if current != Some(cjk) {
                if let Some(old) = current
                    && start < i
                {
                    pieces.push((start, i, Piece::Text(old)));
                }
                start = i;
                current = Some(cjk);
            }
        }
        if let Some(cjk) = current
            && start < text.len()
        {
            pieces.push((start, text.len(), Piece::Text(cjk)));
        }
        for (from, to, piece) in pieces {
            if matches!(piece, Piece::Tab | Piece::Break) {
                let font = self.font(marks, false, size, bold);
                let metrics = self.metrics(&font);
                out.push(Unit {
                    start: at + from,
                    end: at + to,
                    width: 0.0,
                    kind: if matches!(piece, Piece::Tab) {
                        Kind::Tab
                    } else {
                        Kind::Break
                    },
                    text: String::new(),
                    font,
                    color: self.color(marks),
                    marks: marks.clone(),
                    metrics,
                    cjk: false,
                    no_start: false,
                    no_end: false,
                    break_after: false,
                    raise: 0.0,
                    leader: None,
                    foot: None,
                });
                continue;
            }
            let Piece::Text(cjk) = piece else {
                continue;
            };
            let piece = &text[from..to];
            let font = self.font(marks, cjk, size, bold);
            let metrics = self.metrics(&font);
            let original = self.effective_size(marks, size);
            let raise = match marks.baseline {
                Baseline::Normal => 0.0,
                Baseline::Superscript => original * 0.33,
                Baseline::Subscript => -original * 0.14,
            };
            let clusters = self.shape(piece, &font);
            for (k, cluster) in clusters.iter().enumerate() {
                let end = clusters.get(k + 1).map(|c| c.start).unwrap_or(piece.len());
                let s = &piece[cluster.start..end];
                let first = s.chars().next().unwrap_or(' ');
                let kind = if first == '\t' {
                    Kind::Tab
                } else if first == '\n' {
                    Kind::Break
                } else if first == ' ' {
                    Kind::Space
                } else {
                    Kind::Glyph
                };
                out.push(Unit {
                    start: at + from + cluster.start,
                    end: at + from + end,
                    width: cluster.advance,
                    kind,
                    text: s.to_string(),
                    font: font.clone(),
                    color: self.color(marks),
                    marks: marks.clone(),
                    metrics,
                    cjk,
                    no_start: NO_START.contains(first),
                    no_end: s.chars().last().is_some_and(|c| NO_END.contains(c)),
                    break_after: false,
                    raise,
                    leader: None,
                    foot: None,
                });
            }
        }
    }

    fn units(
        &mut self,
        para: &Para,
        look: &ParaLook,
        size: Option<f32>,
        bold: bool,
    ) -> (Vec<Unit>, Vec<FloatAnchor>) {
        let mut out = Vec::new();
        let mut floats = Vec::new();
        let mut at = 0usize;
        let atom_marks = look.mark.clone();
        for inline in &para.inlines {
            match inline {
                Inline::Text { text, marks } => {
                    self.text_units(text, at, marks, size, bold, &mut out);
                    at += text.len();
                }
                Inline::Break | Inline::Tab => {
                    let font = self.font(&atom_marks, false, size, bold);
                    let metrics = self.metrics(&font);
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
                        metrics,
                        font,
                        color: self.color(&atom_marks),
                        marks: atom_marks.clone(),
                        cjk: false,
                        no_start: false,
                        no_end: false,
                        break_after: false,
                        raise: 0.0,
                        leader: None,
                        foot: None,
                    });
                    at += 1;
                }
                Inline::Atom(atom) => {
                    let len = '\u{FFFC}'.len_utf8();
                    let mut marks = atom_marks.clone();
                    let shown = match &atom.kind {
                        AtomKind::Field(code) if code.trim_start().starts_with("NUMPAGES") => {
                            Some(self.pages_total.max(1).to_string())
                        }
                        AtomKind::Field(code) if code.trim_start().starts_with("PAGE") => {
                            Some(self.page_number.to_string())
                        }
                        AtomKind::Footnote if atom.look.shown.is_none() => {
                            self.footnote_number += 1;
                            marks.baseline = Baseline::Superscript;
                            Some(self.footnote_number.to_string())
                        }
                        _ => atom.look.shown.clone(),
                    };
                    let font = self.font(&marks, false, size, bold);
                    let metrics = self.metrics(&font);
                    let original = self.effective_size(&marks, size);
                    let raise = match marks.baseline {
                        Baseline::Superscript => original * 0.33,
                        Baseline::Subscript => -original * 0.14,
                        Baseline::Normal => 0.0,
                    };
                    let foot = matches!(atom.kind, AtomKind::Footnote).then_some(atom.id);
                    if let (AtomKind::Image(path), Some(float)) = (&atom.kind, atom.look.float) {
                        let (w, h) = atom
                            .look
                            .size
                            .map(|(w, h)| (pt(w), pt(h)))
                            .unwrap_or((metrics.ascent * 3.0, metrics.ascent * 3.0));
                        floats.push(FloatAnchor {
                            id: atom.id,
                            path: path.clone(),
                            width: w,
                            height: h,
                            float,
                        });
                        out.push(Unit {
                            start: at,
                            end: at + len,
                            width: 0.0,
                            kind: Kind::Float,
                            text: String::new(),
                            font,
                            color: Rgb::BLACK,
                            marks,
                            metrics,
                            cjk: false,
                            no_start: false,
                            no_end: false,
                            break_after: false,
                            raise: 0.0,
                            leader: None,
                            foot: None,
                        });
                        at += len;
                        continue;
                    }
                    let (kind, width, text) = match (&atom.kind, atom.look.size, shown) {
                        (AtomKind::Image(path), size, _) => {
                            let (w, h) = size
                                .map(|(w, h)| (pt(w), pt(h)))
                                .unwrap_or((metrics.ascent * 3.0, metrics.ascent * 3.0));
                            (Kind::Picture(path.clone(), w, h), w, String::new())
                        }
                        (_, _, Some(text)) => {
                            let width = self.width(&text, &font);
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
                        color: self.color(&marks),
                        font,
                        marks,
                        metrics,
                        cjk: false,
                        no_start: false,
                        no_end: false,
                        break_after: false,
                        raise,
                        leader: None,
                        foot,
                    });
                    at += len;
                }
            }
        }
        (out, floats)
    }

    fn label_unit(&mut self, text: String, para: &Para, size: Option<f32>, bold: bool) -> Unit {
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
        let width = self.width(&text, &font);
        Unit {
            start: 0,
            end: 0,
            width,
            kind: Kind::Label,
            metrics: self.metrics(&font),
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
            break_after: false,
            raise: 0.0,
            leader: None,
            foot: None,
        }
    }

    fn mark_break_opportunities(&mut self, para: &Para, units: &mut [Unit], labels: usize) {
        let text = para.text();
        let mut allowed = HashSet::<usize>::new();
        for (at, kind) in linebreaks(&text) {
            if matches!(
                kind,
                BreakOpportunity::Allowed | BreakOpportunity::Mandatory
            ) {
                allowed.insert(at);
            }
        }
        for (i, unit) in units.iter_mut().enumerate() {
            if i < labels {
                continue;
            }
            unit.break_after = match unit.kind {
                Kind::Break => false,
                Kind::Picture(..) | Kind::Float => true,
                _ => allowed.contains(&unit.end),
            };
        }
    }

    fn apply_kinsoku(&self, units: &mut [Unit]) {
        for i in 0..units.len() {
            if !units[i].break_after {
                continue;
            }
            let next = units.get(i + 1);
            if units[i].no_end || next.is_some_and(|n| n.no_start) {
                units[i].break_after = false;
            }
        }
    }

    fn add_auto_spaces(&mut self, units: &mut Vec<Unit>, look: &ParaLook) {
        if !look.auto_space_de && !look.auto_space_dn {
            return;
        }
        let mut i = 1;
        while i < units.len() {
            let (left, right) = (&units[i - 1], &units[i]);
            if !plain_unit(left) || !plain_unit(right) {
                i += 1;
                continue;
            }
            let boundary = (left.cjk && (is_latin(right) || is_digit(right)))
                || (right.cjk && (is_latin(left) || is_digit(left)));
            let numbers = is_digit(left) || is_digit(right);
            let add = boundary
                && if numbers {
                    look.auto_space_dn
                } else {
                    look.auto_space_de
                };
            if !add {
                i += 1;
                continue;
            }
            let font = if left.cjk {
                left.font.clone()
            } else {
                right.font.clone()
            };
            let size = font.size.max(if left.cjk {
                right.font.size
            } else {
                left.font.size
            });
            let metrics = self.metrics(&font);
            let gap = Unit {
                start: left.end,
                end: left.end,
                width: size / 4.0,
                kind: Kind::Gap,
                text: String::new(),
                font,
                color: left.color,
                marks: Marks::default(),
                metrics,
                cjk: false,
                no_start: false,
                no_end: false,
                break_after: units[i - 1].break_after,
                raise: 0.0,
                leader: None,
                foot: None,
            };
            units[i - 1].break_after = false;
            units.insert(i, gap);
            i += 2;
        }
    }
}

fn tail_width(units: &[Unit], from: usize) -> f32 {
    units[from..]
        .iter()
        .take_while(|u| !matches!(u.kind, Kind::Tab | Kind::Break))
        .map(|u| u.width)
        .sum()
}

fn decimal_prefix(units: &[Unit], from: usize) -> Option<f32> {
    let mut width = 0.0;
    for unit in units[from..].iter() {
        if matches!(unit.kind, Kind::Tab | Kind::Break) {
            break;
        }
        if let Some(dot) = unit.text.find('.') {
            let before = &unit.text[..dot];
            if before.is_empty() {
                return Some(width);
            }
            return Some(
                width
                    + before.chars().count() as f32 * unit.width
                        / unit.text.chars().count().max(1) as f32,
            );
        }
        width += unit.width;
    }
    None
}

fn next_stop(
    abs: f32,
    stops: &[(f32, TabKind, Option<char>)],
    default: f32,
) -> (f32, TabKind, Option<char>) {
    if let Some(found) = stops.iter().copied().find(|s| s.0 > abs + 0.01) {
        return found;
    }
    let step = default.max(1.0);
    let at = ((abs / step).floor() + 1.0) * step;
    (at, TabKind::Left, None)
}

fn tab_width(
    units: &[Unit],
    j: usize,
    abs: f32,
    stops: &[(f32, TabKind, Option<char>)],
    default: f32,
    left: f32,
    labels: usize,
) -> (f32, Option<char>) {
    if j < labels && left > abs + 0.5 {
        return ((left - abs).max(0.0), None);
    }
    let mut at = abs;
    for _ in 0..100 {
        let (stop, kind, leader) = next_stop(at, stops, default);
        let tail = tail_width(units, j + 1);
        let target = match kind {
            TabKind::Left => stop,
            TabKind::Center => stop - tail / 2.0,
            TabKind::Right => stop - tail,
            TabKind::Decimal => stop - decimal_prefix(units, j + 1).unwrap_or(tail),
        };
        if target > at + 0.01 {
            return (target - at, leader);
        }
        at = stop;
    }
    (default.max(1.0), None)
}

struct LineCfg<'a> {
    look: &'a ParaLook,
    para: &'a Para,
    mark: Metrics,
    grid: Option<f32>,
}

#[allow(clippy::too_many_arguments)]
fn make_line(
    cx: &mut Ctx<'_>,
    units: &[Unit],
    y: f32,
    indent: f32,
    avail: f32,
    cfg: &LineCfg<'_>,
    last: bool,
    forced: bool,
    start: usize,
    end: usize,
) -> Laid {
    let (mut ascent, mut descent, mut gap) = (0.0f32, 0.0f32, 0.0f32);
    for u in units {
        match u.kind {
            Kind::Picture(_, _, h) => ascent = ascent.max(h),
            Kind::Break | Kind::Float => {}
            _ => {
                if u.raise > 0.0 {
                    ascent = ascent.max(u.metrics.ascent + u.raise);
                    descent = descent.max((u.metrics.descent - u.raise).max(0.0));
                } else {
                    ascent = ascent.max((u.metrics.ascent + u.raise).max(0.0));
                    descent = descent.max(u.metrics.descent - u.raise);
                }
                gap = gap.max(u.metrics.line_gap);
            }
        }
    }
    if ascent == 0.0 && descent == 0.0 {
        ascent = cfg.mark.ascent;
        descent = cfg.mark.descent;
        gap = cfg.mark.line_gap;
    }
    let natural = ascent + descent + gap;
    let (mut height, mut baseline) = match cfg.look.line {
        LineSpacing::Auto(n) => {
            let h = natural * n as f32 / 240.0;
            (h, ascent + gap / 2.0)
        }
        LineSpacing::AtLeast(t) => {
            let h = natural.max(pt(t));
            (h, h - natural + ascent + gap / 2.0)
        }
        LineSpacing::Exact(t) => {
            let h = pt(t);
            (h, (h - natural) * 0.8 + ascent + gap / 2.0)
        }
    };
    if cfg.look.snap_to_grid
        && let Some(pitch) = cfg.grid.filter(|p| *p > 0.5)
    {
        let count = (natural / pitch - 0.001).ceil().max(1.0);
        height = count * pitch;
        baseline = (height - natural) / 2.0 + ascent + gap / 2.0;
    }

    // Trailing spaces hang past the edge and do not count for alignment.
    let content_end = units
        .iter()
        .rposition(|u| !matches!(u.kind, Kind::Space | Kind::Break | Kind::Gap))
        .map(|i| i + 1)
        .unwrap_or(0);
    let content: f32 = units[..content_end].iter().map(|u| u.width).sum();
    let slack = (avail - content).max(0.0);
    let mut shift = match cfg.para.align {
        Align::Center => slack / 2.0,
        Align::Right => slack,
        _ => 0.0,
    };
    let justify = ((cfg.para.align == Align::Justify && !last) || cfg.look.distribute)
        && !forced
        && content > 0.0
        && slack > 0.0;
    let spaces = units[..content_end]
        .iter()
        .filter(|u| matches!(u.kind, Kind::Space))
        .count();
    let text_boundaries = units[..content_end]
        .iter()
        .filter(|u| matches!(u.kind, Kind::Glyph | Kind::Space))
        .count()
        .saturating_sub(1);
    let inter = justify && (cfg.look.distribute || spaces == 0) && text_boundaries > 0;
    let space_extra = if justify && !inter && spaces > 0 {
        slack / spaces as f32
    } else {
        0.0
    };
    let spread = if inter {
        slack / text_boundaries as f32
    } else {
        0.0
    };
    if justify {
        shift = 0.0;
    }

    let mut items = Vec::new();
    let mut stops = Vec::new();
    let mut refs = Vec::new();
    let mut x = indent + shift;
    let split = space_extra > 0.0 || spread > 0.0;
    let mut run: Option<(f32, String, FontKey, Rgb, f32)> = None;
    for (k, unit) in units.iter().enumerate() {
        if !matches!(unit.kind, Kind::Label) && unit.end > unit.start {
            stops.push((unit.start, x));
        }
        if let Some(id) = unit.foot {
            refs.push(id);
        }
        let unit_baseline = baseline - unit.raise;
        if let Some(color) = unit.marks.highlight {
            items.push(Item::Fill {
                rect: Rect {
                    x,
                    y: unit_baseline - unit.metrics.ascent,
                    width: unit.width,
                    height: unit.metrics.ascent + unit.metrics.descent,
                },
                color,
            });
        }
        match &unit.kind {
            Kind::Glyph | Kind::Label | Kind::Space if !unit.text.is_empty() => {
                let joins = !split
                    && run.as_ref().is_some_and(|(_, _, font, color, base)| {
                        *font == unit.font
                            && *color == unit.color
                            && (*base - unit_baseline).abs() < 0.01
                    });
                if joins {
                    if let Some(r) = run.as_mut() {
                        r.1.push_str(&unit.text);
                    }
                } else {
                    flush_run(&mut run, &mut items);
                    run = Some((
                        x,
                        unit.text.clone(),
                        unit.font.clone(),
                        unit.color,
                        unit_baseline,
                    ));
                }
            }
            Kind::Tab => {
                flush_run(&mut run, &mut items);
                if let Some(leader) = unit.leader
                    && unit.width > 0.0
                {
                    let ch = leader.to_string();
                    let advance = cx.width(&ch, &unit.font).max(0.1);
                    let count = (unit.width / advance).floor() as usize;
                    if count > 0 {
                        items.push(Item::Text {
                            x,
                            baseline: unit_baseline,
                            text: ch.repeat(count),
                            font: unit.font.clone(),
                            color: unit.color,
                            rotation: 0.0,
                            center: (0.0, 0.0),
                        });
                    }
                }
            }
            Kind::Picture(path, w, h) => {
                flush_run(&mut run, &mut items);
                items.push(Item::Picture {
                    rect: Rect {
                        x,
                        y: unit_baseline - h,
                        width: *w,
                        height: *h,
                    },
                    path: path.clone(),
                });
            }
            _ => flush_run(&mut run, &mut items),
        }
        let (underline_offset, underline_width) = cx.shaper.underline(&unit.font);
        if unit.marks.underline && !matches!(unit.kind, Kind::Break | Kind::Label | Kind::Float) {
            items.push(Item::Rule {
                from: (x, unit_baseline + underline_offset),
                to: (x + unit.width, unit_baseline + underline_offset),
                width: underline_width,
                color: unit.color,
            });
        }
        if unit.marks.strike && !matches!(unit.kind, Kind::Break | Kind::Label | Kind::Float) {
            let y = unit_baseline - unit.metrics.ascent * 0.3;
            items.push(Item::Rule {
                from: (x, y),
                to: (x + unit.width, y),
                width: underline_width,
                color: unit.color,
            });
        }
        x += unit.width
            + if matches!(unit.kind, Kind::Space) && k < content_end {
                space_extra
            } else {
                0.0
            };
        if spread > 0.0 && k + 1 < content_end {
            x += spread;
        }
    }
    flush_run(&mut run, &mut items);
    let ends_with_break = units.last().is_some_and(|u| matches!(u.kind, Kind::Break));
    if !ends_with_break {
        stops.push((end, x));
    }
    if stops.is_empty() {
        stops.push((start, indent + shift));
    }
    Laid {
        y,
        height,
        baseline,
        items,
        para: cfg.para.id,
        range: start..end,
        stops,
        refs,
    }
}

fn flush_run(run: &mut Option<(f32, String, FontKey, Rgb, f32)>, items: &mut Vec<Item>) {
    if let Some((x, text, font, color, baseline)) = run.take()
        && !text.is_empty()
    {
        items.push(Item::Text {
            x,
            baseline,
            text,
            font,
            color,
            rotation: 0.0,
            center: (0.0, 0.0),
        });
    }
}

pub(crate) fn para(cx: &mut Ctx<'_>, para: &Para, width: f32, slide: bool) -> ParaOut {
    para_wrapped(cx, para, width, slide, None)
}

pub(crate) fn para_wrapped(
    cx: &mut Ctx<'_>,
    para: &Para,
    width: f32,
    slide: bool,
    wrap: Option<&WrapCtx<'_>>,
) -> ParaOut {
    let (look, size, bold) = hinted(para);
    let size = size.or(slide.then_some(super::SLIDE_PT));
    let mut units = Vec::new();
    if let Some(label) = cx.label(&look) {
        units.insert(0, cx.label_unit(label, para, size, bold));
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
                units.insert(1, gap);
            }
        }
    }
    let labels = units.len();
    let (content, floats) = cx.units(para, &look, size, bold);
    units.extend(content);
    let end = para.text().len();
    cx.mark_break_opportunities(para, &mut units, labels);
    cx.apply_kinsoku(&mut units);
    cx.add_auto_spaces(&mut units, &look);
    for unit in units.iter_mut().skip(labels) {
        if matches!(unit.kind, Kind::Break) {
            unit.break_after = false;
        }
    }

    let mark_font = cx.font(&look.mark, false, size, bold);
    let mark = cx.metrics(&mark_font);
    let left = pt(look.left);
    let right = pt(look.right);
    let first_indent = pt(look.first_line);
    let stops: Vec<(f32, TabKind, Option<char>)> = look
        .tabs
        .iter()
        .map(|t| (pt(t.at), t.kind, t.leader))
        .collect();
    let grid = look.snap_to_grid.then_some(cx.line_pitch).flatten();
    let cfg = LineCfg {
        look: &look,
        para,
        mark,
        grid,
    };

    let mut lines = Vec::new();
    let mut i = 0;
    let mut first_line = true;
    let mut y_acc = 0.0f32;
    loop {
        let mut left_cut = 0.0f32;
        let mut right_cut = 0.0f32;
        if let Some(w) = wrap {
            let estimate = mark.ascent + mark.descent;
            let mut top = w.top + y_acc;
            loop {
                let mut pushed = false;
                for f in w.floats {
                    if !matches!(f.wrap, Wrap::TopAndBottom) {
                        continue;
                    }
                    let band_top = f.rect.y - f.clear[0];
                    let band_bottom = f.rect.y + f.rect.height + f.clear[2];
                    if top < band_bottom && top + estimate > band_top {
                        let push = band_bottom - top;
                        if push > 0.01 {
                            y_acc += push;
                            top += push;
                            pushed = true;
                        }
                    }
                }
                if !pushed {
                    break;
                }
            }
            for f in w.floats {
                if !matches!(f.wrap, Wrap::Square) {
                    continue;
                }
                let band_top = f.rect.y - f.clear[0];
                let band_bottom = f.rect.y + f.rect.height + f.clear[2];
                if top < band_bottom && top + estimate > band_top {
                    let fx = f.rect.x - f.clear[3];
                    let fr = f.rect.x + f.rect.width + f.clear[1];
                    if fx <= w.left + w.width / 2.0 {
                        left_cut = left_cut.max(fr - w.left);
                    } else {
                        right_cut = right_cut.max(w.left + w.width - fx);
                    }
                }
            }
        }
        let indent = left + if first_line { first_indent } else { 0.0 } + left_cut;
        let avail = (width - indent - right - right_cut).max(1.0);
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
            let abs = indent + x;
            let (w, leader) = if matches!(unit.kind, Kind::Tab) {
                tab_width(&units, j, abs, &stops, cx.default_tab, left, labels)
            } else {
                (unit.width, None)
            };
            let fits = x + w <= avail + 0.01 || matches!(unit.kind, Kind::Space | Kind::Gap);
            if !fits && j > i {
                let hangs = look.overflow_punct
                    && unit.no_start
                    && x < avail - 0.01
                    && x + w - avail <= unit.font.size;
                if !hangs {
                    if let Some(b) = last_break {
                        j = b + 1;
                    }
                    break;
                }
            }
            if matches!(unit.kind, Kind::Tab) {
                units[j].width = w;
                units[j].leader = leader;
            }
            x += w;
            if units[j].break_after && j + 1 < units.len() && j >= labels {
                last_break = Some(j);
            }
            j += 1;
        }
        let line_end = if j >= units.len() {
            end
        } else {
            units[j].start
        };
        let line_start = units[i..j]
            .iter()
            .find(|u| !matches!(u.kind, Kind::Label) && u.end > u.start)
            .map(|u| u.start)
            .unwrap_or(if j < units.len() { units[j].start } else { end });
        lines.push(make_line(
            cx,
            &units[i..j],
            y_acc,
            indent,
            avail,
            &cfg,
            j >= units.len() || forced,
            forced,
            if i >= units.len() { end } else { line_start },
            line_end,
        ));
        y_acc += lines.last().map(|l| l.height).unwrap_or(0.0);
        i = j;
        first_line = false;
        if i >= units.len() {
            if forced {
                lines.push(make_line(
                    cx,
                    &[],
                    y_acc,
                    left,
                    avail,
                    &cfg,
                    true,
                    false,
                    end,
                    end,
                ));
            }
            break;
        }
    }
    ParaOut {
        before: pt(look.before),
        after: pt(look.after),
        page_break_before: look.page_break_before,
        keep_next: look.keep_next,
        keep_lines: look.keep_lines,
        widow_control: look.widow_control,
        contextual: look.contextual,
        lines,
        shading: look.shading,
        floats,
    }
}

/// Natural width of a paragraph's longest unwrapped line, for table autofit.
pub(crate) fn natural_width(cx: &mut Ctx<'_>, para: &Para) -> f32 {
    let (look, size, bold) = hinted(para);
    let (units, _) = cx.units(para, &look, size, bold);
    let mut widest: f32 = units.iter().map(|u| u.width).sum();
    let mut line = 0.0;
    for unit in &units {
        if matches!(unit.kind, Kind::Break) {
            widest = widest.max(line);
            line = 0.0;
        } else {
            line += unit.width;
        }
    }
    let indent = pt(look.left + look.right) + pt(look.first_line).abs();
    widest + indent
}

/// Lays blocks without pages: a table cell, a header, a footer, a text box.
pub(crate) fn stack(
    cx: &mut Ctx<'_>,
    blocks: &[Block],
    width: f32,
) -> (Vec<(f32, Laid)>, Vec<Item>, f32) {
    let mut placed = Vec::new();
    let mut extra = Vec::new();
    let mut y = 0.0;
    let mut first = true;
    let mut after: f32 = 0.0;
    let mut last_style: Option<ParaStyle> = None;
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                let b = para(cx, p, width, false);
                let mut before = if first { 0.0 } else { b.before };
                if b.contextual && last_style.as_ref() == Some(&p.style) {
                    before = 0.0;
                    y -= after;
                    y = y.max(0.0);
                }
                y += before;
                let para_start = y;
                for line in b.lines {
                    let h = line.height;
                    let ly = para_start + line.y;
                    placed.push((ly, line));
                    y = ly + h;
                }
                y += b.after;
                after = b.after;
                last_style = Some(p.style.clone());
            }
            Block::Table(t) => {
                let (rows, _, height) = super::table::table(cx, t, width);
                let mut ty = y;
                for row in rows {
                    for item in row.items {
                        extra.push(super::translate(&item, 0.0, ty));
                    }
                    for (ly, line) in row.lines {
                        placed.push((ty + ly, line));
                    }
                    ty += row.height;
                }
                y += height;
                last_style = None;
                after = 0.0;
            }
            Block::Opaque { .. } => {}
        }
        first = false;
    }
    (placed, extra, y)
}
