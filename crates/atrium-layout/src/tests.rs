use atrium_office::look::{
    AtomLook, Baseline, Border, Borders, CellLook, Float, LineSpacing, Origin, PageLook, ParaLook,
    Rgb, Section, TabKind, TabStop, TableLook, Wrap,
};
use atrium_office::{
    Align, Atom, AtomKind, Block, Footnote, Inline, Marks, Para, ParaStyle, Table, WordDoc,
};

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

fn page(doc: WordDoc, width: i32, height: i32, pitch: Option<i32>) -> WordDoc {
    let page = PageLook {
        width,
        height,
        top: 0,
        bottom: 0,
        left: 0,
        right: 0,
        line_pitch: pitch,
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

fn cell(id: u64, text: &str) -> Vec<Block> {
    vec![Block::Paragraph(plain(id, text))]
}

fn table_doc(table: Table, width: i32, height: i32) -> WordDoc {
    looked(vec![Block::Table(table)], one_section(width, height))
}

#[test]
fn a_table_grid_is_scaled_to_the_text_area() {
    let table = Table {
        id: 10,
        rows: vec![vec![cell(11, "甲"), cell(12, "乙")]],
        merged: vec![],
        look: TableLook {
            columns: vec![1000, 1000],
            padding: [0; 4],
            ..Default::default()
        },
    };
    let out = layout(&table_doc(table, 1000, 2000), &FixedShaper);
    assert!(out.lines[1].stops[0].1 >= 25.0);
    assert!(out.lines[1].stops[0].1 < 30.0);
}

#[test]
fn a_table_without_a_grid_fits_its_contents() {
    let table = Table {
        id: 10,
        rows: vec![vec![cell(11, "字"), cell(12, "一二")]],
        merged: vec![],
        look: TableLook {
            padding: [1, 0, 0, 0],
            ..Default::default()
        },
    };
    let out = layout(&table_doc(table, 4000, 2000), &FixedShaper);
    assert!(
        close(out.lines[1].stops[0].1, 10.0),
        "{} {:?}",
        out.lines[1].stops[0].1,
        out.lines
            .iter()
            .map(|l| (l.para, l.stops[0].1))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_table_row_splits_unless_cant_split() {
    let table = |cant| Table {
        id: 10,
        rows: vec![vec![cell(11, "一二三四五六")]],
        merged: vec![],
        look: TableLook {
            columns: vec![400],
            cant_split: vec![cant],
            padding: [0; 4],
            ..Default::default()
        },
    };
    let doc = |cant| {
        looked(
            vec![
                Block::Paragraph(plain(1, "一二三四")),
                Block::Table(table(cant)),
            ],
            one_section(400, 800),
        )
    };
    let out = layout(&doc(false), &FixedShaper);
    assert_eq!(out.pages.len(), 2);
    assert_eq!(
        out.lines
            .iter()
            .filter(|l| l.page == 0 && l.para == 11)
            .count(),
        2,
    );
    assert_eq!(
        out.lines
            .iter()
            .filter(|l| l.page == 1 && l.para == 11)
            .count(),
        1
    );

    let out = layout(&doc(true), &FixedShaper);
    assert_eq!(out.pages.len(), 2);
    assert_eq!(
        out.lines
            .iter()
            .filter(|l| l.page == 1 && l.para == 11)
            .count(),
        3
    );
}

#[test]
fn header_rows_repeat_on_later_pages() {
    let table = Table {
        id: 10,
        rows: vec![
            vec![cell(100, "头")],
            vec![cell(101, "一")],
            vec![cell(102, "二")],
            vec![cell(103, "三")],
        ],
        merged: vec![],
        look: TableLook {
            columns: vec![400],
            header_rows: 1,
            padding: [0; 4],
            ..Default::default()
        },
    };
    let out = layout(&table_doc(table, 400, 400), &FixedShaper);
    assert_eq!(out.pages.len(), 3);
    let pages: Vec<usize> = out
        .lines
        .iter()
        .filter(|l| l.para == 100)
        .map(|l| l.page)
        .collect();
    assert_eq!(pages, [0, 1, 2]);
}

#[test]
fn a_vertical_merge_is_one_cell_with_text_in_the_first_row() {
    let table = Table {
        id: 10,
        rows: vec![
            vec![cell(11, "合"), cell(12, "一")],
            vec![cell(13, "二"), cell(14, "三")],
        ],
        merged: vec![],
        look: TableLook {
            columns: vec![200, 200],
            padding: [0; 4],
            cells: vec![
                vec![
                    CellLook {
                        shading: Some(Rgb(0xFF0000)),
                        ..Default::default()
                    },
                    CellLook::default(),
                ],
                vec![
                    CellLook {
                        merged_up: true,
                        ..Default::default()
                    },
                    CellLook::default(),
                ],
            ],
            ..Default::default()
        },
    };
    let out = layout(&table_doc(table, 400, 2000), &FixedShaper);
    assert_eq!(out.lines.iter().filter(|l| l.para == 11).count(), 1);
    assert!(out.lines.iter().all(|l| l.para != 13));
    assert!(out.pages[0].items.iter().any(|i| matches!(
        i,
        Item::Fill { color, rect }
            if *color == Rgb(0xFF0000) && close(rect.height, 20.0)
    )));
}

#[test]
fn nested_tables_are_laid_out_inside_their_cell() {
    let inner = Table {
        id: 20,
        rows: vec![vec![cell(21, "内")]],
        merged: vec![],
        look: TableLook {
            columns: vec![200],
            padding: [0; 4],
            ..Default::default()
        },
    };
    let table = Table {
        id: 10,
        rows: vec![vec![vec![Block::Table(inner)]]],
        merged: vec![],
        look: TableLook {
            columns: vec![400],
            padding: [0; 4],
            ..Default::default()
        },
    };
    let out = layout(&table_doc(table, 4000, 2000), &FixedShaper);
    let line = out.lines.iter().find(|l| l.para == 21).unwrap();
    assert!(line.stops[0].1 >= 0.0);
    assert!(
        out.lines
            .iter()
            .any(|l| l.para == 21 && l.stops[0].1 < 200.0)
    );
}

#[test]
fn a_wider_border_wins_where_two_cells_meet() {
    let wide = Border {
        width: 40,
        color: Rgb::BLACK,
        space: 0,
    };
    let narrow = Border {
        width: 10,
        color: Rgb::BLACK,
        space: 0,
    };
    let table = Table {
        id: 10,
        rows: vec![vec![cell(11, "甲"), cell(12, "乙")]],
        merged: vec![],
        look: TableLook {
            columns: vec![200, 200],
            padding: [0; 4],
            cells: vec![vec![
                CellLook {
                    borders: Some(Borders {
                        right: Some(wide),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                CellLook {
                    borders: Some(Borders {
                        left: Some(narrow),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ]],
            ..Default::default()
        },
    };
    let out = layout(&table_doc(table, 400, 2000), &FixedShaper);
    let shared: Vec<f32> = out.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Rule {
                from, to, width, ..
            } if close(from.0, 10.0) && close(to.0, 10.0) => Some(*width),
            _ => None,
        })
        .collect();
    assert_eq!(shared.len(), 1);
    assert!(close(shared[0], 2.0));
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.05
}

#[test]
fn overflow_punctuation_may_hang_past_the_edge() {
    let text = "一二三四五六七八九十，好";
    let mut hanging = plain(1, text);
    hanging.look.overflow_punct = true;
    let out = layout(&page(body(vec![hanging]), 2100, 20000, None), &FixedShaper);
    // The comma starts inside the 105 pt line and hangs; the next line is 好.
    assert_eq!(out.lines[0].range, 0..33);
    assert_eq!(out.lines[1].range, 33..36);

    let mut strict = plain(1, text);
    strict.look.overflow_punct = false;
    let out = layout(&page(body(vec![strict]), 2100, 20000, None), &FixedShaper);
    assert_eq!(out.lines[0].range, 0..27);
}

#[test]
fn justification_stretches_latin_spaces_not_the_last_line() {
    let mut p = plain(1, "aa aa aa aa");
    p.align = Align::Justify;
    let out = layout(&page(body(vec![p]), 1000, 20000, None), &FixedShaper);
    assert_eq!(out.lines.len(), 2);
    // The first word's trailing space grew from 5 to 10; the line's own
    // trailing space hangs past the margin, so the line-end caret is at 55.
    let at_second_word = out.lines[0]
        .stops
        .iter()
        .find(|(o, _)| *o == 3)
        .map(|s| s.1)
        .unwrap();
    assert!(close(at_second_word, 20.0));
    assert!(
        close(out.lines[0].stops.last().unwrap().1, 55.0),
        "{:?}",
        out.lines[0].stops
    );
    assert!(close(out.lines[1].stops.last().unwrap().1, 10.0));
}

#[test]
fn justification_stretches_cjk_gaps_not_the_last_line() {
    let mut p = plain(1, "一二三四五六");
    p.align = Align::Justify;
    let out = layout(&page(body(vec![p]), 900, 20000, None), &FixedShaper);
    assert_eq!(out.lines.len(), 2);
    assert!(close(out.lines[0].stops.last().unwrap().1, 45.0));
    assert!(close(out.lines[1].stops.last().unwrap().1, 20.0));
}

#[test]
fn distribute_stretches_every_line() {
    let mut p = plain(1, "一二三四五六");
    p.align = Align::Justify;
    p.look.distribute = true;
    let out = layout(&page(body(vec![p]), 900, 20000, None), &FixedShaper);
    assert!(close(out.lines[0].stops.last().unwrap().1, 45.0));
    assert!(close(out.lines[1].stops.last().unwrap().1, 45.0));
}

#[test]
fn script_spacing_adds_a_quarter_em_between_cjk_and_latin_or_numbers() {
    let out = layout(
        &page(body(vec![plain(1, "中A中")]), 2000, 20000, None),
        &FixedShaper,
    );
    assert!(close(out.lines[0].stops.last().unwrap().1, 30.0));

    let mut off = plain(1, "中A中");
    off.look.auto_space_de = false;
    let out = layout(&page(body(vec![off]), 2000, 20000, None), &FixedShaper);
    assert!(close(out.lines[0].stops.last().unwrap().1, 25.0));

    let out = layout(
        &page(body(vec![plain(1, "中1中")]), 2000, 20000, None),
        &FixedShaper,
    );
    assert!(close(out.lines[0].stops.last().unwrap().1, 30.0));
    let mut off = plain(1, "中1中");
    off.look.auto_space_dn = false;
    let out = layout(&page(body(vec![off]), 2000, 20000, None), &FixedShaper);
    assert!(close(out.lines[0].stops.last().unwrap().1, 25.0));
}

#[test]
fn tabs_honour_alignment_kind_and_leaders() {
    let mut p = plain(1, "A\tB");
    p.look.tabs = vec![TabStop {
        at: 1000,
        kind: TabKind::Left,
        leader: Some('.'),
    }];
    let out = layout(&page(body(vec![p]), 2000, 20000, None), &FixedShaper);
    assert!(close(out.lines[0].stops.last().unwrap().1, 55.0));
    let b = out.pages[0].items.iter().find_map(|i| match i {
        Item::Text { x, text, .. } if text == "B" => Some(*x),
        _ => None,
    });
    assert!(b.is_some_and(|x| close(x, 50.0)));
    assert!(out.pages[0].items.iter().any(|i| matches!(
        i,
        Item::Text { text, .. } if text.starts_with("....")
    )));

    for (kind, expected) in [
        (TabKind::Center, 45.0),
        (TabKind::Right, 40.0),
        (TabKind::Decimal, 45.0),
    ] {
        let mut p = plain(
            1,
            if kind == TabKind::Decimal {
                "A\t1.5"
            } else {
                "A\tBB"
            },
        );
        p.look.tabs = vec![TabStop {
            at: 1000,
            kind,
            leader: None,
        }];
        let out = layout(&page(body(vec![p]), 2000, 20000, None), &FixedShaper);
        let b = out.pages[0].items.iter().find_map(|i| match i {
            Item::Text { x, text, .. }
                if text.starts_with(if kind == TabKind::Decimal { "1" } else { "B" }) =>
            {
                Some(*x)
            }
            _ => None,
        });
        assert!(b.is_some_and(|x| close(x, expected)), "{kind:?}");
    }
}

#[test]
fn default_tab_stops_come_from_the_document() {
    let doc = page(body(vec![plain(1, "A\tB")]), 2000, 20000, None).with_default_tab_stop(420);
    let out = layout(&doc, &FixedShaper);
    let b = out.pages[0].items.iter().find_map(|i| match i {
        Item::Text { x, text, .. } if text == "B" => Some(*x),
        _ => None,
    });
    assert!(b.is_some_and(|x| close(x, 21.0)));
}

#[test]
fn line_spacing_follows_auto_at_least_and_exact() {
    let mut p = plain(1, "字");
    p.look.line = LineSpacing::Auto(360);
    let out = layout(&page(body(vec![p]), 2000, 20000, None), &FixedShaper);
    assert!(close(out.lines[0].height, 15.0));

    let mut p = plain(1, "字");
    p.look.line = LineSpacing::AtLeast(300);
    let out = layout(&page(body(vec![p]), 2000, 20000, None), &FixedShaper);
    assert!(close(out.lines[0].height, 15.0));

    let mut p = plain(1, "字");
    p.look.line = LineSpacing::Exact(100);
    let out = layout(&page(body(vec![p]), 2000, 20000, None), &FixedShaper);
    assert!(close(out.lines[0].height, 5.0));
}

#[test]
fn the_document_grid_rounds_lines_up_and_centres_the_text() {
    let mut p = plain(1, "字");
    p.look.snap_to_grid = true;
    let out = layout(&page(body(vec![p]), 2000, 20000, Some(300)), &FixedShaper);
    assert!(close(out.lines[0].height, 15.0));
    assert!(close(out.lines[0].baseline - out.lines[0].top, 10.5));
}

#[test]
fn superscript_and_subscript_shift_the_baseline() {
    let mut super_text = Marks {
        baseline: Baseline::Superscript,
        ..Default::default()
    };
    super_text.size = Some(200);
    let mut p = plain(1, "");
    p.inlines = vec![
        Inline::Text {
            text: "A".into(),
            marks: Marks {
                size: Some(200),
                ..Default::default()
            },
        },
        Inline::Text {
            text: "B".into(),
            marks: super_text,
        },
    ];
    let out = layout(
        &page(body(vec![p.clone()]), 2000, 20000, None),
        &FixedShaper,
    );
    let baselines: Vec<f32> = out.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text { baseline, text, .. } if text == "A" || text == "B" => Some(*baseline),
            _ => None,
        })
        .collect();
    assert_eq!(baselines.len(), 2);
    assert!(baselines[1] < baselines[0] - 2.0);

    if let Inline::Text { marks, .. } = &mut p.inlines[1] {
        marks.baseline = Baseline::Subscript;
    }
    let out = layout(&page(body(vec![p]), 2000, 20000, None), &FixedShaper);
    let baselines: Vec<f32> = out.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text { baseline, text, .. } if text == "A" || text == "B" => Some(*baseline),
            _ => None,
        })
        .collect();
    assert!(baselines[1] > baselines[0] + 0.5);
}

#[test]
fn underline_and_highlight_are_drawn() {
    let marks = Marks {
        underline: true,
        highlight: Some(atrium_office::look::Rgb(0xFFFF00)),
        size: Some(200),
        ..Default::default()
    };
    let mut p = plain(1, "");
    p.inlines = vec![Inline::Text {
        text: "字".into(),
        marks,
    }];
    let out = layout(&page(body(vec![p]), 2000, 20000, None), &FixedShaper);
    assert!(
        out.pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Rule { .. }))
    );
    assert!(out.pages[0].items.iter().any(|i| matches!(
        i,
        Item::Fill { color, .. } if *color == atrium_office::look::Rgb(0xFFFF00)
    )));
}

fn looked(blocks: Vec<Block>, sections: Vec<Section>) -> WordDoc {
    WordDoc::from_blocks(blocks).with_look(
        sections,
        Marks {
            size: Some(200),
            ..Default::default()
        },
    )
}

fn one_section(width: i32, height: i32) -> Vec<Section> {
    vec![Section {
        page: PageLook {
            width,
            height,
            top: 0,
            bottom: 0,
            left: 0,
            right: 0,
            header: 0,
            footer: 0,
            ..Default::default()
        },
        new_page: true,
        ..Default::default()
    }]
}

#[test]
fn paragraph_spacing_adds_and_contextual_spacing_collapses_it() {
    let mut first = plain(1, "字");
    first.look.after = 240;
    let mut second = plain(2, "字");
    second.look.before = 160;
    let out = layout(
        &looked(
            vec![
                Block::Paragraph(first.clone()),
                Block::Paragraph(second.clone()),
            ],
            one_section(2000, 2000),
        ),
        &FixedShaper,
    );
    assert!(close(out.lines[1].top, 30.0));

    second.look.contextual = true;
    let out = layout(
        &looked(
            vec![Block::Paragraph(first), Block::Paragraph(second)],
            one_section(2000, 2000),
        ),
        &FixedShaper,
    );
    assert!(close(out.lines[1].top, 10.0));
}

#[test]
fn space_before_is_dropped_at_the_top_of_a_page_but_not_the_first_page() {
    let mut first = plain(1, "一字");
    first.look.before = 200;
    let out = layout(
        &looked(vec![Block::Paragraph(first)], one_section(2000, 20000)),
        &FixedShaper,
    );
    assert!(close(out.lines[0].top, 10.0));

    let mut early = plain(1, "一二三四");
    early.look.before = 0;
    let mut later = plain(2, "五");
    later.look.before = 400;
    let out = layout(
        &looked(
            vec![Block::Paragraph(early), Block::Paragraph(later)],
            one_section(400, 500),
        ),
        &FixedShaper,
    );
    assert_eq!(out.pages.len(), 2);
    let line = out.lines.iter().find(|l| l.para == 2).unwrap();
    assert!(close(line.top, 0.0));
}

#[test]
fn keep_next_moves_a_heading_with_its_first_body_line() {
    let mut heading = plain(2, "标题");
    heading.look.keep_next = true;
    let out = layout(
        &looked(
            vec![
                Block::Paragraph(plain(1, "一二三四五六")),
                Block::Paragraph(heading),
                Block::Paragraph(plain(3, "一二三四")),
            ],
            one_section(400, 600),
        ),
        &FixedShaper,
    );
    let heading_line = out.lines.iter().find(|l| l.para == 2).unwrap();
    let body_line = out.lines.iter().find(|l| l.para == 3).unwrap();
    assert_eq!(heading_line.page, 1,);
    assert_eq!(body_line.page, 1);
}

#[test]
fn keep_lines_moves_a_paragraph_whole() {
    let mut whole = plain(2, "五六七八九");
    whole.look.keep_lines = true;
    let out = layout(
        &looked(
            vec![
                Block::Paragraph(plain(1, "一二三四")),
                Block::Paragraph(whole),
            ],
            one_section(400, 800),
        ),
        &FixedShaper,
    );
    let pages: Vec<usize> = out
        .lines
        .iter()
        .filter(|l| l.para == 2)
        .map(|l| l.page)
        .collect();
    assert_eq!(pages, [1, 1, 1]);
}

#[test]
fn widow_control_leaves_two_lines_at_the_bottom_and_two_at_the_top() {
    let mut long = plain(2, "一二三四五六七八");
    long.look.widow_control = true;
    let out = layout(
        &looked(
            vec![Block::Paragraph(plain(1, "字")), Block::Paragraph(long)],
            one_section(400, 800),
        ),
        &FixedShaper,
    );
    let pages: Vec<usize> = out
        .lines
        .iter()
        .filter(|l| l.para == 2)
        .map(|l| l.page)
        .collect();
    assert_eq!(pages, [0, 0, 1, 1]);
}

#[test]
fn page_break_before_starts_a_page() {
    let mut p = plain(2, "字");
    p.look.page_break_before = true;
    let out = layout(
        &looked(
            vec![Block::Paragraph(plain(1, "字")), Block::Paragraph(p)],
            one_section(2000, 20000),
        ),
        &FixedShaper,
    );
    assert_eq!(out.pages.len(), 2);
    assert_eq!(out.lines[1].page, 1);
}

#[test]
fn a_continuous_section_stays_on_the_page() {
    let mut sections = one_section(2000, 20000);
    sections[0].last_block = Some(1);
    let mut second = Section {
        page: sections[0].page.clone(),
        new_page: false,
        ..Default::default()
    };
    second.last_block = None;
    sections.push(second);
    let out = layout(
        &looked(
            vec![
                Block::Paragraph(plain(1, "字")),
                Block::Paragraph(plain(2, "字")),
            ],
            sections,
        ),
        &FixedShaper,
    );
    assert_eq!(out.pages.len(), 1);
    assert!(close(out.lines[1].top, 10.0));
}

#[test]
fn columns_fill_down_then_across() {
    let mut sections = one_section(2000, 400);
    sections[0].page.columns = 2;
    sections[0].page.column_gap = 0;
    let out = layout(
        &looked(
            vec![
                Block::Paragraph(plain(1, "一")),
                Block::Paragraph(plain(2, "二")),
                Block::Paragraph(plain(3, "三")),
            ],
            sections,
        ),
        &FixedShaper,
    );
    assert_eq!(out.pages.len(), 1);
    assert_eq!(out.lines[2].page, 0);
    assert!(out.lines[2].stops[0].1 >= 49.0);
    assert!(close(out.lines[2].top, 0.0));
}

#[test]
fn a_different_first_page_uses_the_first_header() {
    let header = vec![Block::Paragraph(plain(10, "正文"))];
    let first = vec![Block::Paragraph(plain(11, "首页"))];
    let mut sections = one_section(2000, 200);
    sections[0].header = Some(header);
    sections[0].first_header = Some(first);
    sections[0].title_page = true;
    let out = layout(
        &looked(
            vec![
                Block::Paragraph(plain(1, "字")),
                Block::Paragraph(plain(2, "字")),
            ],
            sections,
        ),
        &FixedShaper,
    );
    assert_eq!(out.pages.len(), 2);
    let has = |page: usize, text: &str| {
        out.pages[page].items.iter().any(|i| {
            matches!(
                i,
                Item::Text { text: t, .. } if t == text
            )
        })
    };
    assert!(has(0, "首页"));
    assert!(has(1, "正文"));
}

#[test]
fn page_and_num_pages_fields_show_the_page_numbers() {
    let field = |id, code: &str| {
        Inline::Atom(Atom {
            id,
            kind: AtomKind::Field(code.into()),
            look: Default::default(),
        })
    };
    let header = vec![Block::Paragraph(Para {
        id: 10,
        style: ParaStyle::Normal,
        align: Align::Left,
        inlines: vec![
            field(11, "PAGE"),
            Inline::Text {
                text: "/".into(),
                marks: Marks::default(),
            },
            field(12, "NUMPAGES"),
        ],
        locked: false,
        look: ParaLook::default(),
    })];
    let mut sections = one_section(2000, 200);
    sections[0].header = Some(header);
    let out = layout(
        &looked(
            vec![
                Block::Paragraph(plain(1, "字")),
                Block::Paragraph(plain(2, "字")),
            ],
            sections,
        ),
        &FixedShaper,
    );
    assert_eq!(out.pages.len(), 2);
    let text = |page: usize| -> String {
        out.pages[page]
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    assert!(text(0).contains('1'));
    assert!(text(0).contains('2'));
    assert!(text(1).contains('2'));
}

#[test]
fn a_taller_header_moves_the_body_down() {
    let header = vec![Block::Paragraph(plain(10, "一二三"))];
    let mut sections = one_section(200, 2000);
    sections[0].page.top = 200;
    sections[0].page.header = 0;
    sections[0].header = Some(header);
    let out = layout(
        &looked(vec![Block::Paragraph(plain(1, "字"))], sections),
        &FixedShaper,
    );
    let body = out.lines.iter().find(|l| l.para == 1).unwrap();
    assert!(
        close(body.top, 30.0),
        "{} {:?}",
        body.top,
        out.lines
            .iter()
            .map(|l| (l.para, l.page, l.top, l.height, l.furniture))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_section_can_restart_page_numbers() {
    let mut sections = one_section(2000, 200);
    sections[0].last_block = Some(2);
    sections[0].new_page = true;
    let mut second = Section {
        page: sections[0].page.clone(),
        new_page: true,
        page_start: Some(5),
        ..Default::default()
    };
    second.last_block = None;
    sections.push(second);
    let out = layout(
        &looked(
            vec![
                Block::Paragraph(plain(1, "一")),
                Block::Paragraph(plain(2, "二")),
                Block::Paragraph(plain(3, "三")),
                Block::Paragraph(plain(4, "四")),
            ],
            sections,
        ),
        &FixedShaper,
    );
    assert_eq!(out.pages.len(), 4);
    assert_eq!(
        out.pages.iter().map(|p| p.number).collect::<Vec<_>>(),
        [1, 2, 5, 6]
    );
}

fn float_image(id: u64, wrap: Wrap, x: i32, y: i32) -> Inline {
    Inline::Atom(Atom {
        id,
        kind: AtomKind::Image(None),
        look: AtomLook {
            size: Some((400, 400)),
            float: Some(Float {
                x,
                from_x: Origin::Margin,
                y,
                from_y: Origin::Margin,
                wrap,
                clear: [0; 4],
            }),
            shown: None,
        },
    })
}

#[test]
fn square_wrap_shortens_the_lines_beside_a_float() {
    let mut p = plain(1, "");
    p.inlines = vec![
        float_image(20, Wrap::Square, 0, 0),
        Inline::Text {
            text: "一二三四五六七八九十".into(),
            marks: Marks {
                size: Some(200),
                ..Default::default()
            },
        },
    ];
    let out = layout(
        &looked(vec![Block::Paragraph(p)], one_section(2000, 2000)),
        &FixedShaper,
    );
    assert_eq!(out.lines[0].range.end, 27);
    assert!(out.lines[0].stops[0].1 >= 19.9);
    assert!(
        out.pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Picture { rect, .. } if close(rect.width, 20.0)))
    );
}

#[test]
fn top_and_bottom_wrap_pushes_the_text_below() {
    let mut p = plain(1, "");
    p.inlines = vec![
        float_image(20, Wrap::TopAndBottom, 0, 0),
        Inline::Text {
            text: "字".into(),
            marks: Marks {
                size: Some(200),
                ..Default::default()
            },
        },
    ];
    let out = layout(
        &looked(vec![Block::Paragraph(p)], one_section(2000, 2000)),
        &FixedShaper,
    );
    let body = out.lines.iter().find(|l| l.para == 1).unwrap();
    assert!(body.top >= 19.9);
}

#[test]
fn behind_and_in_front_floats_take_no_space_and_are_drawn_in_order() {
    let behind = {
        let mut p = plain(1, "");
        p.inlines = vec![
            float_image(20, Wrap::Behind, 0, 0),
            Inline::Text {
                text: "字".into(),
                marks: Marks::default(),
            },
        ];
        layout(
            &looked(vec![Block::Paragraph(p)], one_section(2000, 2000)),
            &FixedShaper,
        )
    };
    assert!(matches!(
        behind.pages[0].items.first(),
        Some(Item::Picture { .. })
    ));

    let front = {
        let mut p = plain(1, "");
        p.inlines = vec![
            float_image(20, Wrap::InFront, 0, 0),
            Inline::Text {
                text: "字".into(),
                marks: Marks::default(),
            },
        ];
        layout(
            &looked(vec![Block::Paragraph(p)], one_section(2000, 2000)),
            &FixedShaper,
        )
    };
    assert!(matches!(
        front.pages[0].items.last(),
        Some(Item::Picture { .. })
    ));
}

#[test]
fn a_footnote_is_drawn_at_the_page_bottom_above_a_separator() {
    let mut p = plain(1, "");
    p.inlines = vec![
        Inline::Text {
            text: "字".into(),
            marks: Marks {
                size: Some(200),
                ..Default::default()
            },
        },
        Inline::Atom(Atom {
            id: 50,
            kind: AtomKind::Footnote,
            look: AtomLook::default(),
        }),
    ];
    let note = Footnote {
        id: 50,
        blocks: vec![Block::Paragraph(plain(60, "注"))],
    };
    let doc = looked(vec![Block::Paragraph(p)], one_section(2000, 2000)).with_footnotes(vec![note]);
    let out = layout(&doc, &FixedShaper);
    assert!(out.lines.iter().any(|l| l.para == 60));
    assert!(out.pages[0].items.iter().any(|i| matches!(
        i,
        Item::Text { text, .. } if text == "注"
    )));
    assert!(out.pages[0].items.iter().any(|i| matches!(
        i,
        Item::Text { text, .. } if text == "1"
    )));
    assert!(
        out.pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Rule { .. }))
    );
}
