//! A presentation as slides of text boxes.

use crate::word::{EditError, Inline, NodeId, Para};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub id: NodeId,
    /// Position and size in points: x, y, width, height.
    pub frame: [i32; 4],
    pub paras: Vec<Para>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slide {
    pub hidden: bool,
    /// Width and height in points.
    pub size: (i32, i32),
    pub shapes: Vec<Shape>,
}

impl Slide {
    /// The slide's title: its first non-empty paragraph.
    pub fn title(&self) -> String {
        self.shapes
            .iter()
            .flat_map(|s| s.paras.iter())
            .map(|p| p.text().trim().to_string())
            .find(|t| !t.is_empty())
            .unwrap_or_default()
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
            .flat_map(|s| s.paras.iter_mut())
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
