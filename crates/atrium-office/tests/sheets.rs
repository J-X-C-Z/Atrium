use atrium_office::{
    Office, Op, SheetOp, a1,
    look::{CellStyle, CondKind, HAlign, Rgb},
    open,
};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sheets")
        .join(name)
}

fn workbook(name: &str) -> atrium_office::Workbook {
    let doc = open(&fixture(name)).unwrap();
    let Office::Sheet(wb) = doc.office else {
        panic!("not a workbook")
    };
    wb
}

fn parts(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let file = fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        out.insert(entry.name().to_owned(), bytes);
    }
    out
}

fn inside<'a>(text: &'a str, open: &str, close: &str) -> &'a str {
    let start = text.find(open).unwrap() + open.len();
    let end = text[start..].find(close).unwrap() + start;
    &text[start..end]
}

#[test]
fn styles_layout_cells_and_drawings_are_read() {
    let doc = open(&fixture("excel-style.xlsx")).unwrap();
    let Office::Sheet(wb) = &doc.office else {
        panic!("not a workbook")
    };
    let s = &wb.sheets()[0];
    assert_eq!(s.name, "预算");
    assert_eq!(s.merged, vec![(0, 0, 0, 2)]);

    let header = wb.styles()[1].clone();
    assert!(header.font.bold);
    assert_eq!(header.font.color, Some(Rgb(0xFF0000)));
    assert!(header.font.fonts.latin.contains("宋体"));
    assert_eq!(header.fill, Some(Rgb(0x8FAADC)));
    assert!(header.borders.top.is_some() && header.borders.bottom.is_some());
    assert_eq!(header.h_align, HAlign::Center);
    assert!(header.wrap);

    assert_eq!(s.cells[&(0, 1)].value, atrium_office::Value::Empty);
    assert_eq!(s.cells[&(0, 1)].style, 1);
    assert_eq!(wb.styles()[2].number_format, "0.00%");
    assert_eq!(wb.styles()[9].number_format, "#,##0.00");
    assert_eq!(wb.styles()[3].number_format, "yyyy\"年\"m\"月\"d\"日\"");
    assert_eq!(wb.styles()[4].number_format, "yyyy\"年\"m\"月\"d\"日\"");

    let header_row = &s.look.row_styles[&4];
    assert_eq!(*header_row, 1);
    assert!(s.look.hidden_rows.contains(&2));
    assert!(s.look.hidden_rows.contains(&8));
    assert_eq!(s.look.rows[&0], 24.0);
    assert!(s.look.hidden_columns.contains(&2));
    assert!((s.look.columns[&0] - 108.73).abs() < 0.2);
    assert!((s.look.default_width - 47.99).abs() < 0.2);
    assert_eq!(s.look.default_height, 15.0);
    assert_eq!(s.look.frozen, (1, 1));
    assert!(!s.look.gridlines);
    assert_eq!(s.look.zoom, 125);
    assert!(s.look.tab_color.is_some());

    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::CellIs(_)))
    );
    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::Contains(ref t) if t == "明"))
    );
    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::Top { n: 1, .. }))
    );
    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::AboveAverage { below: false }))
    );
    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::Duplicate { unique: false }))
    );
    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::Duplicate { unique: true }))
    );
    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::ColorScale(_)))
    );
    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::DataBar(_)))
    );
    assert!(
        s.look
            .conditional
            .iter()
            .any(|r| matches!(r.kind, CondKind::Other(_)))
    );

    assert_eq!(s.look.drawings.len(), 3);
    let picture = s
        .look
        .drawings
        .iter()
        .find_map(|d| match &d.what {
            atrium_office::look::Drawing::Picture(path) => Some(path),
            _ => None,
        })
        .unwrap();
    assert!(picture.exists());
    let chart = s
        .look
        .drawings
        .iter()
        .find_map(|d| match &d.what {
            atrium_office::look::Drawing::Chart(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(chart.title.as_deref(), Some("季度"));
    assert_eq!(chart.series.len(), 1);
    assert_eq!(
        chart.series[0].values,
        vec![Some(10.0), Some(20.0), Some(30.0)]
    );
    let shape = s
        .look
        .drawings
        .iter()
        .find_map(|d| match &d.what {
            atrium_office::look::Drawing::Shape(sh) => Some(sh),
            _ => None,
        })
        .unwrap();
    assert_eq!(shape.paras[0].text(), "备注");
    assert_eq!(a1(0, 1), "B1");
}

#[test]
fn shown_applies_formats_and_conditional_rules() {
    let wb = workbook("excel-style.xlsx");
    assert_eq!(wb.shown(0, 1, 0).text, "25.00%");
    assert_eq!(wb.shown(0, 1, 1).text, "1,234.50");
    assert_eq!(wb.shown(0, 1, 2).text, "1.25E-04");
    assert_eq!(wb.shown(0, 2, 0).text, "1 1/4");
    assert_eq!(wb.shown(0, 2, 1).text, "2024年3月5日");
    assert_eq!(wb.shown(0, 2, 2).text, "3:00 PM");
    assert_eq!(wb.shown(0, 2, 3).text, "1,234.50");
    assert_eq!(wb.shown(0, 3, 0).text, "2024年3月5日");
    assert_eq!(wb.shown(0, 3, 1).text, "-12.50");
    assert_eq!(wb.shown(0, 3, 1).style.font.color, Some(Rgb(0xFF0000)));
    assert_eq!(wb.shown(0, 3, 2).text, "¥1,234.50");
    assert_eq!(wb.shown(0, 3, 3).text, "$13");

    assert!(wb.shown(0, 4, 0).style.font.bold);
    assert_eq!(wb.shown(0, 0, 1).style.fill, Some(Rgb(0x8FAADC)));

    // cellIs D8 > 0: green fill from dxf 0.
    assert_eq!(wb.shown(0, 7, 3).style.fill, Some(Rgb(0xC6EFCE)));
    // containsText A6 "明": red bold font from dxf 1.
    assert_eq!(wb.shown(0, 5, 0).style.font.color, Some(Rgb(0x9C0006)));
    assert_eq!(wb.shown(0, 5, 0).style.font.fonts.latin, "Calibri");
    assert_eq!(wb.shown(0, 5, 0).style.font.size, Some(220));
    // top10 B6: yellow fill.
    assert_eq!(wb.shown(0, 5, 1).style.fill, Some(Rgb(0xFFEB9C)));
    // aboveAverage B8.
    assert_eq!(wb.shown(0, 7, 1).style.fill, Some(Rgb(0xF4B084)));
    // duplicate A8.
    assert_eq!(wb.shown(0, 7, 0).style.font.color, Some(Rgb(0x0000FF)));
    // unique C8.
    assert_eq!(wb.shown(0, 7, 2).style.fill, Some(Rgb(0xBDD7EE)));
    // colour scale low value is the first stop.
    assert_eq!(wb.shown(0, 6, 0).style.fill, Some(Rgb(0xF8696B)));
    // data bars: D7 is the minimum, D8 the maximum.
    assert_eq!(wb.shown(0, 6, 3).bar, Some((0.0, Rgb(0x638EC6))));
    assert_eq!(wb.shown(0, 7, 3).bar, Some((1.0, Rgb(0x638EC6))));
    // Unknown rules never apply.
    assert_eq!(wb.shown(0, 7, 4).style.fill, None);
}

#[test]
fn editing_a_styled_cell_keeps_its_style_and_format() {
    let mut doc = open(&fixture("excel-style.xlsx")).unwrap();
    doc.apply(Op::Sheet(SheetOp::SetCell {
        sheet: 0,
        r: 1,
        c: 0,
        input: "0.5".into(),
    }))
    .unwrap();
    let Office::Sheet(wb) = &doc.office else {
        panic!("not a workbook")
    };
    let cell = &wb.sheets()[0].cells[&(1, 0)];
    assert_eq!(cell.style, 2);
    assert_eq!(wb.shown(0, 1, 0).text, "50.00%");
}

#[test]
fn formatting_sizes_and_panes_round_trip_and_only_add_style_records() {
    let dir = tempfile::tempdir().unwrap();
    let source = fixture("excel-style.xlsx");
    let copy = dir.path().join("edited.xlsx");
    fs::copy(&source, &copy).unwrap();
    let original_parts = parts(&source);
    let original_styles = String::from_utf8(original_parts["xl/styles.xml"].clone()).unwrap();

    let mut doc = open(&copy).unwrap();
    let Office::Sheet(ref mut wb) = doc.office else {
        panic!("not a workbook")
    };
    let mut style = CellStyle {
        fill: Some(Rgb(0x00B0F0)),
        number_format: "0.000".into(),
        ..Default::default()
    };
    style.font.bold = true;
    let style = wb.add_style(style);
    doc.apply(Op::Sheet(SheetOp::SetStyles {
        sheet: 0,
        from: (0, 0),
        to: (1, 1),
        style,
    }))
    .unwrap();
    doc.apply(Op::Sheet(SheetOp::SetColumnWidth {
        sheet: 0,
        column: 3,
        width: 120.0,
    }))
    .unwrap();
    doc.apply(Op::Sheet(SheetOp::SetRowHeight {
        sheet: 0,
        row: 2,
        height: 40.0,
    }))
    .unwrap();
    doc.apply(Op::Sheet(SheetOp::SetFrozen {
        sheet: 0,
        rows: 2,
        cols: 1,
    }))
    .unwrap();
    doc.save().unwrap();

    let reopened = workbook_path(&copy);
    assert_eq!(reopened.shown(0, 1, 0).text, "0.250");
    assert_eq!(reopened.shown(0, 0, 0).style.fill, Some(Rgb(0x00B0F0)));
    assert!(reopened.shown(0, 0, 0).style.font.bold);
    assert_eq!(reopened.sheets()[0].look.frozen, (2, 1));
    assert!((reopened.sheets()[0].look.columns[&3] - 120.0).abs() < 0.2);
    assert!((reopened.sheets()[0].look.rows[&2] - 40.0).abs() < 0.1);

    let edited_parts = parts(&copy);
    for (name, bytes) in &original_parts {
        if name == "xl/styles.xml" || name == "xl/worksheets/sheet1.xml" {
            continue;
        }
        assert_eq!(&edited_parts[name], bytes, "{name}");
    }
    let edited_styles = String::from_utf8(edited_parts["xl/styles.xml"].clone()).unwrap();
    assert!(edited_styles.contains(inside(&original_styles, "<fonts", "</fonts>")));
    assert!(edited_styles.contains(inside(&original_styles, "<fills", "</fills>")));
    assert!(edited_styles.contains(inside(&original_styles, "<borders", "</borders>")));
    assert!(edited_styles.contains(inside(&original_styles, "<cellXfs", "</cellXfs>")));
    assert!(edited_styles.matches("<xf ").count() > original_styles.matches("<xf ").count());
}

fn workbook_path(path: &Path) -> atrium_office::Workbook {
    let doc = open(path).unwrap();
    let Office::Sheet(wb) = doc.office else {
        panic!("not a workbook")
    };
    wb
}

#[test]
fn undoing_a_style_edit_saves_the_original_archive() {
    let dir = tempfile::tempdir().unwrap();
    let source = fixture("excel-style.xlsx");
    let copy = dir.path().join("undo.xlsx");
    fs::copy(&source, &copy).unwrap();
    let original = fs::read(&copy).unwrap();
    let mut doc = open(&copy).unwrap();
    let Office::Sheet(ref mut wb) = doc.office else {
        panic!("not a workbook")
    };
    let style = CellStyle {
        fill: Some(Rgb(0xFF0000)),
        ..Default::default()
    };
    let style = wb.add_style(style);
    let undo = doc
        .apply(Op::Sheet(SheetOp::SetStyle {
            sheet: 0,
            r: 0,
            c: 0,
            style,
        }))
        .unwrap();
    doc.apply(undo).unwrap();
    doc.save().unwrap();
    assert_eq!(fs::read(&copy).unwrap(), original);
}

#[test]
fn applying_an_existing_style_reuses_its_records() {
    let dir = tempfile::tempdir().unwrap();
    let source = fixture("excel-style.xlsx");
    let copy = dir.path().join("reuse.xlsx");
    fs::copy(&source, &copy).unwrap();
    let original = parts(&source);
    let original_styles = String::from_utf8(original["xl/styles.xml"].clone()).unwrap();
    let before = original_styles.matches("<xf ").count();
    let mut doc = open(&copy).unwrap();
    doc.apply(Op::Sheet(SheetOp::SetStyle {
        sheet: 0,
        r: 1,
        c: 1,
        style: 1,
    }))
    .unwrap();
    doc.save().unwrap();
    let wb = workbook_path(&copy);
    assert_eq!(wb.shown(0, 1, 1).style.fill, Some(Rgb(0x8FAADC)));
    let after = String::from_utf8(parts(&copy)["xl/styles.xml"].clone()).unwrap();
    assert_eq!(after.matches("<xf ").count(), before);
}

#[test]
fn clearing_a_style_writes_the_default_index() {
    let dir = tempfile::tempdir().unwrap();
    let source = fixture("excel-style.xlsx");
    let copy = dir.path().join("clear.xlsx");
    fs::copy(&source, &copy).unwrap();
    let mut doc = open(&copy).unwrap();
    doc.apply(Op::Sheet(SheetOp::SetStyle {
        sheet: 0,
        r: 1,
        c: 0,
        style: 0,
    }))
    .unwrap();
    doc.save().unwrap();
    let wb = workbook_path(&copy);
    assert_eq!(wb.sheets()[0].cells[&(1, 0)].style, 0);
    assert_eq!(wb.shown(0, 1, 0).text, "0.25");
}

#[test]
fn wps_style_workbook_reads_indexed_colours_and_chinese_dates() {
    let wb = workbook("wps-style.xlsx");
    assert_eq!(wb.styles()[1].fill, Some(Rgb(0xFF0000)));
    assert_eq!(wb.sheets()[0].look.tab_color, Some(Rgb(0x0000FF)));
    assert_eq!(wb.shown(0, 2, 1).text, "2024年3月5日");
    assert_eq!(wb.shown(0, 1, 0).text, "25.00%");
    assert_eq!(wb.sheets()[0].look.drawings.len(), 3);
}
