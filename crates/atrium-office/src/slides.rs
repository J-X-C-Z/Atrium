//! A presentation as slides of text boxes.

use crate::word::{EditError, Inline, NodeId, Para};

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub id: NodeId,
    /// Position and size in points: x, y, width, height.
    pub frame: [i32; 4],
    pub paras: Vec<Para>,
    pub look: crate::look::ShapeLook,
    pub kind: crate::look::ShapeKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Slide {
    pub hidden: bool,
    /// Width and height in points.
    pub size: (i32, i32),
    /// Drawn in order, the first at the back.
    pub shapes: Vec<Shape>,
    pub background: Option<crate::look::Rgb>,
}

impl Slide {
    /// The slide's title: its first non-empty paragraph.
    pub fn title(&self) -> String {
        self.shapes
            .iter()
            .flat_map(Shape::text_paras)
            .map(|p| p.text().trim().to_string())
            .find(|t| !t.is_empty())
            .unwrap_or_default()
    }
}

impl Shape {
    /// Every paragraph the shape shows or edits, including table cells.
    pub fn text_paras(&self) -> Box<dyn Iterator<Item = &Para> + '_> {
        let own = self.paras.iter();
        match &self.kind {
            crate::look::ShapeKind::Table(table) => Box::new(
                own.chain(
                    table
                        .rows
                        .iter()
                        .flatten()
                        .flatten()
                        .filter_map(|b| match b {
                            crate::word::Block::Paragraph(p) => Some(p),
                            _ => None,
                        }),
                ),
            ),
            _ => Box::new(own),
        }
    }

    /// The same paragraphs, for edits.
    pub fn text_paras_mut(&mut self) -> Box<dyn Iterator<Item = &mut Para> + '_> {
        let own = self.paras.iter_mut();
        match &mut self.kind {
            crate::look::ShapeKind::Table(table) => Box::new(
                own.chain(
                    table
                        .rows
                        .iter_mut()
                        .flatten()
                        .flatten()
                        .filter_map(|b| match b {
                            crate::word::Block::Paragraph(p) => Some(p),
                            _ => None,
                        }),
                ),
            ),
            _ => Box::new(own),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlideOp {
    /// Replaces what a paragraph in a text box holds.
    SetContent { para: NodeId, inlines: Vec<Inline> },
}

#[derive(Clone, Debug, Default)]
pub struct Deck {
    pub(crate) slides: Vec<Slide>,
}

impl Deck {
    pub fn from_slides(slides: Vec<Slide>) -> Self {
        Self { slides }
    }

    pub fn slides(&self) -> &[Slide] {
        &self.slides
    }

    pub fn apply(&mut self, op: SlideOp) -> Result<SlideOp, EditError> {
        let SlideOp::SetContent { para, inlines } = op;
        let p = self
            .slides
            .iter_mut()
            .flat_map(|s| s.shapes.iter_mut())
            .flat_map(Shape::text_paras_mut)
            .find(|p| p.id == para)
            .ok_or(EditError::Missing)?;
        if p.locked {
            return Err(EditError::Locked);
        }
        Ok(SlideOp::SetContent {
            para,
            inlines: std::mem::replace(&mut p.inlines, inlines),
        })
    }
}
