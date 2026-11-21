use atrium_office::{Op, SaveError, SheetOp, open};
use std::{fs, path::Path};

fn worksheet(path: &Path) -> Vec<u8> {
    let bytes = include_bytes!("fixtures/writer-corpus/python/atrium-sheet.xlsx").to_vec();
    fs::write(path, &bytes).unwrap();
    bytes
}
fn set(text: &str) -> Op {
    Op::Sheet(SheetOp::SetCell {
        sheet: 0,
        r: 0,
        c: 0,
        input: text.to_owned(),
    })
}
fn text_from_xlsx(path: &Path) -> String {
    let doc = open(path).unwrap();
    let atrium_office::Office::Sheet(model) = doc.office else {
        panic!("not a workbook")
    };
    model.sheets()[0].cells[&(0, 0)].display.clone()
}
#[test]
fn conflict_copy_and_force_preserve_their_distinct_contracts() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("source.xlsx");
    worksheet(&path);
    let mut doc = open(&path).unwrap();
    doc.apply(set("待保存")).unwrap();
    let previous = doc.hash.clone();
    fs::write(&path, b"external edit").unwrap();
    assert_eq!(doc.save(), Err(SaveError::ChangedOnDisk));
    assert!(doc.is_dirty());
    assert_eq!(doc.hash, previous);
    let exists = d.path().join("existing.xlsx");
    fs::write(&exists, b"must not overwrite").unwrap();
    assert!(doc.save_as(&exists).is_err());
    assert_eq!(fs::read(&exists).unwrap(), b"must not overwrite");
    assert!(doc.is_dirty());

    let copy = d.path().join("copy.xlsx");
    doc.save_as(&copy).unwrap();
    assert_eq!(text_from_xlsx(&copy), "待保存");
    assert_eq!(doc.path, copy);
    assert!(!doc.is_dirty());
    assert_eq!(fs::read(&path).unwrap(), b"external edit");
    let hash = doc.hash.clone();
    doc.apply(set("明确覆盖")).unwrap();
    fs::write(&copy, b"other writer").unwrap();
    doc.hash = hash.clone();
    assert_eq!(doc.save(), Err(SaveError::ChangedOnDisk));
    doc.save_over().unwrap();
    assert_eq!(text_from_xlsx(&copy), "明确覆盖");
    assert_ne!(doc.hash, hash);
    assert!(!doc.is_dirty());
}
#[test]
fn reversed_public_cell_edit_saves_original_zip_exactly() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("source.xlsx");
    let bytes = worksheet(&path);
    let mut doc = open(&path).unwrap();
    let inverse = doc.apply(set("can undo")).unwrap();
    doc.apply(inverse).unwrap();
    doc.hash = "display field is not an authority".into();
    doc.save().unwrap();
    assert_eq!(doc.hash.len(), 64);
    assert!(!doc.is_dirty());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn ooxml_corpus_opens_through_editor_and_saves_byte_identical() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let d = tempfile::tempdir().unwrap();
    let mut count = 0;
    for sub in [
        "",
        "writer-corpus/python",
        "writer-corpus/libreoffice",
        "office2pdf-golden",
        "array",
    ] {
        for input in fs::read_dir(root.join(sub))
            .unwrap()
            .map(|e| e.unwrap().path())
        {
            let Some(ext) = input.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if !matches!(ext, "docx" | "xlsx" | "pptx") {
                continue;
            }
            let bytes = fs::read(&input).unwrap();
            let to = d.path().join(format!("copy-{count}.{ext}"));
            let mut doc = open(&input).unwrap_or_else(|e| panic!("{input:?}: {e}"));
            doc.save_as(&to).unwrap();
            assert_eq!(fs::read(to).unwrap(), bytes, "{input:?}");
            count += 1;
        }
    }
    assert_eq!(count, 11);
}
