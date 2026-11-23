use atrium_office::look::{PageLook, ParaLook, Section, TableLook};
use atrium_office::{Align, Block, Inline, Marks, Para, ParaStyle, Table, WordDoc};

use crate::{FixedShaper, Item, Pos, layout};

fn body(paras: Vec<Para>) -> WordDoc {
    WordDoc::from_blocks(paras.into_iter().map(Block::Paragraph).collect())
}

/// A page 100 pt wide with no margins, so a line holds 100 pt of text.
fn narrow(doc: WordDoc, height: i32) -> WordDoc {
    let page = PageLook {
        width: 2000,
        height,
        top: 0,
        bottom: 0,
        left: 0,
        right: 0,
        ..Default::default()
    };
    let sections = vec![Section {
        page,
        new_page: true,
        ..Default::default()
    }];
    let defaults = Marks {
        size: Some(200),
        ..Default::default()
    };
    doc.with_look(sections, defaults)
}

fn plain(id: u64, text: &str) -> Para {
    let mut p = Para::new(id, ParaStyle::Normal, text);
    p.look = ParaLook {
        widow_control: true,
        ..Default::default()
    };
    p
}

#[test]
fn chinese_text_wraps_at_any_character_but_not_before_a_comma() {
    // Ten 10 pt characters fill a line; the comma may not start the next.
    let doc = narrow(body(vec![plain(1, "一二三四五六七八九十，好")]), 20000);
    let out = layout(&doc, &FixedShaper);
    let lines: Vec<_> = out.lines.iter().map(|l| l.range.clone()).collect();
    assert_eq!(lines.len(), 2);
    // The tenth character moves down with the comma.
    assert_eq!(lines[0], 0..27);
    assert_eq!(lines[1].start, 27);
}

#[test]
fn latin_words_wrap_at_spaces() {
    // 5 pt per Latin letter: 20 letters a line.
    let doc = narrow(body(vec![plain(1, "aaaa bbbb cccc dddd eeee")]), 20000);
    let out = layout(&doc, &FixedShaper);
    assert_eq!(out.lines.len(), 2);
    assert_eq!(out.lines[1].range.start, 20);
}

#[test]
fn pages_break_when_full() {
    // Lines are 10 pt; the page holds 5.
    let paras = (1..=12).map(|i| plain(i, "字")).collect();
    let doc = narrow(body(paras), 1000);
    let out = layout(&doc, &FixedShaper);
    assert_eq!(out.pages.len(), 3);
    assert_eq!(out.lines[5].page, 1);
    assert_eq!(out.pages[2].number, 3);
}

#[test]
fn center_and_right_alignment_shift_the_line() {
    let mut p = plain(1, "字字");
    p.align = Align::Center;
    let mut q = plain(2, "字字");
    q.align = Align::Right;
    let out = layout(&narrow(body(vec![p, q]), 20000), &FixedShaper);
    assert_eq!(out.lines[0].stops[0].1, 40.0);
    assert_eq!(out.lines[1].stops[0].1, 80.0);
}

#[test]
fn caret_hit_and_arrows() {
    let doc = narrow(body(vec![plain(1, "一二三"), plain(2, "四五")]), 20000);
    let out = layout(&doc, &FixedShaper);
    let caret = out.caret(Pos::new(1, 3)).unwrap();
    assert_eq!(caret.x, 10.0);
    // A click near the right half of the second character lands after it.
    assert_eq!(out.hit(0, 17.0, 2.0), Some(Pos::new(1, 6)));
    // Off the end of the first paragraph steps into the second.
    assert_eq!(out.step(Pos::new(1, 9), true), Some(Pos::new(2, 0)));
    assert_eq!(out.step(Pos::new(2, 0), false), Some(Pos::new(1, 9)));
    assert_eq!(
        out.vertical(Pos::new(1, 6), true, 20.0),
        Some(Pos::new(2, 6))
    );
    let sel = out.selection(Pos::new(1, 3), Pos::new(2, 3));
    assert_eq!(sel.len(), 2);
    assert_eq!(sel[0].1.x, 10.0);
    assert_eq!(sel[1].1.width, 10.0);
}

#[test]
fn an_empty_paragraph_still_has_a_line_and_a_caret() {
    let doc = narrow(body(vec![plain(1, ""), plain(2, "字")]), 20000);
    let out = layout(&doc, &FixedShaper);
    assert_eq!(out.lines.len(), 2);
    assert!(out.caret(Pos::new(1, 0)).is_some());
    assert_eq!(out.lines[1].top, 10.0);
}

#[test]
fn a_forced_break_starts_a_new_line() {
    let mut p = plain(1, "");
    p.inlines = vec![
        Inline::Text {
            text: "甲".into(),
            marks: Marks::default(),
        },
        Inline::Break,
        Inline::Text {
            text: "乙".into(),
            marks: Marks::default(),
        },
    ];
    let out = layout(&narrow(body(vec![p]), 20000), &FixedShaper);
    assert_eq!(out.lines.len(), 2);
    assert_eq!(out.lines[1].range, 4..7);
    assert_eq!(out.caret(Pos::new(1, 4)).unwrap().top, 10.0);
}

#[test]
fn numbered_items_count_up() {
    let item = |id| {
        Para::new(
            id,
            ParaStyle::ListItem {
                level: 0,
                ordered: true,
            },
            "项",
        )
    };
    let out = layout(&narrow(body(vec![item(1), item(2)]), 20000), &FixedShaper);
    let labels: Vec<String> = out.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text { text, .. } if text.ends_with('.') => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["1.", "2."]);
    // The caret starts at the text, after the label.
    assert!(out.caret(Pos::new(1, 0)).unwrap().x >= 21.0);
}

#[test]
fn tables_put_cells_side_by_side_with_borders() {
    let cell = |id, text| vec![Block::Paragraph(plain(id, text))];
    let table = Table {
        id: 10,
        rows: vec![vec![cell(11, "甲"), cell(12, "乙")]],
        merged: vec![],
        look: TableLook::default(),
    };
    let doc = narrow(WordDoc::from_blocks(vec![Block::Table(table)]), 20000);
    let out = layout(&doc, &FixedShaper);
    assert_eq!(out.lines.len(), 2);
    assert!(out.lines[1].stops[0].1 > 50.0);
    let rules = out.pages[0]
        .items
        .iter()
        .filter(|i| matches!(i, Item::Rule { .. }))
        .count();
    assert_eq!(rules, 8);
}
