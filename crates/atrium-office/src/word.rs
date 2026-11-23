//! A Word document as paragraphs, tables and pieces kept whole.

use std::path::PathBuf;

/// A node's identity: stable for the life of an open document, so edits,
/// change reports and the file binding agree on which paragraph is which.
pub type NodeId = u64;

/// Ids for nodes made by edits, above any a loader hands out.
pub(crate) const FRESH: NodeId = 1 << 40;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub link: Option<String>,
    /// The effective fonts; empty names take the document's default.
    pub fonts: crate::look::Fonts,
    /// The effective size; `None` takes the document's default.
    pub size: Option<crate::look::Twips>,
    pub color: Option<crate::look::Rgb>,
    pub highlight: Option<crate::look::Rgb>,
    pub baseline: crate::look::Baseline,
}

/// What a paragraph holds, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text {
        text: String,
        marks: Marks,
    },
    Break,
    Tab,
    /// An image, a field, a footnote reference…: one character wide and kept whole.
    Atom(Atom),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Atom {
    pub id: NodeId,
    pub kind: AtomKind,
    pub look: crate::look::AtomLook,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtomKind {
    /// With the picture extracted to a file the UI can show, when there is one.
    Image(Option<PathBuf>),
    /// The field's instruction, such as `PAGE` or `TOC \o "1-3"`; what it shows is in `Atom::look.shown`.
    Field(String),
    Footnote,
    Other(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ParaStyle {
    #[default]
    Normal,
    Title,
    /// 1 to 6.
    Heading(u8),
    ListItem {
        level: u8,
        ordered: bool,
    },
    Quote,
    /// A style this model shows as normal text and keeps as it is.
    Other(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Para {
    pub id: NodeId,
    pub style: ParaStyle,
    pub align: Align,
    pub inlines: Vec<Inline>,
    /// Holds tracked changes, comments, complex fields, content controls or
    /// math: shown, but edited only in Word.
    pub locked: bool,
    pub look: crate::look::ParaLook,
}

impl Para {
    pub fn new(id: NodeId, style: ParaStyle, text: &str) -> Self {
        Self {
            id,
            style,
            align: Align::Left,
            inlines: if text.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Text {
                    text: text.into(),
                    marks: Marks::default(),
                }]
            },
            locked: false,
            look: Default::default(),
        }
    }

    /// The paragraph's text, with atoms as U+FFFC and tabs and breaks as themselves.
    pub fn text(&self) -> String {
        inline_text(&self.inlines)
    }
}

pub fn inline_text(inlines: &[Inline]) -> String {
    inlines
        .iter()
        .map(|i| match i {
            Inline::Text { text, .. } => text.as_str(),
            Inline::Break => "\n",
            Inline::Tab => "\t",
            Inline::Atom(_) => "\u{FFFC}",
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub id: NodeId,
    /// Rows of cells; each cell holds blocks, usually one paragraph.
    pub rows: Vec<Vec<Vec<Block>>>,
    /// Cells, as row and column, folded into the cell on their left.
    pub merged: Vec<(usize, usize)>,
    pub look: crate::look::TableLook,
}

/// A footnote's text, referred to by an atom with the same `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Footnote {
    pub id: NodeId,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(Para),
    Table(Table),
    /// A section break, a content control, anything else: shown by its label, kept whole.
    Opaque {
        id: NodeId,
        label: String,
    },
}

impl Block {
    pub fn id(&self) -> NodeId {
        match self {
            Block::Paragraph(p) => p.id,
            Block::Table(t) => t.id,
            Block::Opaque { id, .. } => *id,
        }
    }
}

/// One edit. Applying it returns its exact inverse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WordOp {
    /// Replaces what a paragraph holds. The file binding maps the change onto
    /// the paragraph's runs, keeping the formatting of text that did not change.
    SetContent {
        para: NodeId,
        inlines: Vec<Inline>,
    },
    /// A new paragraph at `at`. `id` restores a deleted paragraph; a fresh
    /// one gets a new id.
    InsertPara {
        at: Place,
        id: Option<NodeId>,
        style: ParaStyle,
        inlines: Vec<Inline>,
    },
    DeletePara {
        para: NodeId,
    },
    SetStyle {
        para: NodeId,
        style: ParaStyle,
    },
    SetAlign {
        para: NodeId,
        align: Align,
    },
    /// Replaces how a paragraph is laid out: spacing, indents, tabs, list…
    SetLook {
        para: NodeId,
        look: Box<crate::look::ParaLook>,
    },
}

/// Where a new paragraph goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Right after this block, in the same body or cell.
    After(NodeId),
    /// First in the body.
    BodyStart,
    /// First in a table's cell.
    CellStart {
        table: NodeId,
        row: usize,
        col: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The paragraph holds things only Word edits.
    Locked,
    /// No such paragraph, row or cell.
    Missing,
    Invalid(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::Locked => f.write_str("this part can only be edited in the original app"),
            EditError::Missing => f.write_str("that part of the document is gone"),
            EditError::Invalid(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for EditError {}

#[derive(Clone, Debug, Default)]
pub struct WordDoc {
    pub(crate) blocks: Vec<Block>,
    next: NodeId,
    /// One per section, in order; empty means one A4 section.
    pub(crate) sections: Vec<crate::look::Section>,
    /// How text looks where nothing else says: `docDefaults` and the Normal style.
    pub(crate) defaults: Marks,
    /// Word's `defaultTabStop`, in twips.
    pub(crate) default_tab_stop: crate::look::Twips,
    /// Footnote text, by the id of the atom that refers to it.
    pub(crate) footnotes: Vec<Footnote>,
}

/// Where a paragraph sits: its container's blocks and its index there.
fn find_mut(blocks: &mut Vec<Block>, id: NodeId) -> Option<(&mut Vec<Block>, usize)> {
    if let Some(at) = blocks.iter().position(|b| b.id() == id) {
        return Some((blocks, at));
    }
    for block in blocks.iter_mut() {
        if let Block::Table(table) = block {
            for row in table.rows.iter_mut() {
                for cell in row.iter_mut() {
                    if let Some(found) = find_mut(cell, id) {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

impl WordDoc {
    /// A document of these blocks. Loaders build documents this way; so do tests.
    pub fn from_blocks(blocks: Vec<Block>) -> Self {
        Self {
            blocks,
            next: FRESH,
            sections: Vec::new(),
            defaults: Marks::default(),
            default_tab_stop: 420,
            footnotes: Vec::new(),
        }
    }

    /// Sets the sections and the default run look. Loaders call this after `from_blocks`.
    pub fn with_look(mut self, sections: Vec<crate::look::Section>, defaults: Marks) -> Self {
        self.sections = sections;
        self.defaults = defaults;
        self
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// The sections in order, at least one.
    pub fn sections(&self) -> std::borrow::Cow<'_, [crate::look::Section]> {
        if self.sections.is_empty() {
            std::borrow::Cow::Owned(vec![crate::look::Section {
                new_page: true,
                ..Default::default()
            }])
        } else {
            std::borrow::Cow::Borrowed(&self.sections)
        }
    }

    /// How text looks where nothing else says.
    pub fn defaults(&self) -> &Marks {
        &self.defaults
    }

    /// Word's `defaultTabStop`: the distance between automatic tab stops.
    pub fn default_tab_stop(&self) -> crate::look::Twips {
        if self.default_tab_stop > 0 {
            self.default_tab_stop
        } else {
            420
        }
    }

    /// Sets the document's automatic tab stop width.
    pub fn with_default_tab_stop(mut self, twips: crate::look::Twips) -> Self {
        self.default_tab_stop = twips;
        self
    }

    /// The document's footnotes.
    pub fn footnotes(&self) -> &[Footnote] {
        &self.footnotes
    }

    /// Sets the document's footnotes.
    pub fn with_footnotes(mut self, footnotes: Vec<Footnote>) -> Self {
        self.footnotes = footnotes;
        self
    }

    fn para_mut(&mut self, id: NodeId) -> Result<&mut Para, EditError> {
        let (container, at) = find_mut(&mut self.blocks, id).ok_or(EditError::Missing)?;
        match &mut container[at] {
            Block::Paragraph(p) if p.locked => Err(EditError::Locked),
            Block::Paragraph(p) => Ok(p),
            _ => Err(EditError::Invalid("not a paragraph".into())),
        }
    }

    /// The start of the body or cell that holds `para`.
    fn start_of(&self, para: NodeId) -> Place {
        fn search(blocks: &[Block], para: NodeId) -> Option<Place> {
            for b in blocks {
                if let Block::Table(t) = b {
                    for (r, row) in t.rows.iter().enumerate() {
                        for (c, cell) in row.iter().enumerate() {
                            if cell.iter().any(|x| x.id() == para) {
                                return Some(Place::CellStart {
                                    table: t.id,
                                    row: r,
                                    col: c,
                                });
                            }
                            if let Some(found) = search(cell, para) {
                                return Some(found);
                            }
                        }
                    }
                }
            }
            None
        }
        search(&self.blocks, para).unwrap_or(Place::BodyStart)
    }

    pub fn apply(&mut self, op: WordOp) -> Result<WordOp, EditError> {
        Ok(match op {
            WordOp::SetContent { para, inlines } => {
                let p = self.para_mut(para)?;
                WordOp::SetContent {
                    para,
                    inlines: std::mem::replace(&mut p.inlines, inlines),
                }
            }
            WordOp::SetStyle { para, style } => {
                let p = self.para_mut(para)?;
                WordOp::SetStyle {
                    para,
                    style: std::mem::replace(&mut p.style, style),
                }
            }
            WordOp::SetAlign { para, align } => {
                let p = self.para_mut(para)?;
                WordOp::SetAlign {
                    para,
                    align: std::mem::replace(&mut p.align, align),
                }
            }
            WordOp::SetLook { para, look } => {
                let p = self.para_mut(para)?;
                WordOp::SetLook {
                    para,
                    look: Box::new(std::mem::replace(&mut p.look, *look)),
                }
            }
            WordOp::InsertPara {
                at,
                id,
                style,
                inlines,
            } => {
                let id = id.unwrap_or_else(|| {
                    self.next += 1;
                    self.next
                });
                let mut para = Para::new(id, style, "");
                para.inlines = inlines;
                match at {
                    Place::BodyStart => self.blocks.insert(0, Block::Paragraph(para)),
                    Place::After(after) => {
                        let (container, i) =
                            find_mut(&mut self.blocks, after).ok_or(EditError::Missing)?;
                        container.insert(i + 1, Block::Paragraph(para));
                    }
                    Place::CellStart { table, row, col } => {
                        let (container, i) =
                            find_mut(&mut self.blocks, table).ok_or(EditError::Missing)?;
                        let Block::Table(t) = &mut container[i] else {
                            return Err(EditError::Invalid("not a table".into()));
                        };
                        let cell = t
                            .rows
                            .get_mut(row)
                            .and_then(|r| r.get_mut(col))
                            .ok_or(EditError::Missing)?;
                        cell.insert(0, Block::Paragraph(para));
                    }
                }
                WordOp::DeletePara { para: id }
            }
            WordOp::DeletePara { para } => {
                let (container, at) = find_mut(&mut self.blocks, para).ok_or(EditError::Missing)?;
                match &container[at] {
                    Block::Paragraph(p) if p.locked => return Err(EditError::Locked),
                    Block::Paragraph(_) => {}
                    _ => return Err(EditError::Invalid("not a paragraph".into())),
                }
                let place = match at.checked_sub(1) {
                    Some(i) => Place::After(container[i].id()),
                    None => self.start_of(para),
                };
                let (container, at) = find_mut(&mut self.blocks, para).ok_or(EditError::Missing)?;
                let Block::Paragraph(p) = container.remove(at) else {
                    unreachable!()
                };
                WordOp::InsertPara {
                    at: place,
                    id: Some(p.id),
                    style: p.style,
                    inlines: p.inlines,
                }
            }
        })
    }

    /// The document as Markdown, as an agent is given it. Atoms show as
    /// `label(kind)`, so a caller can put them in the UI's language.
    pub fn to_markdown(&self, label: &dyn Fn(&AtomKind) -> String) -> String {
        let mut out = String::new();
        write_blocks(&mut out, &self.blocks, label);
        out.trim_end().to_string() + "\n"
    }
}

pub(crate) fn marked(inlines: &[Inline], label: &dyn Fn(&AtomKind) -> String) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text { text, marks } => {
                let mut t = text.clone();
                if marks.bold {
                    t = format!("**{t}**");
                }
                if marks.italic {
                    t = format!("*{t}*");
                }
                if marks.strike {
                    t = format!("~~{t}~~");
                }
                if let Some(url) = &marks.link {
                    t = format!("[{t}]({url})");
                }
                s.push_str(&t);
            }
            Inline::Break => s.push_str("  \n"),
            Inline::Tab => s.push('\t'),
            Inline::Atom(a) => s.push_str(&format!("[{}]", label(&a.kind))),
        }
    }
    s
}

fn write_blocks(out: &mut String, blocks: &[Block], label: &dyn Fn(&AtomKind) -> String) {
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                let text = marked(&p.inlines, label);
                let line = match &p.style {
                    ParaStyle::Title => format!("# {text}"),
                    ParaStyle::Heading(n) => {
                        format!("{} {text}", "#".repeat((*n).clamp(1, 6) as usize))
                    }
                    ParaStyle::ListItem { level, ordered } => format!(
                        "{}{} {text}",
                        "  ".repeat(*level as usize),
                        if *ordered { "1." } else { "-" }
                    ),
                    ParaStyle::Quote => format!("> {text}"),
                    _ => text,
                };
                out.push_str(&line);
                out.push_str("\n\n");
            }
            Block::Table(t) => {
                for (r, row) in t.rows.iter().enumerate() {
                    let cells: Vec<String> = row
                        .iter()
                        .map(|cell| {
                            let mut s = String::new();
                            write_blocks(&mut s, cell, label);
                            s.trim().replace('\n', " ").replace('|', "\\|")
                        })
                        .collect();
                    out.push_str(&format!("| {} |\n", cells.join(" | ")));
                    if r == 0 {
                        out.push_str(&format!("|{}\n", " --- |".repeat(row.len())));
                    }
                }
                out.push('\n');
            }
            Block::Opaque { label: l, .. } => {
                out.push_str(&format!("[{l}]\n\n"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> WordDoc {
        WordDoc::from_blocks(vec![
            Block::Paragraph(Para::new(1, ParaStyle::Heading(1), "周报")),
            Block::Paragraph(Para::new(2, ParaStyle::Normal, "本周完成了登录页。")),
            Block::Table(Table {
                id: 3,
                rows: vec![vec![
                    vec![Block::Paragraph(Para::new(4, ParaStyle::Normal, "预算"))],
                    vec![Block::Paragraph(Para::new(5, ParaStyle::Normal, "50 万"))],
                ]],
                merged: vec![],
                look: Default::default(),
            }),
        ])
    }

    #[test]
    fn every_edit_undoes_exactly() {
        let mut d = doc();
        let before = d.blocks.clone();
        let ops = vec![
            WordOp::SetContent {
                para: 2,
                inlines: vec![Inline::Text {
                    text: "本周完成了登录页和注册页。".into(),
                    marks: Marks::default(),
                }],
            },
            WordOp::InsertPara {
                at: Place::After(2),
                id: None,
                style: ParaStyle::Normal,
                inlines: vec![],
            },
            WordOp::SetStyle {
                para: 1,
                style: ParaStyle::Title,
            },
            WordOp::DeletePara { para: 5 },
        ];
        let mut undo = Vec::new();
        for op in ops {
            undo.push(d.apply(op).unwrap());
        }
        while let Some(op) = undo.pop() {
            d.apply(op).unwrap();
        }
        assert_eq!(d.blocks, before);
    }

    #[test]
    fn locked_paragraphs_refuse_edits() {
        let mut p = Para::new(9, ParaStyle::Normal, "有修订");
        p.locked = true;
        let mut d = WordDoc::from_blocks(vec![Block::Paragraph(p)]);
        assert_eq!(
            d.apply(WordOp::DeletePara { para: 9 }),
            Err(EditError::Locked)
        );
    }

    #[test]
    fn markdown_keeps_structure() {
        let md = doc().to_markdown(&|_| "图片".into());
        assert!(md.starts_with("# 周报\n\n本周完成了登录页。\n\n| 预算 | 50 万 |\n| --- | --- |"));
    }
}
