//! Lays out a Word document, or a slide, as pages that a UI paints directly:
//! glyph runs at positions, rules, fills and pictures. Each line also keeps
//! where every character boundary sits, so an editor can put a caret,
//! select, and hit-test without measuring anything itself.
//!
//! The engine never touches fonts: a [`Shaper`] that the app provides
//! measures text. The app's shaper is the same one that paints, so what the
//! engine measures is what the screen shows.
//!
//! Units are points. Positions are from the page's top-left corner.

use std::ops::Range;
use std::path::PathBuf;

use atrium_office::look::Rgb;
use atrium_office::{Deck, NodeId, Slide, WordDoc};

mod engine;
mod nav;
mod sheet;

pub use nav::Pos;
pub use sheet::layout_sheet_drawings;

/// A font as the engine asks for it.
#[derive(Clone, Debug, PartialEq)]
pub struct FontKey {
    /// A family name, or empty for the app's default for this script.
    pub family: String,
    /// Points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    /// Whether the text is Chinese, Japanese or Korean, so an empty family
    /// picks a CJK face.
    pub cjk: bool,
}

/// One grapheme cluster of shaped text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cluster {
    /// Where it starts in the shaped text, in bytes.
    pub start: usize,
    /// Its advance in points.
    pub advance: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Above the baseline, in points.
    pub ascent: f32,
    /// Below the baseline, in points, positive.
    pub descent: f32,
    /// Extra space the font asks for between lines.
    pub line_gap: f32,
}

/// Measures text. The app implements it over its text system; tests use
/// [`FixedShaper`].
pub trait Shaper {
    /// `text`'s grapheme clusters in order, each with its advance. The
    /// shaper falls back to other fonts for characters `font` lacks.
    fn shape(&self, text: &str, font: &FontKey) -> Vec<Cluster>;
    fn metrics(&self, font: &FontKey) -> Metrics;
}

/// A shaper where every character is as wide as the font size and Latin
/// characters half that: enough for tests to reason about lines.
#[derive(Clone, Copy, Debug, Default)]
pub struct FixedShaper;

impl Shaper for FixedShaper {
    fn shape(&self, text: &str, font: &FontKey) -> Vec<Cluster> {
        text.char_indices()
            .map(|(start, c)| Cluster {
                start,
                advance: if c.is_ascii() {
                    font.size / 2.0
                } else {
                    font.size
                },
            })
            .collect()
    }

    fn metrics(&self, font: &FontKey) -> Metrics {
        Metrics {
            ascent: font.size * 0.8,
            descent: font.size * 0.2,
            line_gap: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

/// Something to paint, in the order given: later items over earlier ones.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// Text shaped as one run, its first cluster's left edge at `x` and
    /// its baseline at `baseline`. The painter shapes `text` with `font`
    /// again, so the glyphs match what the engine measured.
    Text {
        x: f32,
        baseline: f32,
        text: String,
        font: FontKey,
        color: Rgb,
    },
    Fill {
        rect: Rect,
        color: Rgb,
    },
    /// A straight rule: an underline, a strike, a border.
    Rule {
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
        color: Rgb,
    },
    Picture {
        rect: Rect,
        /// `None` when the picture could not be read: paint a placeholder.
        path: Option<PathBuf>,
    },
    /// An outline in page points: a drawn shape, a chart's line or wedge.
    Path {
        segments: Vec<Seg>,
        /// Colour and opacity, 0 to 255.
        fill: Option<(Rgb, u8)>,
        stroke: Option<Pen>,
    },
    /// A rectangle filled with a linear gradient.
    Gradient {
        rect: Rect,
        /// Degrees, clockwise from left to right.
        angle: f32,
        /// Positions from 0 to 1, colour, opacity.
        stops: Vec<(f32, Rgb, u8)>,
    },
}

/// One step of an [`Item::Path`], in page points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    /// Control point, then end point.
    Quad(f32, f32, f32, f32),
    /// Two control points, then end point.
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pen {
    pub width: f32,
    pub color: Rgb,
    pub alpha: u8,
    pub dash: atrium_office::look::Dash,
}

/// One laid-out line of a paragraph, for the caret, selection and hit
/// tests. Lines are kept in document order.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub page: usize,
    pub para: NodeId,
    /// The bytes of `Para::text()` on this line.
    pub range: Range<usize>,
    /// The line's box: from the top of its tallest glyph to the bottom of
    /// its deepest, plus line spacing.
    pub top: f32,
    pub height: f32,
    pub baseline: f32,
    /// Each caret stop on the line, left to right: a byte offset into
    /// `Para::text()` and its x. The first is `range.start`, the last
    /// `range.end`.
    pub stops: Vec<(usize, f32)>,
    /// In a header or footer: shown, not edited from the page.
    pub furniture: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    /// The number printed on it.
    pub number: u32,
    pub background: Option<Rgb>,
    pub items: Vec<Item>,
}

/// A laid-out document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub pages: Vec<Page>,
    pub lines: Vec<Line>,
}

/// Lays out a whole document.
pub fn layout(doc: &WordDoc, shaper: &dyn Shaper) -> Layout {
    engine::document(doc, shaper)
}

/// Lays out the document again after the paragraphs in `changed` changed,
/// were added or were removed. The result is the same as [`layout`]; an
/// engine may reuse the pages before the first change.
pub fn relayout(
    previous: &Layout,
    doc: &WordDoc,
    changed: &[NodeId],
    shaper: &dyn Shaper,
) -> Layout {
    let _ = (previous, changed);
    engine::document(doc, shaper)
}

/// Lays out one slide as a page.
pub fn layout_slide(slide: &Slide, shaper: &dyn Shaper) -> Layout {
    engine::slide(slide, 0, shaper)
}

/// A chart drawn into `rect`, from its cached values.
pub fn chart(chart: &atrium_office::look::Chart, rect: Rect, shaper: &dyn Shaper) -> Vec<Item> {
    engine::chart(chart, rect, shaper)
}

/// Lays out every slide, one page each, hidden ones included.
pub fn layout_deck(deck: &Deck, shaper: &dyn Shaper) -> Layout {
    let mut out = Layout::default();
    for (i, slide) in deck.slides().iter().enumerate() {
        let one = engine::slide(slide, i, shaper);
        out.pages.extend(one.pages);
        out.lines.extend(one.lines);
    }
    out
}

#[cfg(test)]
mod tests;
