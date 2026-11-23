use atrium_layout::{FixedShaper, Item, Pos, layout_deck, layout_slide};
use atrium_office::look::{ShapeKind, ShapeLook};
use atrium_office::{Deck, Inline, Marks, Para, ParaStyle, Shape, Slide};
use std::path::Path;

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../atrium-office/tests/fixtures/slides-full/deck.pptx")
}

fn deck() -> Deck {
    let doc = atrium_office::open(&fixture()).unwrap();
    let atrium_office::Office::Slides(deck) = doc.office else {
        panic!("not a deck");
    };
    deck
}

#[test]
fn fixture_deck_lays_out_paths_pictures_and_text() {
    let layout = layout_deck(&deck(), &FixedShaper);
    assert_eq!(layout.pages.len(), 2);
    for (i, page) in layout.pages.iter().enumerate() {
        let paths = page
            .items
            .iter()
            .filter(|i| matches!(i, Item::Path { .. }))
            .count();
        let pictures = page
            .items
            .iter()
            .filter(|i| matches!(i, Item::Picture { .. }))
            .count();
        let text = page
            .items
            .iter()
            .filter(|i| matches!(i, Item::Text { .. }))
            .count();
        assert_eq!(
            (paths, pictures, text),
            if i == 0 { (15, 2, 29) } else { (8, 2, 23) }
        );
    }
    assert_eq!(
        layout.pages[0]
            .items
            .iter()
            .filter(|i| matches!(i, Item::Picture { .. }))
            .count(),
        2
    );
    assert!(layout.lines.iter().any(|l| !l.furniture));
    assert!(
        layout.pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Text { rotation, .. } if *rotation != 0.0))
    );
}

#[test]
fn table_cells_are_hit_testable() {
    let deck = deck();
    let table_para = deck.slides()[0]
        .shapes
        .iter()
        .find_map(|s| match &s.kind {
            ShapeKind::Table(t) => Some(t),
            _ => None,
        })
        .unwrap()
        .rows[1]
        .iter()
        .flatten()
        .filter_map(|b| match b {
            atrium_office::Block::Paragraph(p) => Some(p),
            _ => None,
        })
        .find(|p| p.text() == "B")
        .unwrap();
    let layout = layout_deck(&deck, &FixedShaper);
    let line = layout
        .lines
        .iter()
        .find(|l| l.para == table_para.id && !l.furniture)
        .unwrap();
    let hit = layout.hit(0, line.stops[0].1 + 1.0, line.top + line.height / 2.0);
    assert_eq!(hit, Some(Pos::new(table_para.id, line.range.start)));
}

#[test]
fn autofit_and_font_scale_grow_or_shrink_text() {
    let mut para = Para::new(
        1,
        ParaStyle::Normal,
        "一二三四五六七八九十一二三四五六七八九十",
    );
    para.look.mark = Marks::default();
    let shape = Shape {
        id: 2,
        frame: [0, 0, 100, 20],
        paras: vec![para],
        look: ShapeLook {
            auto_grow: true,
            ..ShapeLook::default()
        },
        kind: ShapeKind::Text,
    };
    let slide = Slide {
        hidden: false,
        size: (200, 200),
        shapes: vec![shape],
        background: None,
    };
    let layout = layout_slide(&slide, &FixedShaper);
    assert!(layout.lines.len() > 1);
    let bottom = layout.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text { baseline, .. } => Some(*baseline),
            _ => None,
        })
        .fold(0.0f32, f32::max);
    assert!(
        bottom > 20.0,
        "the shape grew to hold wrapped text: {bottom}"
    );

    let mut tiny = slide.clone();
    tiny.shapes[0].look.auto_grow = false;
    tiny.shapes[0].look.font_scale = Some(500);
    tiny.shapes[0].paras[0].inlines = vec![Inline::Text {
        text: "A".into(),
        marks: Marks::default(),
    }];
    let layout = layout_slide(&tiny, &FixedShaper);
    let size = layout.pages[0]
        .items
        .iter()
        .find_map(|i| match i {
            Item::Text { font, .. } => Some(font.size),
            _ => None,
        })
        .unwrap();
    assert_eq!(size, 9.0);
}
