//! What you changed in a document, told the way a person would, so the agent
//! can carry on from your version.

use crate::Office;
use crate::sheet::a1;
use crate::word::{Block, NodeId, ParaStyle};

/// Where a piece of text sits, as a person names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// The `n`th paragraph (from 1) under heading `section`, or from the start.
    Para {
        section: Option<String>,
        n: usize,
    },
    TableCell {
        table: usize,
        row: usize,
        col: usize,
    },
    SheetCell {
        sheet: String,
        cell: String,
    },
    Slide {
        n: usize,
        title: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    Node(NodeId),
    Cell(usize, u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub key: Key,
    pub at: Location,
    pub text: String,
    /// A heading's level, so restyling shows.
    pub style: String,
}

/// Every piece of text in a document, in reading order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot(pub Vec<Piece>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Edited {
        at: Location,
        old: String,
        new: String,
    },
    Added {
        at: Location,
        text: String,
    },
    Removed {
        at: Location,
        text: String,
    },
    Restyled {
        at: Location,
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

fn style_name(style: &ParaStyle) -> String {
    match style {
        ParaStyle::Heading(n) => format!("h{n}"),
        ParaStyle::Title => "title".into(),
        ParaStyle::ListItem { .. } => "list".into(),
        ParaStyle::Quote => "quote".into(),
        _ => String::new(),
    }
}

pub(crate) fn snapshot(office: &Office) -> Snapshot {
    let mut pieces = Vec::new();
    match office {
        Office::Word(doc) => {
            let mut section: Option<String> = None;
            let mut n = 0;
            let mut tables = 0;
            for block in doc.blocks() {
                match block {
                    Block::Paragraph(p) => {
                        let text = p.text();
                        if matches!(p.style, ParaStyle::Heading(_) | ParaStyle::Title) {
                            pieces.push(Piece {
                                key: Key::Node(p.id),
                                at: Location::Para {
                                    section: section.clone(),
                                    n: n + 1,
                                },
                                text: text.clone(),
                                style: style_name(&p.style),
                            });
                            section = Some(text.trim().to_string());
                            n = 0;
                            continue;
                        }
                        n += 1;
                        pieces.push(Piece {
                            key: Key::Node(p.id),
                            at: Location::Para {
                                section: section.clone(),
                                n,
                            },
                            text,
                            style: style_name(&p.style),
                        });
                    }
                    Block::Table(t) => {
                        tables += 1;
                        for (r, row) in t.rows.iter().enumerate() {
                            for (c, cell) in row.iter().enumerate() {
                                for b in cell {
                                    if let Block::Paragraph(p) = b {
                                        pieces.push(Piece {
                                            key: Key::Node(p.id),
                                            at: Location::TableCell {
                                                table: tables,
                                                row: r + 1,
                                                col: c + 1,
                                            },
                                            text: p.text(),
                                            style: String::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                    Block::Opaque { .. } => {}
                }
            }
        }
        Office::Sheet(wb) => {
            for (i, sheet) in wb.sheets().iter().enumerate() {
                for ((r, c), cell) in &sheet.cells {
                    pieces.push(Piece {
                        key: Key::Cell(i, *r, *c),
                        at: Location::SheetCell {
                            sheet: sheet.name.clone(),
                            cell: a1(*r, *c),
                        },
                        text: cell.input(),
                        style: String::new(),
                    });
                }
            }
        }
        Office::Slides(deck) => {
            for (i, slide) in deck.slides().iter().enumerate() {
                let title = slide.title();
                for p in slide.shapes.iter().flat_map(|s| s.text_paras()) {
                    pieces.push(Piece {
                        key: Key::Node(p.id),
                        at: Location::Slide {
                            n: i + 1,
                            title: title.clone(),
                        },
                        text: p.text(),
                        style: String::new(),
                    });
                }
            }
        }
    }
    Snapshot(pieces)
}

/// What changed from `before` to `after`, in `after`'s reading order, with
/// removals where they were.
pub fn changes(before: &Snapshot, after: &Snapshot) -> Vec<Change> {
    use std::collections::HashMap;
    let old: HashMap<&Key, &Piece> = before.0.iter().map(|p| (&p.key, p)).collect();
    let new: HashMap<&Key, &Piece> = after.0.iter().map(|p| (&p.key, p)).collect();
    let mut out = Vec::new();
    // Removals are reported before whatever followed them.
    let mut removed: Vec<&Piece> = before
        .0
        .iter()
        .filter(|p| !new.contains_key(&p.key))
        .collect();
    removed.reverse();
    for piece in &after.0 {
        while let Some(gone) = removed.last() {
            let gone_at = before.0.iter().position(|p| p.key == gone.key).unwrap_or(0);
            let next_at = before.0.iter().position(|p| p.key == piece.key);
            if next_at.is_some_and(|n| n > gone_at) {
                out.push(Change::Removed {
                    at: gone.at.clone(),
                    text: gone.text.clone(),
                });
                removed.pop();
            } else {
                break;
            }
        }
        match old.get(&piece.key) {
            None if piece.text.trim().is_empty() => {}
            None => out.push(Change::Added {
                at: piece.at.clone(),
                text: piece.text.clone(),
            }),
            Some(was) if was.text != piece.text => out.push(Change::Edited {
                at: piece.at.clone(),
                old: was.text.clone(),
                new: piece.text.clone(),
            }),
            Some(was) if was.style != piece.style => out.push(Change::Restyled {
                at: piece.at.clone(),
                text: piece.text.clone(),
            }),
            Some(_) => {}
        }
    }
    for gone in removed.into_iter().rev() {
        if !gone.text.trim().is_empty() {
            out.push(Change::Removed {
                at: gone.at.clone(),
                text: gone.text.clone(),
            });
        }
    }
    out.retain(|c| !matches!(c, Change::Removed { text, .. } if text.trim().is_empty()));
    out
}

const QUOTE: usize = 200;
const MOST: usize = 30;

fn quote(text: &str) -> String {
    let flat: String = text
        .replace('\u{FFFC}', "▢")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() > QUOTE {
        format!("{}…", flat.chars().take(QUOTE).collect::<String>())
    } else {
        flat
    }
}

fn place(at: &Location, lang: Lang) -> String {
    match (at, lang) {
        (
            Location::Para {
                section: Some(s),
                n,
            },
            Lang::Zh,
        ) => format!("「{}」第 {n} 段", quote(s)),
        (Location::Para { section: None, n }, Lang::Zh) => format!("开头第 {n} 段"),
        (
            Location::Para {
                section: Some(s),
                n,
            },
            Lang::En,
        ) => format!("\"{}\" paragraph {n}", quote(s)),
        (Location::Para { section: None, n }, Lang::En) => format!("paragraph {n} at the start"),
        (Location::TableCell { table, row, col }, Lang::Zh) => {
            format!("表格 {table} 第 {row} 行第 {col} 列")
        }
        (Location::TableCell { table, row, col }, Lang::En) => {
            format!("table {table}, row {row}, column {col}")
        }
        (Location::SheetCell { sheet, cell }, _) => format!("{sheet}!{cell}"),
        (Location::Slide { n, title }, Lang::Zh) if !title.is_empty() => {
            format!("第 {n} 张幻灯片「{}」", quote(title))
        }
        (Location::Slide { n, .. }, Lang::Zh) => format!("第 {n} 张幻灯片"),
        (Location::Slide { n, title }, Lang::En) if !title.is_empty() => {
            format!("slide {n} \"{}\"", quote(title))
        }
        (Location::Slide { n, .. }, Lang::En) => format!("slide {n}"),
    }
}

/// A short report of `changes` to `file`, for the agent.
pub fn describe(file: &str, changes: &[Change], lang: Lang) -> String {
    let zh = lang == Lang::Zh;
    let mut out = if zh {
        format!("我在 {file} 里改了这些地方：\n")
    } else {
        format!("I changed these places in {file}:\n")
    };
    for c in changes.iter().take(MOST) {
        let line = match (c, zh) {
            (Change::Edited { at, old, new }, true) => {
                format!("{}：「{}」→「{}」", place(at, lang), quote(old), quote(new))
            }
            (Change::Edited { at, old, new }, false) => format!(
                "{}: \"{}\" → \"{}\"",
                place(at, lang),
                quote(old),
                quote(new)
            ),
            (Change::Added { at, text }, true) => {
                format!("{}：新增「{}」", place(at, lang), quote(text))
            }
            (Change::Added { at, text }, false) => {
                format!("{}: added \"{}\"", place(at, lang), quote(text))
            }
            (Change::Removed { at, text }, true) => {
                format!("删除了{}「{}」", place(at, lang), quote(text))
            }
            (Change::Removed { at, text }, false) => {
                format!("removed {} \"{}\"", place(at, lang), quote(text))
            }
            (Change::Restyled { at, text }, true) => {
                format!("{}：改了格式「{}」", place(at, lang), quote(text))
            }
            (Change::Restyled { at, text }, false) => {
                format!("{}: restyled \"{}\"", place(at, lang), quote(text))
            }
        };
        out.push_str(&format!("- {line}\n"));
    }
    if changes.len() > MOST {
        let more = changes.len() - MOST;
        out.push_str(&if zh {
            format!("- 还有 {more} 处改动\n")
        } else {
            format!("- and {more} more changes\n")
        });
    }
    out.push_str(if zh {
        "请在这个基础上继续。"
    } else {
        "Please carry on from this version."
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{Sheet, SheetOp, Workbook};
    use crate::word::*;

    fn text(s: &str) -> Vec<Inline> {
        vec![Inline::Text {
            text: s.into(),
            marks: Marks::default(),
        }]
    }

    #[test]
    fn edits_and_removals_read_like_a_person_wrote_them() {
        let mut office = Office::Word(WordDoc::from_blocks(vec![
            Block::Paragraph(Para::new(1, ParaStyle::Heading(1), "预算")),
            Block::Paragraph(Para::new(2, ParaStyle::Normal, "第一段")),
            Block::Paragraph(Para::new(3, ParaStyle::Normal, "预计 50 万")),
            Block::Paragraph(Para::new(4, ParaStyle::Heading(1), "风险")),
            Block::Paragraph(Para::new(5, ParaStyle::Normal, "供应商延期")),
        ]));
        let before = office.snapshot();
        let Office::Word(doc) = &mut office else {
            unreachable!()
        };
        doc.apply(WordOp::SetContent {
            para: 3,
            inlines: text("预计 65 万，含外包"),
        })
        .unwrap();
        doc.apply(WordOp::DeletePara { para: 5 }).unwrap();
        let report = describe("报告.docx", &changes(&before, &office.snapshot()), Lang::Zh);
        assert_eq!(
            report,
            "我在 报告.docx 里改了这些地方：\n\
             - 「预算」第 2 段：「预计 50 万」→「预计 65 万，含外包」\n\
             - 删除了「风险」第 1 段「供应商延期」\n\
             请在这个基础上继续。"
        );
    }

    #[test]
    fn cells_are_reported_by_name() {
        let mut office = Office::Sheet(Workbook::from_sheets(vec![Sheet::new("Sheet1")]));
        let before = office.snapshot();
        let Office::Sheet(wb) = &mut office else {
            unreachable!()
        };
        wb.apply(SheetOp::SetCell {
            sheet: 0,
            r: 6,
            c: 1,
            input: "150".into(),
        })
        .unwrap();
        let report = describe("预算.xlsx", &changes(&before, &office.snapshot()), Lang::En);
        assert!(report.contains("- Sheet1!B7: added \"150\""), "{report}");
    }
}
