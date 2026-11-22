//! End-to-end checks on small real files: pages, grids, search, cache and
//! the failure modes.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use atrium_preview::{
    Ask, CellAlign, Fonts, Kind, Layout, Preview, Previewer, Refused, kind, probe,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn work_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("atrium-preview-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn previewer(dir: &Path) -> Previewer {
    Previewer::new(dir.join("cache"), Fonts::system())
}

fn ask(width: u32, pages: std::ops::Range<usize>) -> Ask {
    Ask {
        width_px: width,
        pages,
        sheet: None,
        layout: Layout::Grid,
    }
}

fn pages(preview: &Preview) -> (&[atrium_preview::Page], usize, &[String]) {
    match preview {
        Preview::Pages {
            pages,
            total,
            warnings,
        } => (pages, *total, warnings),
        Preview::Table { .. } => panic!("expected pages, got a table"),
    }
}

fn table(preview: &Preview) -> (&[String], usize, &[Vec<atrium_preview::Cell>], bool) {
    match preview {
        Preview::Table {
            sheets,
            current,
            rows,
            more_rows,
        } => (sheets, *current, rows, *more_rows),
        Preview::Pages { .. } => panic!("expected a table, got pages"),
    }
}

/// PNG width and whether any pixel is dark enough to be text.
fn png_stats(path: &Path) -> (u32, u32, bool) {
    let bytes = fs::read(path).unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    let mut dark = false;
    for pixel in buffer[..info.buffer_size()].chunks(4) {
        if pixel[0] < 120 && pixel[1] < 120 && pixel[2] < 120 {
            dark = true;
            break;
        }
    }
    (info.width, info.height, dark)
}

#[test]
fn clamp_kinds() {
    assert_eq!(kind(&fixture("weekly.docx")), Kind::Word);
    assert_eq!(kind(&fixture("slides.pptx")), Kind::Slides);
    assert_eq!(kind(&fixture("workbook.xlsx")), Kind::Sheet);
    assert_eq!(kind(&fixture("gbk.csv")), Kind::Csv);
    assert_eq!(kind(&fixture("two-page.pdf")), Kind::Pdf);
}

#[test]
fn word_docx_renders_pages_and_finds_chinese() {
    let dir = work_dir("word");
    let previewer = previewer(&dir);
    let preview = previewer
        .preview(&fixture("weekly.docx"), &ask(600, 0..20))
        .expect("weekly.docx should render");
    let (pages, total, _) = pages(&preview);
    assert!(total >= 2, "two pages expected, got {total}");
    assert_eq!(pages.len(), total, "all asked pages should be drawn");
    for page in pages {
        let (width, height, dark) = png_stats(&page.png);
        assert_eq!(width, 600, "page {} is not 600 px wide", page.index);
        assert!(height > 100);
        assert!(dark, "page {} has no dark text pixels", page.index);
        assert!(page.width_pt > 100.0 && page.height_pt > 100.0);
    }
    let hits = previewer
        .find(&fixture("weekly.docx"), "周报")
        .expect("search should work");
    assert!(!hits.is_empty(), "周报 was not found in the rendered pages");
    let [x, y, w, h] = hits[0].rect_pt;
    assert!(w > 0.0 && h > 0.0 && x >= -1.0 && y >= -1.0);
    assert!(hits[0].page < total);
}

#[test]
fn pptx_has_one_page_per_slide() {
    let dir = work_dir("slides");
    let preview = previewer(&dir)
        .preview(&fixture("slides.pptx"), &ask(600, 0..20))
        .expect("slides.pptx should render");
    let (pages, total, _) = pages(&preview);
    assert_eq!(total, 3);
    assert_eq!(pages.len(), 3);
    for page in pages {
        assert_eq!(png_stats(&page.png).0, 600);
    }
}

#[test]
fn sheet_grid_keeps_spans_dates_and_currency() {
    let dir = work_dir("sheet");
    let previewer = previewer(&dir);
    let preview = previewer
        .preview(&fixture("workbook.xlsx"), &ask(600, 0..1))
        .expect("workbook.xlsx should open as a grid");
    let (sheets, current, rows, _) = table(&preview);
    assert_eq!(sheets, ["销售数据", "汇总"]);
    assert_eq!(current, 0);
    assert_eq!(rows[0][0].span, (1, 4));
    assert_eq!(rows[0][0].text, "销售周报");
    assert_eq!(rows[1][0].text, "产品");
    assert!(rows[1][0].bold);
    assert_eq!(rows[1][1].align, CellAlign::Center);
    assert_eq!(rows[2][0].text, "键盘");
    assert_eq!(rows[2][1].text, "¥1,234.50");
    assert_eq!(rows[2][1].align, CellAlign::Right);
    assert_eq!(rows[2][3].text, "2026-11-16");

    let second = Ask {
        sheet: Some(1),
        ..ask(600, 0..1)
    };
    let preview = previewer
        .preview(&fixture("workbook.xlsx"), &second)
        .expect("the second sheet should open");
    let (_, current, rows, _) = table(&preview);
    assert_eq!(current, 1);
    assert_eq!(rows[0][0].text, "合计");
}

#[test]
fn xlsx_pages_print_layout() {
    let dir = work_dir("sheet-pages");
    let ask = Ask {
        layout: Layout::Pages,
        ..ask(600, 0..20)
    };
    let preview = previewer(&dir)
        .preview(&fixture("workbook.xlsx"), &ask)
        .expect("xlsx should render as pages");
    let (pages, total, _) = pages(&preview);
    assert!(total >= 1);
    assert!(!pages.is_empty());
    assert_eq!(png_stats(&pages[0].png).0, 600);
}

#[test]
fn csv_decodes_gbk_and_bom() {
    let dir = work_dir("csv");
    let previewer = previewer(&dir);
    let gbk = previewer
        .preview(&fixture("gbk.csv"), &ask(600, 0..1))
        .expect("GBK csv should open");
    let (_, _, rows, _) = table(&gbk);
    assert_eq!(rows[0][0].text, "产品");
    assert_eq!(rows[1][0].text, "键盘");
    assert_eq!(rows[1][2].text, "中文编码");

    let bom = previewer
        .preview(&fixture("bom.csv"), &ask(600, 0..1))
        .expect("BOM csv should open");
    let (_, _, rows, _) = table(&bom);
    assert_eq!(rows[0][0].text, "产品");
    assert_eq!(rows[1][0].text, "键盘");
}

#[test]
fn csv_rows_are_capped() {
    let dir = work_dir("csv-big");
    let path = dir.join("big.csv");
    let mut text = String::from("a,b,c\n");
    for row in 0..20_000 {
        text.push_str(&format!("{row},{row},{row}\n"));
    }
    fs::write(&path, text).unwrap();
    let preview = previewer(&dir)
        .preview(&path, &ask(600, 0..1))
        .expect("big csv should open");
    let (_, _, rows, more) = table(&preview);
    assert_eq!(rows.len(), 10_000);
    assert!(more);
}

#[test]
fn cache_reuses_until_the_file_changes() {
    let dir = work_dir("cache");
    let path = dir.join("weekly.docx");
    fs::copy(fixture("weekly.docx"), &path).unwrap();
    let previewer = previewer(&dir);
    let before = probe::conversions_of(&path);
    previewer.preview(&path, &ask(400, 0..1)).unwrap();
    assert_eq!(probe::conversions_of(&path), before + 1);
    previewer.preview(&path, &ask(400, 0..1)).unwrap();
    assert_eq!(
        probe::conversions_of(&path),
        before + 1,
        "second call should hit the cache"
    );

    // A different width draws new pages but must not convert again.
    previewer.preview(&path, &ask(500, 0..1)).unwrap();
    assert_eq!(
        probe::conversions_of(&path),
        before + 1,
        "a wider page should only redraw"
    );

    let later = SystemTime::now() + Duration::from_secs(3);
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(later))
        .unwrap();
    previewer.preview(&path, &ask(400, 0..1)).unwrap();
    assert_eq!(
        probe::conversions_of(&path),
        before + 2,
        "a new file version converts again"
    );
}

#[test]
fn truncated_docx_is_refused_without_panicking() {
    let dir = work_dir("broken");
    let result = previewer(&dir).preview(&fixture("truncated.docx"), &ask(400, 0..1));
    assert!(
        matches!(result, Err(Refused::Damaged) | Err(Refused::Failed(_))),
        "got {result:?}"
    );
}

#[test]
fn a_file_over_hundred_megabytes_is_too_big() {
    let dir = work_dir("huge");
    let path = dir.join("huge.pdf");
    let file = fs::File::create(&path).unwrap();
    file.set_len(101 * 1024 * 1024).unwrap();
    let result = previewer(&dir).preview(&path, &ask(400, 0..1));
    assert!(matches!(result, Err(Refused::TooBig)), "got {result:?}");
}

#[test]
fn encrypted_pdf_asks_for_a_password() {
    let dir = work_dir("encrypted");
    let result = previewer(&dir).preview(&fixture("encrypted.pdf"), &ask(400, 0..1));
    assert!(matches!(result, Err(Refused::Password)), "got {result:?}");
}

#[test]
fn zip_bomb_is_too_big() {
    let dir = work_dir("bomb");
    let result = previewer(&dir).preview(&fixture("bomb.docx"), &ask(400, 0..1));
    assert!(matches!(result, Err(Refused::TooBig)), "got {result:?}");
}

#[test]
fn pdf_pages_and_search() {
    let dir = work_dir("pdf");
    let previewer = previewer(&dir);
    let preview = previewer
        .preview(&fixture("two-page.pdf"), &ask(500, 0..20))
        .expect("two-page.pdf should render");
    let (pages, total, warnings) = pages(&preview);
    assert_eq!(total, 2);
    assert_eq!(pages.len(), 2);
    assert!(warnings.is_empty());
    let hits = previewer
        .find(&fixture("two-page.pdf"), "page two")
        .expect("search should work");
    assert_eq!(hits.len(), 1, "got {hits:?}");
    assert_eq!(hits[0].page, 1);
    assert!(hits[0].rect_pt[2] > 0.0);
}

#[test]
fn one_call_asks_for_at_most_twenty_pages() {
    let dir = work_dir("cap");
    let preview = previewer(&dir)
        .preview(&fixture("two-page.pdf"), &ask(300, 0..20))
        .unwrap();
    let (page_list, total, _) = pages(&preview);
    assert_eq!(total, 2);
    assert_eq!(page_list.len(), 2);
    let empty = previewer(&dir)
        .preview(&fixture("two-page.pdf"), &ask(300, 0..0))
        .unwrap();
    assert!(pages(&empty).0.is_empty());
}

/// Release-mode timings for the PR. Run with `cargo test --release -- --ignored`.
#[test]
#[ignore]
fn timings() {
    let dir = work_dir("timings");
    for name in [
        "weekly.docx",
        "slides.pptx",
        "workbook.xlsx",
        "two-page.pdf",
    ] {
        let start = std::time::Instant::now();
        let preview = previewer(&dir)
            .preview(&fixture(name), &ask(800, 0..3))
            .unwrap();
        let pages = match &preview {
            Preview::Pages { pages, .. } => pages.len(),
            Preview::Table { .. } => 0,
        };
        println!("{name}: {:?} ({pages} pages)", start.elapsed());
    }
}
