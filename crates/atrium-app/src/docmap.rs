//! Between an office document's model and the block editor: blocks to show,
//! and the editor's changes back as edits to the model.
//!
//! Inline formatting travels as Markdown inside a block's text: `**bold**`,
//! `*italic*`, `~~strike~~`, `<u>underline</u>` and `[text](url)`. Pieces
//! kept whole (an image, a field) show as `⟦label·n⟧`; deleting the token
//! deletes the piece, and anything else typed over it is plain text.

use std::collections::HashMap;

use crate::i18n::t;
use atrium_office::{Atom, AtomKind, Block, Inline, Marks, NodeId, Para, ParaStyle, Place, WordOp};
use ely_gpui_component::documents::{BlockData, BlockKind};

/// What a block in the editor stands for in the model.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Para {
        id: NodeId,
        locked: bool,
        atoms: Vec<Atom>,
        style: ParaStyle,
    },
    /// A table whose cells are one paragraph each, row by row.
    Table { cells: Vec<NodeId> },
    /// Shown, never edited: a table Word lays out in ways this view cannot,
    /// a section break, a slide's heading.
    Fixed,
}

/// Why the editor's change cannot become edits; the editor is put back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Locked,
    TableShape,
    Fixed,
}

pub fn atom_label(kind: &AtomKind) -> String {
    use crate::i18n::t;
    match kind {
        AtomKind::Image(_) => t("image").to_string(),
        AtomKind::Field(name) if !name.is_empty() => name.clone(),
        AtomKind::Field(_) => t("field").to_string(),
        AtomKind::Footnote => t("footnote").to_string(),
        AtomKind::Other(name) => name.clone(),
    }
}

/// A paragraph's content as the editor's text.
pub fn inlines_to_text(
    inlines: &[Inline],
    label: &dyn Fn(&AtomKind) -> String,
) -> (String, Vec<Atom>) {
    let mut s = String::new();
    let mut atoms = Vec::new();
    for i in inlines {
        match i {
            Inline::Text { text, marks } => {
                let mut t = text.clone();
                if marks.underline {
                    t = format!("<u>{t}</u>");
                }
                if marks.strike {
                    t = format!("~~{t}~~");
                }
                if marks.italic {
                    t = format!("*{t}*");
                }
                if marks.bold {
                    t = format!("**{t}**");
                }
                if let Some(url) = &marks.link {
                    t = format!("[{t}]({url})");
                }
                s.push_str(&t);
            }
            Inline::Break => s.push('\n'),
            Inline::Tab => s.push('\t'),
            Inline::Atom(a) => {
                atoms.push(a.clone());
                s.push_str(&format!("⟦{}·{}⟧", label(&a.kind), atoms.len()));
            }
        }
    }
    (s, atoms)
}

fn push_text(out: &mut Vec<Inline>, text: &str, marks: &Marks) {
    if text.is_empty() {
        return;
    }
    for (i, piece) in text.split('\n').enumerate() {
        if i > 0 {
            out.push(Inline::Break);
        }
        for (j, part) in piece.split('\t').enumerate() {
            if j > 0 {
                out.push(Inline::Tab);
            }
            if part.is_empty() {
                continue;
            }
            if let Some(Inline::Text {
                text: last,
                marks: m,
            }) = out.last_mut()
                && m == marks
            {
                last.push_str(part);
                continue;
            }
            out.push(Inline::Text {
                text: part.into(),
                marks: marks.clone(),
            });
        }
    }
}

/// The editor's text back as inlines; `atoms` are the pieces its tokens name.
pub fn text_to_inlines(text: &str, atoms: &[Atom]) -> Vec<Inline> {
    let mut out = Vec::new();
    parse(text, &Marks::default(), atoms, &mut out);
    out
}

/// An opening marker, its closing one, and the mark it sets.
type Wrap = (&'static str, &'static str, fn(&mut Marks));

fn parse(text: &str, marks: &Marks, atoms: &[Atom], out: &mut Vec<Inline>) {
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        // Pairs that wrap text in a mark, longest opener first.
        let wraps: [Wrap; 4] = [
            ("**", "**", |m| m.bold = true),
            ("~~", "~~", |m| m.strike = true),
            ("<u>", "</u>", |m| m.underline = true),
            ("*", "*", |m| m.italic = true),
        ];
        let mut matched = false;
        for (open, close, set) in wraps {
            if let Some(after) = rest.strip_prefix(open)
                && let Some(end) = after.find(close)
                && end > 0
                && !after[..end].starts_with(char::is_whitespace)
                && !after[..end].ends_with(char::is_whitespace)
            {
                push_text(out, &plain, marks);
                plain.clear();
                let mut inner = marks.clone();
                set(&mut inner);
                parse(&after[..end], &inner, atoms, out);
                rest = &after[end + close.len()..];
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        // A link: [text](url)
        if let Some(after) = rest.strip_prefix('[')
            && let Some(close) = after.find("](")
            && let Some(end) = after[close + 2..].find(')')
        {
            push_text(out, &plain, marks);
            plain.clear();
            let mut inner = marks.clone();
            inner.link = Some(after[close + 2..close + 2 + end].to_string());
            parse(&after[..close], &inner, atoms, out);
            rest = &after[close + 2 + end + 1..];
            continue;
        }
        // A kept piece: ⟦label·n⟧
        if let Some(after) = rest.strip_prefix('⟦')
            && let Some(end) = after.find('⟧')
            && let Some((_, n)) = after[..end].rsplit_once('·')
            && let Ok(n) = n.parse::<usize>()
            && let Some(atom) = n.checked_sub(1).and_then(|i| atoms.get(i))
        {
            push_text(out, &plain, marks);
            plain.clear();
            out.push(Inline::Atom(atom.clone()));
            rest = &after[end + '⟧'.len_utf8()..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    push_text(out, &plain, marks);
}

pub fn style_to_kind(style: &ParaStyle) -> BlockKind {
    match style {
        ParaStyle::Title => BlockKind::Heading(1),
        ParaStyle::Heading(n) => BlockKind::Heading((*n).clamp(1, 3)),
        ParaStyle::ListItem { ordered: true, .. } => BlockKind::Numbered,
        ParaStyle::ListItem { ordered: false, .. } => BlockKind::Bullet,
        ParaStyle::Quote => BlockKind::Quote,
        ParaStyle::Normal | ParaStyle::Other(_) => BlockKind::Paragraph,
    }
}

/// The style a block of `kind` gives a paragraph that had `old`.
pub fn kind_to_style(kind: &BlockKind, old: &ParaStyle) -> ParaStyle {
    if style_to_kind(old) == *kind {
        return old.clone();
    }
    let level = match old {
        ParaStyle::ListItem { level, .. } => *level,
        _ => 0,
    };
    match kind {
        BlockKind::Heading(n) => ParaStyle::Heading(*n),
        BlockKind::Bullet | BlockKind::Todo(_) => ParaStyle::ListItem {
            level,
            ordered: false,
        },
        BlockKind::Numbered => ParaStyle::ListItem {
            level,
            ordered: true,
        },
        BlockKind::Quote | BlockKind::Callout => ParaStyle::Quote,
        _ => ParaStyle::Normal,
    }
}

/// One cell's paragraph, when a table can be edited as plain cells.
fn simple_cell(cell: &[Block]) -> Option<&Para> {
    match cell {
        [Block::Paragraph(p)]
            if !p.locked && !p.inlines.iter().any(|i| matches!(i, Inline::Atom(_))) =>
        {
            Some(p)
        }
        _ => None,
    }
}

/// What a part of the body the editor keeps as it is reads as, by its XML
/// name; `None` for page setup and markers, which have nothing to read.
pub fn opaque_text(name: &str) -> Option<String> {
    let what = match name {
        "sectPr"
        | "bookmarkStart"
        | "bookmarkEnd"
        | "proofErr"
        | "permStart"
        | "permEnd"
        | "commentRangeStart"
        | "commentRangeEnd"
        | "moveFromRangeStart"
        | "moveFromRangeEnd"
        | "moveToRangeStart"
        | "moveToRangeEnd"
        | "customXmlInsRangeStart"
        | "customXmlInsRangeEnd"
        | "customXmlDelRangeStart"
        | "customXmlDelRangeEnd" => {
            return None;
        }
        "sdt" => t("A content control"),
        "altChunk" => t("An embedded document"),
        "customXml" => t("Custom XML content"),
        "ins" | "del" | "moveFrom" | "moveTo" => t("A tracked change"),
        _ => t("Content this editor keeps as it is"),
    };
    Some(format!("{what}{}", t(" — edit it in Word")))
}

/// The blocks to show for a Word document, each with what it stands for.
pub fn word_blocks(blocks: &[Block]) -> Vec<(BlockData, Target)> {
    let mut out = Vec::new();
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                let (text, atoms) = inlines_to_text(&p.inlines, &atom_label);
                // A paragraph only Word edits shows boxed, so it reads as set apart.
                let kind = if p.locked {
                    BlockKind::Callout
                } else {
                    style_to_kind(&p.style)
                };
                out.push((
                    BlockData::new(kind, [text]),
                    Target::Para {
                        id: p.id,
                        locked: p.locked,
                        atoms,
                        style: p.style.clone(),
                    },
                ));
            }
            Block::Table(table) => {
                let columns = table.rows.first().map_or(0, Vec::len);
                let rectangular = columns > 0 && table.rows.iter().all(|r| r.len() == columns);
                let cells: Option<Vec<&Para>> = table
                    .rows
                    .iter()
                    .flat_map(|r| r.iter())
                    .map(|c| simple_cell(c))
                    .collect();
                match cells {
                    Some(cells) if rectangular && table.rows.len() >= 2 => {
                        let texts: Vec<String> = cells.iter().map(|p| p.text()).collect();
                        out.push((
                            BlockData::new(
                                BlockKind::Table {
                                    columns,
                                    merged: table.merged.clone(),
                                },
                                texts,
                            ),
                            Target::Table {
                                cells: cells.iter().map(|p| p.id).collect(),
                            },
                        ));
                    }
                    _ => {
                        // Shown as text, kept as it is.
                        let text: Vec<String> = table
                            .rows
                            .iter()
                            .map(|r| {
                                r.iter()
                                    .map(|c| {
                                        c.iter()
                                            .filter_map(|b| match b {
                                                Block::Paragraph(p) => Some(p.text()),
                                                _ => None,
                                            })
                                            .collect::<Vec<_>>()
                                            .join(" ")
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" | ")
                            })
                            .collect();
                        out.push((
                            BlockData::new(BlockKind::Callout, [text.join("\n")]),
                            Target::Fixed,
                        ));
                    }
                }
            }
            Block::Opaque { label, .. } => {
                if let Some(text) = opaque_text(label) {
                    out.push((BlockData::new(BlockKind::Callout, [text]), Target::Fixed));
                }
            }
        }
    }
    out
}

/// The edits that turn `before` into `after` (the editor's blocks, by key),
/// applied one by one through `apply`, which returns a new paragraph's id.
/// `targets` learns the keys of new paragraphs.
pub fn reconcile(
    before: &[(u64, BlockData)],
    after: &[(u64, BlockData)],
    targets: &mut HashMap<u64, Target>,
    apply: &mut dyn FnMut(WordOp) -> Option<NodeId>,
) -> Result<usize, Refusal> {
    let old: HashMap<u64, &BlockData> = before.iter().map(|(k, d)| (*k, d)).collect();
    let still: std::collections::HashSet<u64> = after.iter().map(|(k, _)| *k).collect();

    // Check everything first, so a refusal changes nothing.
    for (key, data) in after {
        let Some(was) = old.get(key) else { continue };
        if *was == data {
            continue;
        }
        match targets.get(key) {
            Some(Target::Para { locked: true, .. }) => return Err(Refusal::Locked),
            Some(Target::Fixed) => return Err(Refusal::Fixed),
            Some(Target::Table { cells })
                if cells.len() != data.texts.len() || was.kind != data.kind =>
            {
                return Err(Refusal::TableShape);
            }
            _ => {}
        }
    }
    for (key, _) in before {
        if !still.contains(key) {
            match targets.get(key) {
                Some(Target::Para { locked: true, .. }) => return Err(Refusal::Locked),
                Some(Target::Fixed) | Some(Target::Table { .. }) => return Err(Refusal::Fixed),
                _ => {}
            }
        }
    }
    for (key, data) in after {
        if !old.contains_key(key) && !data.kind.is_prose() {
            return Err(Refusal::TableShape);
        }
    }

    let mut edits = 0;
    // Removed paragraphs go first, so a moved one is not deleted after it was placed.
    for (key, _) in before {
        if !still.contains(key)
            && let Some(Target::Para { id, .. }) = targets.remove(key)
        {
            apply(WordOp::DeletePara { para: id });
            edits += 1;
        }
    }
    let mut prev: Option<NodeId> = None;
    for (key, data) in after {
        let text = data.texts.first().cloned().unwrap_or_default();
        match old.get(key) {
            None => {
                let style = kind_to_style(&data.kind, &ParaStyle::Normal);
                let inlines = text_to_inlines(&text, &[]);
                let at = prev.map_or(Place::BodyStart, Place::After);
                if let Some(id) = apply(WordOp::InsertPara {
                    at,
                    id: None,
                    style: style.clone(),
                    inlines,
                }) {
                    targets.insert(
                        *key,
                        Target::Para {
                            id,
                            locked: false,
                            atoms: Vec::new(),
                            style,
                        },
                    );
                    prev = Some(id);
                }
                edits += 1;
            }
            Some(was) => match targets.get_mut(key) {
                Some(Target::Para {
                    id, atoms, style, ..
                }) => {
                    if was.kind != data.kind {
                        let new = kind_to_style(&data.kind, style);
                        apply(WordOp::SetStyle {
                            para: *id,
                            style: new.clone(),
                        });
                        *style = new;
                        edits += 1;
                    }
                    if was.texts != data.texts {
                        apply(WordOp::SetContent {
                            para: *id,
                            inlines: text_to_inlines(&text, atoms),
                        });
                        edits += 1;
                    }
                    prev = Some(*id);
                }
                Some(Target::Table { cells }) => {
                    for (i, (a, b)) in was.texts.iter().zip(&data.texts).enumerate() {
                        if a != b {
                            apply(WordOp::SetContent {
                                para: cells[i],
                                inlines: text_to_inlines(b, &[]),
                            });
                            edits += 1;
                        }
                    }
                }
                _ => {}
            },
        }
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_office::{Office, WordDoc};

    fn label(_: &AtomKind) -> String {
        "image".into()
    }

    #[test]
    fn marks_round_trip_through_text() {
        let image = Atom {
            id: 9,
            kind: AtomKind::Image(None),
        };
        let inlines = vec![
            Inline::Text {
                text: "预计 ".into(),
                marks: Marks::default(),
            },
            Inline::Text {
                text: "65 万".into(),
                marks: Marks {
                    bold: true,
                    ..Default::default()
                },
            },
            Inline::Atom(image.clone()),
            Inline::Text {
                text: "见附件".into(),
                marks: Marks {
                    link: Some("https://a.b".into()),
                    underline: true,
                    ..Default::default()
                },
            },
        ];
        let (text, atoms) = inlines_to_text(&inlines, &label);
        assert_eq!(text, "预计 **65 万**⟦image·1⟧[<u>见附件</u>](https://a.b)");
        assert_eq!(text_to_inlines(&text, &atoms), inlines);
    }

    #[test]
    fn stray_markers_stay_text() {
        assert_eq!(
            text_to_inlines("2 * 3 = 6", &[]),
            vec![Inline::Text {
                text: "2 * 3 = 6".into(),
                marks: Marks::default()
            }]
        );
    }

    #[test]
    fn editor_changes_become_edits() {
        let mut office = Office::Word(WordDoc::from_blocks(vec![
            Block::Paragraph(Para::new(1, ParaStyle::Heading(1), "周报")),
            Block::Paragraph(Para::new(2, ParaStyle::Normal, "本周完成了登录页。")),
            Block::Paragraph(Para::new(3, ParaStyle::Normal, "下周做注册页。")),
        ]));
        let Office::Word(doc) = &office else {
            unreachable!()
        };
        let shown = word_blocks(doc.blocks());
        let mut targets: HashMap<u64, Target> = HashMap::new();
        let before: Vec<(u64, BlockData)> = shown
            .into_iter()
            .enumerate()
            .map(|(i, (data, target))| {
                targets.insert(i as u64 + 1, target);
                (i as u64 + 1, data)
            })
            .collect();
        let mut after = before.clone();
        after[1].1 = BlockData::new(BlockKind::Paragraph, ["本周完成了**登录页**和注册页。"]);
        after.remove(2);
        after.push((10, BlockData::new(BlockKind::Bullet, ["风险：无"])));
        let edits = reconcile(&before, &after, &mut targets, &mut |op| {
            let Office::Word(doc) = &mut office else {
                unreachable!()
            };
            match doc.apply(op).unwrap() {
                WordOp::DeletePara { para } => Some(para),
                _ => None,
            }
        })
        .unwrap();
        assert_eq!(edits, 3);
        let Office::Word(doc) = &office else {
            unreachable!()
        };
        let md = doc.to_markdown(&label);
        assert_eq!(
            md,
            "# 周报\n\n本周完成了**登录页**和注册页。\n\n- 风险：无\n"
        );
    }

    #[test]
    fn locked_paragraphs_refuse_and_change_nothing() {
        let mut p = Para::new(1, ParaStyle::Normal, "有修订");
        p.locked = true;
        let shown = word_blocks(&[Block::Paragraph(p)]);
        let mut targets = HashMap::from([(1u64, shown[0].1.clone())]);
        let before = vec![(1u64, shown[0].0.clone())];
        let after = vec![(1u64, BlockData::new(BlockKind::Paragraph, ["改了"]))];
        let mut called = false;
        let result = reconcile(&before, &after, &mut targets, &mut |_| {
            called = true;
            None
        });
        assert_eq!(result, Err(Refusal::Locked));
        assert!(!called);
    }
}
