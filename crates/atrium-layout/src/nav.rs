//! Caret positions on a laid-out document: where a position is drawn,
//! which position a click lands on, and how the arrow keys move.

use std::cmp::Ordering;

use atrium_office::NodeId;

use crate::{Layout, Line, Rect};

/// A caret position: a byte offset into a paragraph's `Para::text()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pos {
    pub para: NodeId,
    pub offset: usize,
}

impl Pos {
    pub fn new(para: NodeId, offset: usize) -> Self {
        Self { para, offset }
    }
}

/// Where to draw a caret.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub page: usize,
    pub x: f32,
    pub top: f32,
    pub height: f32,
}

impl Layout {
    /// The line that shows `pos`. An offset at a soft line break belongs to
    /// the line after it.
    pub fn line_of(&self, pos: Pos) -> Option<usize> {
        let mut last = None;
        for (i, line) in self.lines.iter().enumerate() {
            if line.para != pos.para || line.furniture {
                continue;
            }
            if pos.offset >= line.range.start && pos.offset < line.range.end {
                return Some(i);
            }
            if pos.offset == line.range.end {
                last = Some(i);
            }
        }
        last
    }

    fn x_of(line: &Line, offset: usize) -> f32 {
        line.stops
            .iter()
            .find(|(o, _)| *o >= offset)
            .or(line.stops.last())
            .map(|(_, x)| *x)
            .unwrap_or_default()
    }

    pub fn caret(&self, pos: Pos) -> Option<Caret> {
        let line = &self.lines[self.line_of(pos)?];
        Some(Caret {
            page: line.page,
            x: Self::x_of(line, pos.offset),
            top: line.top,
            height: line.height,
        })
    }

    fn nearest_stop(line: &Line, x: f32) -> Pos {
        let offset = line
            .stops
            .iter()
            .min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))
            .map(|(o, _)| *o)
            .unwrap_or(line.range.start);
        Pos::new(line.para, offset)
    }

    /// The position a click at `x`, `y` on `page` lands on: the nearest
    /// editable line, then the nearest character boundary on it.
    pub fn hit(&self, page: usize, x: f32, y: f32) -> Option<Pos> {
        let distance = |line: &Line| {
            let x_gap = match (line.stops.first(), line.stops.last()) {
                (Some(a), Some(_)) if x < a.1 => a.1 - x,
                (Some(_), Some(b)) if x > b.1 => x - b.1,
                _ => 0.0,
            };
            let y_gap = if y < line.top {
                line.top - y
            } else if y > line.top + line.height {
                y - line.top - line.height
            } else {
                0.0
            };
            // Rows matter more than columns: a click beside a line picks it.
            y_gap * 1000.0 + x_gap
        };
        self.lines
            .iter()
            .filter(|l| l.page == page && !l.furniture)
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            .map(|line| Self::nearest_stop(line, x))
    }

    /// Document order of two positions.
    pub fn compare(&self, a: Pos, b: Pos) -> Ordering {
        let line = |p: Pos| self.line_of(p).unwrap_or(usize::MAX);
        line(a).cmp(&line(b)).then_with(|| {
            if a.para == b.para {
                a.offset.cmp(&b.offset)
            } else {
                Ordering::Equal
            }
        })
    }

    /// The boxes that show the text between two positions, one per line.
    pub fn selection(&self, a: Pos, b: Pos) -> Vec<(usize, Rect)> {
        let (from, to) = if self.compare(a, b) == Ordering::Greater {
            (b, a)
        } else {
            (a, b)
        };
        let (Some(first), Some(last)) = (self.line_of(from), self.line_of(to)) else {
            return Vec::new();
        };
        (first..=last)
            .filter(|i| !self.lines[*i].furniture)
            .map(|i| {
                let line = &self.lines[i];
                let left = if i == first {
                    Self::x_of(line, from.offset)
                } else {
                    line.stops.first().map(|s| s.1).unwrap_or_default()
                };
                let mut right = if i == last {
                    Self::x_of(line, to.offset)
                } else {
                    line.stops.last().map(|s| s.1).unwrap_or_default()
                };
                // Show a selected paragraph end as a sliver.
                if i != last && right - left < 2.0 {
                    right = left + 4.0;
                }
                (
                    line.page,
                    Rect {
                        x: left,
                        y: line.top,
                        width: (right - left).max(0.0),
                        height: line.height,
                    },
                )
            })
            .collect()
    }

    fn editable(&self) -> impl Iterator<Item = (usize, &Line)> {
        self.lines.iter().enumerate().filter(|(_, l)| !l.furniture)
    }

    /// The position on the line above or below that is nearest `goal_x`.
    pub fn vertical(&self, pos: Pos, down: bool, goal_x: f32) -> Option<Pos> {
        let now = self.line_of(pos)?;
        let next = if down {
            self.editable().find(|(i, _)| *i > now)
        } else {
            self.editable().filter(|(i, _)| *i < now).last()
        }?;
        Some(Self::nearest_stop(next.1, goal_x))
    }

    pub fn line_start(&self, pos: Pos) -> Pos {
        self.line_of(pos)
            .map(|i| Pos::new(pos.para, self.lines[i].range.start))
            .unwrap_or(pos)
    }

    pub fn line_end(&self, pos: Pos) -> Pos {
        self.line_of(pos)
            .map(|i| {
                let line = &self.lines[i];
                let last_para_line = !self
                    .lines
                    .iter()
                    .skip(i + 1)
                    .any(|l| l.para == pos.para && !l.furniture);
                // Before a soft break's last character, so the caret stays on this line.
                let end = if last_para_line || line.stops.len() < 2 {
                    line.range.end
                } else {
                    line.stops[line.stops.len() - 2].0
                };
                Pos::new(pos.para, end)
            })
            .unwrap_or(pos)
    }

    /// The next or previous caret stop, crossing into the next or previous
    /// paragraph; `None` at either end of the document.
    pub fn step(&self, pos: Pos, forward: bool) -> Option<Pos> {
        let stops: Vec<usize> = self
            .lines
            .iter()
            .filter(|l| l.para == pos.para && !l.furniture)
            .flat_map(|l| l.stops.iter().map(|s| s.0))
            .collect();
        let found = if forward {
            stops.iter().copied().filter(|o| *o > pos.offset).min()
        } else {
            stops.iter().copied().filter(|o| *o < pos.offset).max()
        };
        if let Some(offset) = found {
            return Some(Pos::new(pos.para, offset));
        }
        let now = self.line_of(pos)?;
        if forward {
            let (_, line) = self
                .editable()
                .find(|(i, l)| *i > now && l.para != pos.para)?;
            Some(Pos::new(line.para, line.range.start))
        } else {
            let (_, line) = self
                .editable()
                .filter(|(i, l)| *i < now && l.para != pos.para)
                .last()?;
            let end = self
                .lines
                .iter()
                .filter(|l| l.para == line.para)
                .map(|l| l.range.end)
                .max()
                .unwrap_or(line.range.end);
            Some(Pos::new(line.para, end))
        }
    }

    /// The first and last positions of the document's editable text.
    pub fn ends(&self) -> Option<(Pos, Pos)> {
        let (_, first) = self.editable().next()?;
        let (_, last) = self.editable().last()?;
        Some((
            Pos::new(first.para, first.range.start),
            Pos::new(last.para, last.range.end),
        ))
    }

    /// The paragraphs a selection between two positions touches, in order.
    pub fn paras_between(&self, a: Pos, b: Pos) -> Vec<NodeId> {
        let (from, to) = if self.compare(a, b) == Ordering::Greater {
            (b, a)
        } else {
            (a, b)
        };
        let (Some(first), Some(last)) = (self.line_of(from), self.line_of(to)) else {
            return Vec::new();
        };
        let mut out: Vec<NodeId> = Vec::new();
        for line in &self.lines[first..=last] {
            if !line.furniture && out.last() != Some(&line.para) {
                out.push(line.para);
            }
        }
        out
    }
}
