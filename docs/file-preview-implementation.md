# Building file previews (Word, Excel, PowerPoint, PDF)

A brief for the backend of the side panel's **Preview** tab. It shows a
file an agent made or changed without leaving Atrium: a report, a
spreadsheet, slides, a PDF. Read `AGENTS.md` first.

**Scope: read-only preview.** No editing, no saving, no recalculating
formulas, no running macros. Build only what is needed to draw a file
faithfully enough to check an agent's work.

## The approach: pure Rust by default

Everything runs in-process, in Rust, with no external program. It works the
same on macOS, Windows and Linux.

```
docx / pptx / xlsx ──office2pdf──▶ PDF bytes ──hayro──▶ PNG per page ──▶ UI
pdf ─────────────────────────────▶ PDF bytes ──hayro──▶ PNG per page ──▶ UI
xlsx / xls / ods / csv ──calamine / csv──▶ cell grid ──────────────────▶ UI
```

| Crate | What for | License | Checked |
| --- | --- | --- | --- |
| [`office2pdf`](https://github.com/developer0hye/office2pdf) 0.11 | DOCX, PPTX and XLSX to PDF; lays out through Typst (pure Rust) | Apache-2.0 | yes, see below |
| [`hayro`](https://github.com/LaurenzV/hayro) 0.9 | PDF page to bitmap, pure Rust, no `unsafe` | MIT or Apache-2.0 | yes, see below |
| [`calamine`](https://github.com/tafia/calamine) | xlsx, xls, xlsb and ods cells | MIT | widely used |
| `csv` + `encoding_rs` | csv and tsv, with non-UTF-8 encodings | MIT / Apache | — |

**A spike was run on Linux, in release mode, on office2pdf's own fixtures.**
It converted each file to PDF, then rendered the first 3 pages at 1.5×:

| File | Pages | To PDF | Render |
| --- | --- | --- | --- |
| technical manual (docx) | 2 | 217 ms | 20 ms |
| Korean contract (docx) | 1 | 139 ms | 16 ms |
| startup pitch (pptx) | 3 | 405 ms | 62 ms |
| sales dashboard (xlsx) | 1 | 101 ms | 13 ms |

- The rendered pages looked right: headings, lists, code blocks, page
  headers and footers.
- The spike binary was 72 MB after stripping. Most of that is Typst's
  bundled fonts. Use `office2pdf` with `default-features = false`, then
  supply fonts as in §4, and report the size the app grows by in the PR.

Not used:

- **LibreOffice / ElyOffice's LOK engine**: hundreds of MB, and an exact
  version pinned to one build. It may come later as an optional
  "original layout" mode; it is not in this item.
- **MuPDF** (including ElyOffice's `ely-pdf-*` crates): AGPL-3.0, while
  Atrium is MIT.
- **pdf-render / PDFluent**: a commercial fork of hayro.

Other pure-Rust renderers exist and can be tried if `office2pdf` falls
short: `rdocx` (DOCX layout to PDF and PNG, MIT/Apache), `dxpdf` (DOCX, but
uses Skia, which is C++), `ppt-rs` and `zavora-slide` (PPTX). Do not mix
them in v1. If you switch, say why in the PR, with the fixtures that showed it.

## Status

- **`crates/atrium-preview` exists** with the API below; `kind()` is
  implemented and tested.
- **`Previewer::preview` and `Previewer::find` are implemented**:
  office2pdf for DOCX/PPTX/XLSX pages, hayro for PDF pages and search,
  calamine and `csv` for grids, with the cache, size limits and deadline
  described below. Tests use small fixtures in
  `crates/atrium-preview/tests/fixtures/`.
- **The Preview view is built and calls them**:
  - for PDF, CSV, xls and ods;
  - for docx, xlsx and pptx when `atrium_office::open` cannot open them;
  - in the "Pages" view of any of them. A sheet in "Pages" asks with
    `Layout::Pages`.

## What the UI does (built on the UI side, not in this item)

- A new side tab, **Preview**. You open a file from:
  - the Changes list;
  - a file path in a conversation;
  - "Preview" in a file's menu.
- It shows one of two things:
  - **Pages**: in Ely's `DocumentViewer` (`documents::viewer`), which takes
    `DocPage { source, width, height }`: an image file and its size in
    points. It has thumbnails, zoom and search highlights (`PageHit`).
  - **Table**: sheet tabs, then a grid of cells.
- Word and PowerPoint also get a reflowing **reading view** that the UI
  draws itself; it fits the narrow panel, selects text and follows dark
  mode. Its data comes from `atrium-office` (item 9, step 1,
  `docs/office-editing-implementation.md`), which later makes it editable.
  This item provides the **page view**, the original layout, as images.
- Images, Markdown, code and plain text are drawn by the UI directly. They
  never reach this crate.
- When the file changes on disk, the UI calls `preview` again; the cache
  makes an unchanged file free.

## What to build

A new crate, **`crates/atrium-preview`**, without GPUI, so it is tested
headless. The UI calls it from a background task.

### 1. API

```rust
pub enum Kind { Word, Slides, Sheet, Csv, Pdf, Other }

/// By extension, then checked against the content: a zip for OOXML and
/// ODF, `%PDF-` for PDF. A mismatch is `Other`.
pub fn kind(path: &Path) -> Kind;

pub struct Previewer { /* cache dir, font set, the converted-PDF memo */ }

impl Previewer {
    pub fn new(cache_dir: PathBuf, fonts: Fonts) -> Self;

    /// Blocking; the UI runs it on the background executor.
    pub fn preview(&self, path: &Path, ask: &Ask) -> Result<Preview, Refused>;

    /// Text search over the same PDF, for highlights.
    pub fn find(&self, path: &Path, needle: &str) -> Result<Vec<Hit>, Refused>;
}

pub struct Ask {
    pub width_px: u32,          // panel width × scale factor; pages are drawn this wide
    pub pages: Range<usize>,    // pages wanted now; the UI asks for more as it scrolls
    pub sheet: Option<usize>,   // Sheet only: which sheet as a table
    pub layout: Layout,         // Sheet only
}

pub enum Layout { Grid, Pages }  // a sheet as cells, or as printed pages

pub enum Preview {
    Pages { pages: Vec<Page>, total: usize, warnings: Vec<String> },
    Table { sheets: Vec<String>, current: usize, rows: Vec<Vec<Cell>>, more_rows: bool },
}

pub struct Page { pub index: usize, pub png: PathBuf, pub width_pt: f32, pub height_pt: f32 }
pub struct Cell { pub text: String, pub bold: bool, pub align: Align, pub span: (u16, u16) }
pub struct Hit  { pub page: usize, pub rect_pt: [f32; 4] }

pub enum Refused { Unsupported, Password, Damaged, TooBig, Timeout, Failed(String) }
```

### 2. What each kind gives

| Kind | Extensions | Default view | Notes |
| --- | --- | --- | --- |
| Word | `docx` | `Pages` via office2pdf → hayro | `.doc`, `.odt` and `.rtf` give `Unsupported` in v1 |
| Slides | `pptx` | `Pages`, one per slide | hidden slides left out, as office2pdf does by default; `.ppt` and `.odp` give `Unsupported` |
| Sheet | `xlsx`, `xls`, `xlsb`, `ods` | `Table` (`Layout::Grid`) via calamine | `Layout::Pages` renders `xlsx` with office2pdf, for print layout and charts |
| Csv | `csv`, `tsv` | `Table` | sniff the separator; detect UTF-8 / UTF-16 BOM / GBK / Shift-JIS with `encoding_rs` (try UTF-8, then GBK when the UI language is Chinese) |
| Pdf | `pdf` | `Pages` via hayro | password-protected files give `Password` (hayro has no decryption) |

`Cell` details for the grid:

- Show the **formatted** value: dates as dates, and numbers with the cell's
  number format when calamine exposes it, otherwise as they are.
- Show merged cells as `span`, and bold.
- No other styling in v1.

### 3. Rendering, caching and limits

**Rendering**

1. Convert to PDF once per file version, then keep the PDF bytes in the
   cache (`doc.pdf`) so scrolling does not convert again.
2. Render only the pages in `ask.pages`. Scale each page so it comes out
   `width_px` wide: `scale = width_px / page_width_pt`.
3. Give `office2pdf`'s warnings back in `Preview::Pages::warnings`. They are
   shown in a "Some parts may look different" line.

**Cache**

- Files go under `cache_dir/<key>/`: `doc.pdf`, then
  `<page>-<width_px>.png`.
- `<key>` is the hex SHA-256 of: the absolute path, the file's size and
  mtime, and this crate's `RENDER_VERSION` constant. Bump that constant
  whenever the output changes.
- Keep at most 500 MB in total. When it is over, evict the
  least-recently-used keys (by the directory's mtime; touch it on every hit).
- The UI passes `dirs::cache_dir()/Atrium/previews`.

**Limits**, so a preview can never hang or exhaust the app:

- Files over 100 MB: `TooBig`.
- Do not render more than 20 pages per call.
- Grid: at most 10 000 rows and 200 columns per call, with `more_rows` set.
- Run conversion and rendering in a worker thread with a 30 s deadline,
  and return `Timeout` when it passes. A conversion cannot be interrupted,
  so let the thread finish and discard its result. Never run two
  conversions of the same file at once.
- Catch panics from the dependencies (`std::panic::catch_unwind` around
  each call) and return `Failed`, so a bad file cannot crash Atrium.
- Zip bombs: before converting, reject any OOXML whose total uncompressed
  size is over 500 MB (`TooBig`).

### 4. Fonts (Chinese first)

The UI is used mostly in Chinese, so Chinese documents must not show tofu
(empty boxes).

- `Fonts` holds the system font directories plus any extra paths. Pass
  them through `ConvertOptions::font_paths`.
  - macOS: `/System/Library/Fonts`, `/Library/Fonts`, `~/Library/Fonts`.
  - Windows: `%WINDIR%\Fonts`, `%LOCALAPPDATA%\Microsoft\Windows\Fonts`.
  - Linux: `/usr/share/fonts`, `~/.local/share/fonts`.
- **Map common Chinese font names** to what is installed, so a document set
  in 宋体, 微软雅黑, 黑体 or 等线 does not fall back to a Latin font:
  - macOS: PingFang SC, Songti SC, Heiti SC.
  - Windows: Microsoft YaHei, SimSun, DengXian.
  - Linux: Noto Sans/Serif CJK SC, WenQuanYi.
  Use the API office2pdf provides for substitutes. If it has none, open an
  issue upstream and fall back to the system CJK font for any CJK run.
- **hayro**: set `InterpreterSettings::font_resolver` so non-embedded CJK
  fonts in PDFs use the system CJK font. The PDFs office2pdf writes embed
  their fonts already, so this matters for PDFs from elsewhere.

### 5. Later, not in this item

- ElyOffice's LOK engine as an optional "original layout" mode, for files
  the pure-Rust path draws badly.
- `.doc`, `.ppt`, `.odt` and `.odp`.
- Editing anything.

## Tests

1. **Kinds.** `kind()` for each extension, and for a `.xlsx` whose content
   is a PDF (gives `Other`).
2. **Fixtures.** Commit small files under
   `crates/atrium-preview/tests/fixtures/`, each under 100 KB:
   - Docx: Chinese headings, a table, an image, a header and footer, and
     two pages. Write it in Chinese, for example a 周报 (weekly report).
   - Pptx: three slides, one with a Chinese title.
   - Xlsx: two sheets with Chinese names, a merged cell, a date and a
     currency cell.
   - Csv: one in GBK and one in UTF-8 with a BOM.
   - Pdf: two pages.

   Make them with any office suite, or by hand. office2pdf's
   `tests/golden_mocks` files (Apache-2.0) may be copied with attribution.
3. **Output checks.**
   - Each page and table count is right.
   - Each PNG is `width_px` wide.
   - For the Chinese docx, the page's text has no tofu: render it, then
     check with `find("周报")` that the text is there and positioned.
   - Pixel checks stay loose (the page is not blank and has dark text
     pixels). No exact pixel comparisons: fonts differ per OS.
4. **Grid.**
   - Merged spans, formatted dates, both CSV encodings.
   - A generated 20 000-row CSV gives 10 000 rows and `more_rows`.
5. **Cache.**
   - A second call does not convert again (count with a test hook).
   - Touching the file converts again.
   - Going over the size limit evicts the oldest key.
6. **Failure.**
   - A truncated docx gives `Damaged` or `Failed`, not a panic.
   - An encrypted PDF gives `Password`.
   - A zip bomb gives `TooBig`.
7. **Timing.** Behind `#[ignore]`: preview each fixture in release mode and
   print the times. Paste the output in the PR.

## Done when

- `cargo test -p atrium-preview` passes on Linux. CI runs it on macOS and
  Windows too, if the workflow has them.
- A Chinese docx renders with Chinese text on each OS it was tried on.
  Paste a screenshot or describe it in the PR.
- The PR lists:
  - how much bigger the release binary got;
  - every new dependency and its license.
- The README credits office2pdf, hayro and calamine.
- No GPUI in `atrium-preview`. The UI side then adds the Preview tab on top
  of this API.
