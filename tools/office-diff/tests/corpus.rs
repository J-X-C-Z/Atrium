//! Generates the corpus and lays each file out with Atrium. This is the
//! smoke test the tool itself runs before LibreOffice is involved; it skips
//! cleanly when Python is not installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use atrium_layout::Item;
use office_diff::atrium;
use office_diff::lo;
use office_diff::shaper::SystemShaper;

fn generate(out: &Path) -> bool {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/generate.py");
    if !script.is_file() {
        return false;
    }
    Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg("--out")
        .arg(out)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
fn the_corpus_opens_and_lays_out() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("office-diff-corpus");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    if !generate(&dir) {
        eprintln!("skipping: python3 could not run the corpus generator");
        return;
    }
    let shaper = SystemShaper::new();
    let mut seen = 0;
    for name in [
        "word-bilingual.docx",
        "slides-shapes.pptx",
        "sheet-formats.xlsx",
    ] {
        let path = dir.join(name);
        assert!(path.is_file(), "{name} was not generated");
        let laid = atrium::load(&path, &shaper).unwrap_or_else(|err| panic!("{name}: {err:#}"));
        assert!(!laid.pages.is_empty(), "{name} has no pages");
        assert!(
            laid.tokens.iter().any(|page| !page.is_empty()),
            "{name} has no text tokens"
        );
        for page in &laid.pages {
            assert!(page.width_pt > 1.0 && page.height_pt > 1.0);
            assert!(!page.items.is_empty(), "{name} has an empty page");
        }
        if name != "sheet-formats.xlsx" {
            assert!(
                laid.pages
                    .iter()
                    .flat_map(|p| &p.items)
                    .any(|item| matches!(
                        item,
                        Item::Picture { path: Some(path), .. } if path.is_file()
                    )),
                "{name}: every picture path dangles"
            );
        }
        seen += 1;
    }
    assert_eq!(seen, 3);
}

/// The LibreOffice path has two halves: the `soffice` conversion, which
/// needs LibreOffice, and turning its PDF into pages and text positions,
/// which does not. This exercises the second half on a known PDF so a
/// PDF-side regression is caught on machines without LibreOffice.
#[test]
fn a_known_pdf_yields_pages_and_text_positions() {
    let pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/atrium-preview/tests/fixtures/two-page.pdf");
    if !pdf.is_file() {
        eprintln!("skipping: {} is not present", pdf.display());
        return;
    }
    let tokens = lo::text_tokens(&pdf).expect("read the PDF's text");
    assert!(!tokens.is_empty(), "the PDF has no pages");
    let words: Vec<_> = tokens.iter().flatten().collect();
    assert!(!words.is_empty(), "the PDF has no text");
    for word in &words {
        assert!(!word.text.trim().is_empty());
        assert!(word.x.is_finite() && word.y.is_finite() && word.size > 0.0);
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("office-diff-pdf");
    std::fs::create_dir_all(&dir).unwrap();
    let pages = lo::pages(&pdf, &dir, 600).expect("rasterize the PDF");
    assert_eq!(pages.len(), tokens.len());
    for page in &pages {
        assert!(page.png.is_file(), "no PNG for a page");
        assert!(page.width_pt > 1.0 && page.height_pt > 1.0);
    }
}
