use atrium_office::look::{ChartKind, Fill, PathCmd, ShapeKind, VText};
use atrium_office::{Office, Op, SlideOp, open};
use std::io::Read;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/slides-full/deck.pptx")
}

#[test]
fn fixture_deck_reads_shapes_groups_tables_and_charts() {
    let doc = open(&fixture()).unwrap();
    let Office::Slides(deck) = &doc.office else {
        panic!("not a deck");
    };
    assert_eq!(deck.slides().len(), 2);
    let slide = &deck.slides()[0];
    let shapes = &slide.shapes;
    let inherited = shapes
        .iter()
        .flat_map(|s| s.text_paras())
        .find(|p| p.text() == "Inherited title")
        .expect("inherited placeholder");
    assert!(inherited.look.mark.size == Some(600));
    assert!(inherited.look.mark.bold && inherited.look.mark.italic);
    assert_eq!(
        inherited.look.mark.color,
        Some(atrium_office::look::Rgb(0x4F81BD))
    );
    assert_eq!(inherited.look.mark.fonts.latin, "Calibri Light");
    let inherited_shape = shapes
        .iter()
        .find(|s| s.text_paras().any(|p| p.text() == "Inherited title"))
        .unwrap();
    assert!(inherited_shape.frame[2] > 0 && inherited_shape.frame[3] > 0);
    assert!(
        shapes
            .iter()
            .any(|s| matches!(s.kind, ShapeKind::Picture) && s.frame[2] > 0),
        "master logo and cropped picture are pictures"
    );
    let pictures: Vec<_> = shapes
        .iter()
        .filter(|s| matches!(s.kind, ShapeKind::Picture))
        .collect();
    assert_eq!(pictures.len(), 2);
    assert!(pictures.iter().any(|s| matches!(
        &s.look.fill,
        Fill::Picture { crop, .. } if *crop == [10000, 20000, 0, 30000]
    )));
    assert!(shapes.iter().any(|s| matches!(
        s.look.fill,
        Fill::Gradient { ref stops, .. } if stops.len() == 2
    )));
    assert!(shapes.iter().any(|s| {
        s.look.path.iter().any(|c| matches!(c, PathCmd::Line(..)))
            && s.look.rotation == 5400000 / 60000
    }));
    let flipped = shapes
        .iter()
        .find(|s| s.look.flip == (true, false))
        .expect("flipped shape");
    assert!(matches!(
        flipped.look.path.first(),
        Some(PathCmd::Move(x, _)) if *x == flipped.frame[2]
    ));
    assert!(
        shapes.iter().any(|s| s
            .look
            .line
            .is_some_and(|l| l.dash != Default::default() && l.tail != Default::default())),
        "connector line with dash and arrow"
    );
    assert!(shapes.iter().any(|s| {
        s.look.path.len() >= 4
            && s.look.path.iter().any(|c| matches!(c, PathCmd::Cubic(..))) == false
            && matches!(s.look.path.last(), Some(PathCmd::Close))
    }));
    let rotated = shapes
        .iter()
        .find(|s| s.look.rotation != 0)
        .expect("rotated shape");
    assert_eq!(rotated.look.vert, VText::Vertical);
    let table = shapes
        .iter()
        .find_map(|s| match &s.kind {
            ShapeKind::Table(t) => Some(t),
            _ => None,
        })
        .expect("table");
    assert_eq!(table.look.columns.len(), 3);
    assert_eq!(table.rows.len(), 3);
    assert!(table.merged.contains(&(0, 1)));
    assert!(table.look.cells[2][0].merged_up);
    assert_eq!(
        table.look.cells[0][0].shading,
        Some(atrium_office::look::Rgb(0x1E2761))
    );
    assert!(
        table.look.cells[1][1].shading.is_some(),
        "the table style's band fills the cell"
    );
    assert!(table.rows[0][0].iter().any(|b| matches!(
        b,
        atrium_office::Block::Paragraph(p)
            if p.inlines.iter().any(|i| matches!(
                i,
                atrium_office::Inline::Text { marks, .. }
                    if marks.bold && marks.color == Some(atrium_office::look::Rgb::WHITE)
            ))
    )));
    let charts: Vec<_> = shapes
        .iter()
        .filter_map(|s| match &s.kind {
            ShapeKind::Chart(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(charts.len(), 2);
    assert_eq!(charts[0].kind, ChartKind::Column);
    assert!(charts[0].data_labels);
    assert_eq!(charts[0].series.len(), 2);
    assert_eq!(
        charts[0].series[0].values,
        [Some(3.0), Some(5.0), Some(2.0)]
    );
    assert_eq!(charts[1].kind, ChartKind::Pie);

    let second = &deck.slides()[1];
    assert!(second.background.is_some());
    assert!(matches!(second.shapes[0].look.fill, Fill::Gradient { .. }));
    let kinds: Vec<_> = second
        .shapes
        .iter()
        .filter_map(|s| match &s.kind {
            ShapeKind::Chart(c) => Some(c.kind.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ChartKind::Line,
            ChartKind::Area,
            ChartKind::Bar,
            ChartKind::Doughnut
        ]
    );
    assert!(
        second
            .shapes
            .iter()
            .any(|s| matches!(&s.kind, ShapeKind::Chart(c) if c.percent && c.stacked))
    );
    assert!(
        second
            .shapes
            .iter()
            .flat_map(|s| s.text_paras())
            .any(|p| p.text() == "SmartArt step")
    );
    assert!(
        second
            .shapes
            .iter()
            .any(|s| { matches!(&s.kind, ShapeKind::Fallback(Some(path)) if path.exists()) })
    );
}

#[test]
fn editing_a_table_cell_changes_only_that_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.pptx");
    std::fs::copy(fixture(), &path).unwrap();
    let mut doc = open(&path).unwrap();
    let Office::Slides(deck) = &doc.office else {
        panic!("not a deck");
    };
    let table = deck.slides()[0]
        .shapes
        .iter()
        .find_map(|s| match &s.kind {
            ShapeKind::Table(t) => Some(t),
            _ => None,
        })
        .unwrap();
    let para = table.rows[1]
        .iter()
        .flatten()
        .filter_map(|b| match b {
            atrium_office::Block::Paragraph(p) => Some(p),
            _ => None,
        })
        .find(|p| p.text() == "B")
        .unwrap()
        .clone();
    let mut inlines = para.inlines.clone();
    if let atrium_office::Inline::Text { text, .. } = &mut inlines[0] {
        *text = "B2".into();
    }
    doc.apply(Op::Slides(SlideOp::SetContent {
        para: para.id,
        inlines,
    }))
    .unwrap();
    let before = slide_xml(&fixture(), 1);
    let out = path.with_file_name("edited.pptx");
    doc.save_as(&out).unwrap();
    let after = slide_xml(&out, 1);
    assert_eq!(after, before.replacen(">B<", ">B2<", 1));
    let reopened = open(&out).unwrap();
    let Office::Slides(deck) = &reopened.office else {
        panic!("not a deck");
    };
    assert!(deck.slides()[0].shapes.iter().any(|s| {
        match &s.kind {
            ShapeKind::Table(t) => t
                .rows
                .iter()
                .flatten()
                .flatten()
                .any(|b| matches!(b, atrium_office::Block::Paragraph(p) if p.text() == "B2")),
            _ => false,
        }
    }));
}

#[test]
fn run_size_and_colour_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.pptx");
    std::fs::copy(fixture(), &path).unwrap();
    let mut doc = open(&path).unwrap();
    let Office::Slides(deck) = &doc.office else {
        panic!("not a deck");
    };
    let para = deck.slides()[0]
        .shapes
        .iter()
        .flat_map(|s| s.text_paras())
        .find(|p| p.text() == "Style")
        .unwrap()
        .clone();
    let mut inlines = para.inlines.clone();
    let atrium_office::Inline::Text { marks, .. } = &mut inlines[0] else {
        panic!("run");
    };
    marks.size = Some(480);
    marks.color = Some(atrium_office::look::Rgb(0x112233));
    doc.apply(Op::Slides(SlideOp::SetContent {
        para: para.id,
        inlines,
    }))
    .unwrap();
    let out = path.with_file_name("styled.pptx");
    doc.save_as(&out).unwrap();
    let reopened = open(&out).unwrap();
    let Office::Slides(deck) = &reopened.office else {
        panic!("not a deck");
    };
    let p = deck.slides()[0]
        .shapes
        .iter()
        .flat_map(|s| s.text_paras())
        .find(|p| p.text() == "Style")
        .unwrap();
    let atrium_office::Inline::Text { marks, .. } = &p.inlines[0] else {
        panic!("run");
    };
    assert_eq!(marks.size, Some(480));
    assert_eq!(marks.color, Some(atrium_office::look::Rgb(0x112233)));
}

fn slide_xml(path: &Path, n: usize) -> String {
    let file = std::fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut part = zip.by_name(&format!("ppt/slides/slide{n}.xml")).unwrap();
    let mut xml = String::new();
    part.read_to_string(&mut xml).unwrap();
    xml
}
