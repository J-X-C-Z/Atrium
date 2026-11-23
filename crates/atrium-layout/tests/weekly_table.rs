use atrium_layout::{FixedShaper, Item, layout};
use std::path::Path;

fn weekly() -> atrium_office::WordDoc {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../atrium-preview/tests/fixtures/weekly.docx");
    let doc = atrium_office::open(&path).unwrap();
    let atrium_office::Office::Word(word) = doc.office else {
        panic!("not a word document");
    };
    word
}

fn baseline_of(items: &[Item], needle: &str) -> f32 {
    items
        .iter()
        .find_map(|i| match i {
            Item::Text { baseline, text, .. } if text.contains(needle) => Some(*baseline),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text {needle:?}"))
}

/// Table rows are placed in order inside the table; the paragraph and the
/// picture that follow sit below its bottom edge.
#[test]
fn weekly_table_rows_stay_inside_the_table() {
    let l = layout(&weekly(), &FixedShaper);
    let items = &l.pages[0].items;
    let rows: Vec<f32> = ["负责人", "小林", "小陈"]
        .iter()
        .map(|t| baseline_of(items, t))
        .collect();
    assert!(rows[0] < rows[1] && rows[1] < rows[2], "{rows:?}");
    // The table's rules span its top and bottom edges.
    let ys: Vec<f32> = items
        .iter()
        .filter_map(|i| match i {
            Item::Rule { from, to, .. } if from.0 != to.0 => Some(from.1),
            _ => None,
        })
        .collect();
    let top = ys.iter().cloned().fold(f32::MAX, f32::min);
    let bottom = ys.iter().cloned().fold(f32::MIN, f32::max);
    assert!(
        rows[0] > top && rows[2] < bottom,
        "{rows:?} in {top}..{bottom}"
    );
    // Each cell of a row shares its row's baseline.
    assert_eq!(baseline_of(items, "小陈"), rows[2]);
    assert_eq!(baseline_of(items, "进行中"), rows[2]);
    let after = baseline_of(items, "下图是预览页面");
    assert!(after > bottom, "{after} below {bottom}");
    let picture = items
        .iter()
        .find_map(|i| match i {
            Item::Picture { rect, .. } => Some(rect.y),
            _ => None,
        })
        .expect("picture");
    assert!(picture > after, "picture {picture} below paragraph {after}");
}
