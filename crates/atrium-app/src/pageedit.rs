//! Editing on laid-out pages, without GPUI: what typing, deleting, Enter
//! and formatting do to the document model, as ops that undo exactly.

use atrium_layout::{Layout, Pos};
use atrium_office::{
    Block, Document, Inline, Marks, NodeId, Office, Op, Para, ParaStyle, Place, SlideOp, WordOp,
};

/// Where a paragraph lives: the cells it is nested in, or its slide shape.
type Container = Vec<(NodeId, usize, usize)>;

fn search<'a>(blocks: &'a [Block], id: NodeId, path: &mut Container) -> Option<&'a Para> {
    for block in blocks {
        match block {
            Block::Paragraph(p) if p.id == id => return Some(p),
            Block::Table(t) => {
                for (r, row) in t.rows.iter().enumerate() {
                    for (c, cell) in row.iter().enumerate() {
                        path.push((t.id, r, c));
                        if let Some(p) = search(cell, id, path) {
                            return Some(p);
                        }
                        path.pop();
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The paragraph and where it lives.
pub fn find(office: &Office, id: NodeId) -> Option<(&Para, Container)> {
    match office {
        Office::Word(doc) => {
            let mut path = Vec::new();
            search(doc.blocks(), id, &mut path).map(|p| (p, path))
        }
        Office::Slides(deck) => deck.slides().iter().enumerate().find_map(|(s, slide)| {
            slide.shapes.iter().find_map(|shape| {
                shape
                    .paras
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| (p, vec![(shape.id, s, 0)]))
            })
        }),
        Office::Sheet(_) => None,
    }
}

fn width(inline: &Inline) -> usize {
    match inline {
        Inline::Text { text, .. } => text.len(),
        Inline::Break | Inline::Tab => 1,
        Inline::Atom(_) => '\u{FFFC}'.len_utf8(),
    }
}

pub fn text_len(inlines: &[Inline]) -> usize {
    inlines.iter().map(width).sum()
}

/// Joins neighbouring text with the same marks and drops empty text.
fn normalize(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for inline in inlines {
        if let Inline::Text { text, .. } = &inline
            && text.is_empty()
        {
            continue;
        }
        if let (
            Some(Inline::Text {
                text: last,
                marks: last_marks,
            }),
            Inline::Text { text, marks },
        ) = (out.last_mut(), &inline)
            && last_marks == marks
        {
            last.push_str(text);
            continue;
        }
        out.push(inline);
    }
    out
}

/// The inlines between two byte offsets of the paragraph's text.
pub fn slice(inlines: &[Inline], from: usize, to: usize) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut at = 0;
    for inline in inlines {
        let w = width(inline);
        let (start, end) = (at, at + w);
        at = end;
        if end <= from || start >= to {
            continue;
        }
        match inline {
            Inline::Text { text, marks } => {
                let a = from.saturating_sub(start).min(text.len());
                let b = (to - start).min(text.len());
                if let Some(piece) = text.get(a..b) {
                    out.push(Inline::Text {
                        text: piece.to_string(),
                        marks: marks.clone(),
                    });
                }
            }
            // Pieces kept whole are in or out.
            _ if start >= from && end <= to => out.push(inline.clone()),
            _ => {}
        }
    }
    normalize(out)
}

/// `inlines` with the stretch between two offsets replaced by `insert`.
pub fn splice(inlines: &[Inline], from: usize, to: usize, insert: Vec<Inline>) -> Vec<Inline> {
    let end = text_len(inlines);
    let mut out = slice(inlines, 0, from);
    out.extend(insert);
    out.extend(slice(inlines, to, end));
    normalize(out)
}

/// The marks text typed at `offset` takes: the character before it, or
/// after it at the start.
pub fn marks_at(inlines: &[Inline], offset: usize) -> Marks {
    let mut at = 0;
    let mut found = None;
    for inline in inlines {
        let w = width(inline);
        if let Inline::Text { marks, .. } = inline {
            if at < offset || found.is_none() {
                found = Some(marks.clone());
            }
            if at + w >= offset && at < offset {
                break;
            }
        }
        at += w;
        if at > offset {
            break;
        }
    }
    let mut marks = found.unwrap_or_default();
    // A link does not run on into new text.
    marks.link = None;
    marks
}

/// Applies `change` to the marks of the text between two offsets.
pub fn restyle(
    inlines: &[Inline],
    from: usize,
    to: usize,
    change: &dyn Fn(&mut Marks),
) -> Vec<Inline> {
    let end = text_len(inlines);
    let middle = slice(inlines, from, to)
        .into_iter()
        .map(|inline| match inline {
            Inline::Text { text, mut marks } => {
                change(&mut marks);
                Inline::Text { text, marks }
            }
            other => other,
        })
        .collect();
    splice(inlines, from, from.max(to).min(end), middle)
}

/// Whether every piece of text between two offsets passes `test`.
pub fn all_marked(
    inlines: &[Inline],
    from: usize,
    to: usize,
    test: &dyn Fn(&Marks) -> bool,
) -> bool {
    let mut any = false;
    for inline in slice(inlines, from, to) {
        if let Inline::Text { marks, .. } = inline {
            any = true;
            if !test(&marks) {
                return false;
            }
        }
    }
    any
}

/// Typed text as paragraphs of inlines: lines split at newlines, tabs kept.
pub fn pieces(text: &str, marks: &Marks) -> Vec<Vec<Inline>> {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| {
            let mut out = Vec::new();
            for (k, part) in line.split('\t').enumerate() {
                if k > 0 {
                    out.push(Inline::Tab);
                }
                if !part.is_empty() {
                    out.push(Inline::Text {
                        text: part.to_string(),
                        marks: marks.clone(),
                    });
                }
            }
            out
        })
        .collect()
}

/// A run of ops applied together: all land, or none does.
pub struct Edit<'a> {
    doc: &'a mut Document,
    inverses: Vec<Op>,
}

impl<'a> Edit<'a> {
    pub fn new(doc: &'a mut Document) -> Self {
        Self {
            doc,
            inverses: Vec::new(),
        }
    }

    pub fn office(&self) -> &Office {
        &self.doc.office
    }

    pub fn apply(&mut self, op: Op) -> Result<Op, String> {
        match self.doc.apply(op) {
            Ok(inverse) => {
                self.inverses.push(inverse.clone());
                Ok(inverse)
            }
            Err(e) => {
                self.rollback();
                Err(e.to_string())
            }
        }
    }

    fn rollback(&mut self) {
        while let Some(op) = self.inverses.pop() {
            let _ = self.doc.apply(op);
        }
    }

    /// The ops that undo the edit, in the order to apply them.
    pub fn finish(mut self) -> Vec<Op> {
        let mut out = std::mem::take(&mut self.inverses);
        out.reverse();
        out
    }

    /// Replaces a paragraph's content, for Word or a slide.
    pub fn set_content(&mut self, para: NodeId, inlines: Vec<Inline>) -> Result<(), String> {
        let op = match self.doc.office {
            Office::Slides(_) => Op::Slides(SlideOp::SetContent { para, inlines }),
            _ => Op::Word(WordOp::SetContent { para, inlines }),
        };
        self.apply(op).map(|_| ())
    }
}

/// Applies `ops` in order, all or none, and returns the ops that undo them.
pub fn apply_all(doc: &mut Document, ops: Vec<Op>) -> Result<Vec<Op>, String> {
    let mut edit = Edit::new(doc);
    for op in ops {
        edit.apply(op)?;
    }
    Ok(edit.finish())
}

fn refused(text: &'static str) -> String {
    crate::i18n::t(text).to_string()
}

/// Replaces the text between two positions with `insert`: one entry per
/// paragraph, so more than one entry splits the paragraph. Returns where
/// the caret goes and the ops that undo it.
pub fn replace(
    doc: &mut Document,
    layout: &Layout,
    a: Pos,
    b: Pos,
    insert: Vec<Vec<Inline>>,
) -> Result<(Pos, Vec<Op>), String> {
    let (from, to) = if layout.compare(a, b) == std::cmp::Ordering::Greater {
        (b, a)
    } else {
        (a, b)
    };
    let paras = if from.para == to.para {
        vec![from.para]
    } else {
        layout.paras_between(from, to)
    };
    let slides = matches!(doc.office, Office::Slides(_));
    let (first, home) = find(&doc.office, from.para)
        .map(|(p, c)| (p.clone(), c))
        .ok_or_else(|| refused("That part of the document is gone."))?;
    let last = find(&doc.office, to.para)
        .map(|(p, _)| p.clone())
        .ok_or_else(|| refused("That part of the document is gone."))?;
    for id in &paras {
        let Some((_, there)) = find(&doc.office, *id) else {
            return Err(refused("That part of the document is gone."));
        };
        if there != home {
            return Err(refused(
                "Select within one table cell or text box to change text there.",
            ));
        }
    }
    // A slide's text box takes no new paragraphs here: lines break instead.
    let insert = if slides && insert.len() > 1 {
        let mut joined = Vec::new();
        for (k, piece) in insert.into_iter().enumerate() {
            if k > 0 {
                joined.push(Inline::Break);
            }
            joined.extend(piece);
        }
        vec![joined]
    } else {
        insert
    };
    if slides && paras.len() > 1 {
        return Err(refused(
            "On slides, select within one paragraph to change it.",
        ));
    }

    let prefix = slice(&first.inlines, 0, from.offset);
    let suffix = slice(&last.inlines, to.offset, text_len(&last.inlines));
    let mut edit = Edit::new(doc);
    let count = insert.len();
    let mut caret = from;
    for id in paras.iter().skip(1) {
        edit.apply(Op::Word(WordOp::DeletePara { para: *id }))?;
    }
    let mut previous = first.id;
    for (k, piece) in insert.into_iter().enumerate() {
        let piece_len = text_len(&piece);
        let mut inlines = if k == 0 { prefix.clone() } else { Vec::new() };
        let start = if k == 0 { from.offset } else { 0 };
        inlines.extend(piece);
        if k + 1 == count {
            inlines.extend(suffix.clone());
        }
        let inlines = normalize(inlines);
        if k == 0 {
            edit.set_content(first.id, inlines)?;
        } else {
            // A new paragraph after a heading is body text; a list item stays a list item.
            let style = match &first.style {
                ParaStyle::Title | ParaStyle::Heading(_) if k + 1 == count && suffix.is_empty() => {
                    ParaStyle::Normal
                }
                other => other.clone(),
            };
            let inverse = edit.apply(Op::Word(WordOp::InsertPara {
                at: Place::After(previous),
                id: None,
                style,
                inlines,
            }))?;
            if let Op::Word(WordOp::DeletePara { para }) = inverse {
                previous = para;
            }
        }
        caret = Pos::new(previous, start + piece_len);
    }
    Ok((caret, edit.finish()))
}

/// The plain text between two positions, paragraphs on their own lines.
pub fn text_between(office: &Office, layout: &Layout, a: Pos, b: Pos) -> String {
    let (from, to) = if layout.compare(a, b) == std::cmp::Ordering::Greater {
        (b, a)
    } else {
        (a, b)
    };
    let paras = if from.para == to.para {
        vec![from.para]
    } else {
        layout.paras_between(from, to)
    };
    let mut out = Vec::new();
    for id in paras {
        let Some((p, _)) = find(office, id) else {
            continue;
        };
        let text = p.text();
        let start = if id == from.para { from.offset } else { 0 };
        let end = if id == to.para { to.offset } else { text.len() };
        out.push(
            text.get(start.min(end)..end.min(text.len()))
                .unwrap_or_default()
                .replace('\u{FFFC}', ""),
        );
    }
    out.join("\n")
}

/// Changes the marks of everything between two positions; returns the
/// ops that undo it.
pub fn restyle_between(
    doc: &mut Document,
    layout: &Layout,
    a: Pos,
    b: Pos,
    change: &dyn Fn(&mut Marks),
) -> Result<Vec<Op>, String> {
    let (from, to) = if layout.compare(a, b) == std::cmp::Ordering::Greater {
        (b, a)
    } else {
        (a, b)
    };
    let paras = if from.para == to.para {
        vec![from.para]
    } else {
        layout.paras_between(from, to)
    };
    let mut edit = Edit::new(doc);
    for id in paras {
        let Some((p, _)) = find(edit.office(), id) else {
            continue;
        };
        let len = text_len(&p.inlines);
        let start = if id == from.para { from.offset } else { 0 };
        let end = if id == to.para { to.offset } else { len };
        if start >= end {
            continue;
        }
        let inlines = restyle(&p.inlines, start, end, change);
        if inlines != p.inlines {
            edit.set_content(id, inlines)?;
        }
    }
    Ok(edit.finish())
}

/// Whether everything between two positions passes `test`.
pub fn all_between(
    office: &Office,
    layout: &Layout,
    a: Pos,
    b: Pos,
    test: &dyn Fn(&Marks) -> bool,
) -> bool {
    let (from, to) = if layout.compare(a, b) == std::cmp::Ordering::Greater {
        (b, a)
    } else {
        (a, b)
    };
    let paras = if from.para == to.para {
        vec![from.para]
    } else {
        layout.paras_between(from, to)
    };
    let mut any = false;
    for id in paras {
        let Some((p, _)) = find(office, id) else {
            continue;
        };
        let len = text_len(&p.inlines);
        let start = if id == from.para { from.offset } else { 0 };
        let end = if id == to.para { to.offset } else { len };
        if start >= end {
            continue;
        }
        any = true;
        if !all_marked(&p.inlines, start, end, test) {
            return false;
        }
    }
    any
}

pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// A byte offset as UTF-16 units, for the input method.
pub fn to_utf16(text: &str, offset: usize) -> usize {
    utf16_len(text.get(..offset.min(text.len())).unwrap_or(text))
}

/// UTF-16 units as a byte offset, on a character boundary.
pub fn from_utf16(text: &str, units: usize) -> usize {
    let mut count = 0;
    for (i, c) in text.char_indices() {
        if count >= units {
            return i;
        }
        count += c.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_layout::{FixedShaper, layout};
    use atrium_office::WordDoc;

    fn bold() -> Marks {
        Marks {
            bold: true,
            ..Default::default()
        }
    }

    fn text(s: &str, marks: Marks) -> Inline {
        Inline::Text {
            text: s.into(),
            marks,
        }
    }

    #[test]
    fn splice_and_slice_keep_marks() {
        let inlines = vec![text("甲乙", Marks::default()), text("丙丁", bold())];
        assert_eq!(
            slice(&inlines, 3, 9),
            vec![text("乙", Marks::default()), text("丙", bold())]
        );
        let out = splice(&inlines, 6, 6, vec![text("X", bold())]);
        assert_eq!(
            out,
            vec![text("甲乙", Marks::default()), text("X丙丁", bold())]
        );
        assert_eq!(marks_at(&inlines, 6), Marks::default());
        assert_eq!(marks_at(&inlines, 9), bold());
        assert_eq!(marks_at(&inlines, 0), Marks::default());
        let restyled = restyle(&inlines, 0, 3, &|m| m.italic = true);
        assert!(matches!(&restyled[0], Inline::Text { marks, .. } if marks.italic));
        assert!(all_marked(&inlines, 6, 12, &|m| m.bold));
        assert!(!all_marked(&inlines, 3, 12, &|m| m.bold));
    }

    fn doc(paras: &[(u64, &str)]) -> Document {
        let blocks = paras
            .iter()
            .map(|(id, t)| Block::Paragraph(Para::new(*id, ParaStyle::Normal, t)))
            .collect();
        Document::new(
            "/tmp/x.docx".into(),
            Office::Word(WordDoc::from_blocks(blocks)),
            String::new(),
        )
    }

    fn texts(doc: &Document) -> Vec<String> {
        let Office::Word(w) = &doc.office else {
            return vec![];
        };
        w.blocks()
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p.text()),
                _ => None,
            })
            .collect()
    }

    fn laid(doc: &Document) -> Layout {
        let Office::Word(w) = &doc.office else {
            unreachable!()
        };
        layout(w, &FixedShaper)
    }

    #[test]
    fn typing_enter_and_merging_undo_exactly() {
        let mut d = doc(&[(1, "一二三"), (2, "四五")]);
        let l = laid(&d);
        // Type in the middle.
        let (caret, undo1) = replace(
            &mut d,
            &l,
            Pos::new(1, 3),
            Pos::new(1, 3),
            pieces("X", &Marks::default()),
        )
        .unwrap();
        assert_eq!(texts(&d), ["一X二三", "四五"]);
        assert_eq!(caret, Pos::new(1, 4));
        // Enter splits the paragraph.
        let l = laid(&d);
        let (caret, undo2) =
            replace(&mut d, &l, caret, caret, pieces("\n", &Marks::default())).unwrap();
        assert_eq!(texts(&d), ["一X", "二三", "四五"]);
        assert_eq!(caret.offset, 0);
        assert_ne!(caret.para, 1);
        // Deleting across the break joins them again.
        let l = laid(&d);
        let (_, undo3) = replace(&mut d, &l, Pos::new(1, 4), caret, vec![vec![]]).unwrap();
        assert_eq!(texts(&d), ["一X二三", "四五"]);
        // Undo in reverse order restores each step.
        apply_all(&mut d, undo3).unwrap();
        assert_eq!(texts(&d), ["一X", "二三", "四五"]);
        apply_all(&mut d, undo2).unwrap();
        apply_all(&mut d, undo1).unwrap();
        assert_eq!(texts(&d), ["一二三", "四五"]);
    }

    #[test]
    fn a_selection_across_paragraphs_is_replaced() {
        let mut d = doc(&[(1, "一二三"), (2, "四五"), (3, "六七")]);
        let l = laid(&d);
        replace(
            &mut d,
            &l,
            Pos::new(3, 3),
            Pos::new(1, 3),
            pieces("Z", &Marks::default()),
        )
        .unwrap();
        assert_eq!(texts(&d), ["一Z七"]);
        let l = laid(&d);
        assert_eq!(
            text_between(&d.office, &l, Pos::new(1, 0), Pos::new(1, 4)),
            "一Z"
        );
    }

    #[test]
    fn bold_toggles_over_a_selection() {
        let mut d = doc(&[(1, "一二三")]);
        let l = laid(&d);
        restyle_between(&mut d, &l, Pos::new(1, 0), Pos::new(1, 6), &|m| {
            m.bold = true
        })
        .unwrap();
        assert!(all_between(
            &d.office,
            &l,
            Pos::new(1, 0),
            Pos::new(1, 6),
            &|m| m.bold
        ));
        assert!(!all_between(
            &d.office,
            &l,
            Pos::new(1, 0),
            Pos::new(1, 9),
            &|m| m.bold
        ));
    }

    #[test]
    fn utf16_offsets_round_trip() {
        let s = "a中𝄞b";
        assert_eq!(to_utf16(s, 4), 2);
        assert_eq!(to_utf16(s, 8), 4);
        assert_eq!(from_utf16(s, 4), 8);
        assert_eq!(from_utf16(s, 99), s.len());
    }
}
