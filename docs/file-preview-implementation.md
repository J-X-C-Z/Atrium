# Building file previews (Word, Excel, PowerPoint, PDF)

A brief for the backend of the side panel's **Preview** tab: show a file an
agent made or changed (a report, a spreadsheet, slides, a PDF) without
leaving Atrium. Read `AGENTS.md` first.

The rendering comes from [ElyOffice](https://github.com/ELY-Open-Source/ElyOffice)
(MIT), pinned at tag **v0.1.16** (commit `7bdc36e`). Take only what a
read-only preview needs. Leave out editing, saving, macros, mail, the
gpui views and the engine build scripts.

## What the UI does (built on the UI side)

- A new side tab, **Preview** (`SideTab::Preview`). You open a file from:
  - the Changes list;
  - a file path in a conversation;
  - "Preview" in a file's menu.
- It shows one of three things:
  - **Pages**: in Ely's `DocumentViewer` (`documents::viewer`). It takes
    `DocPage { source, width, height }`, where `source` is an image file
    and the size is in points. It has thumbnails, zoom and search hits
    (`PageHit`).
  - **Table**: sheet tabs, then a grid of cells.
  - **Outline**: headings, paragraphs, tables and images, as a reading view.
- Images, Markdown, code and plain text are drawn by the UI directly. They
  never reach this crate.
- The preview reloads when the file changes on disk. The UI watches the
  file and calls `preview` again; the cache key below makes that cheap.
- A banner offers to install ElyOffice when a better view would need it,
  and links to its download page.

## What to build

A new crate, **`crates/atrium-preview`**, without GPUI, so it can be tested
headless. The UI calls it from a background task.

### 1. Vendored ElyOffice code

Copy into `third_party/ely-office/`, with its `LICENSE` and a `README.md`.
The README names the tag and commit, and lists every file changed and why.

| Take | From | Why |
| --- | --- | --- |
| `ely-ipc` (whole) | `crates/ely-ipc` | the framing the worker speaks; depends only on `log` |
| `ely-lok` client, **without** the `view` feature | `crates/ely-lok/src/{client*, ipc.rs, handshake.rs, lib.rs, ffi.rs}` and only the modules those need | spawn the worker, `load`, `pages`, `paint_tile`, `paint_part`, `sheets`, `export_pdf`, `close_doc`, `shutdown` |

Leave out:

- `ely-pdf-engine` and `ely-pdf-worker`: they are MuPDF, which is
  **AGPL-3.0**. Atrium is MIT. Do not link to or ship MuPDF.
- `ely-office`, `ely-ui`, `ely-files` and every app.

The worker binary (`ely-lok-worker`) and the LibreOffice kernel are **not
built or shipped by Atrium**. They come from the user's ElyOffice install
(§2).

Change the vendored client only where it must:

1. **Explicit paths.** Add `Worker::spawn_at(worker_bin, install, profile)`.
   Today `worker_bin()` and `install_dir()` look beside the current exe,
   which is Atrium's.
2. **Version check.** Keep the `BUILD_ID` check. A different ElyOffice
   version is refused with `Refused::EngineVersion`, never loaded half-working.

Vendoring the whole `ely-lok` crate as is, with the `view` feature off, is
also fine if it builds cleanly against our gpui version. Choose whichever
gives the smaller diff, and say which in the PR.

### 2. Finding the engine

```rust
pub struct Engine {
    pub worker: PathBuf,        // ely-lok-worker
    pub install: PathBuf,       // the LibreOffice program dir LOK loads
    pub version: String,        // ElyOffice's version, for the settings line
}
pub fn find_engine() -> Option<Engine>;
```

Look, in order, and take the first that has both the worker and the kernel
library (`ffi::KERNEL`):

1. **Overrides**: `ATRIUM_LOK_WORKER` plus `ATRIUM_LOK_DIR`, for tests and
   dev builds.
2. **macOS**: `/Applications` and `~/Applications`, the apps `ElyDoc.app`,
   `ElyTable.app` and `ElyShow.app`.
   - worker: `Contents/MacOS/ely-lok-worker`
   - install: `Contents/Frameworks`
3. **Linux**:
   - `/opt/ely-office/<slug>/bin/ely-lok-worker`, with install
     `/opt/ely-office/<slug>/bin/libreoffice/program`, for the slugs
     `ely-doc`, `ely-table` and `ely-show`. This is the deb/rpm layout in
     `packaging/linux/bundle.sh`.
   - The AppImage is not supported. Say so in the README.
4. **Windows**: read registry `HKCU\Software\Ely Office\<App>\InstallDir`,
   falling back to `%LOCALAPPDATA%\Programs\Ely Office\<App>`.
   - worker: `ely-lok-worker.exe`
   - install: `libreoffice\program`

The version comes from the app's `Info.plist`, the deb's version file or the
registry. When it cannot be read, use `"unknown"`; the `BUILD_ID` handshake
is the real check.

### 3. The preview API

```rust
pub enum Kind { Word, Sheet, Slides, Pdf, Csv, Other }
pub fn kind(path: &Path) -> Kind;   // by extension, then the zip/PDF magic

pub struct Previewer { /* one worker, started lazily, on one thread */ }

impl Previewer {
    pub fn new(cache_dir: PathBuf, engine: Option<Engine>) -> Self;
    /// Blocking; the UI runs it on the background executor.
    pub fn preview(&self, path: &Path, ask: &Ask) -> Result<Preview, Refused>;
    pub fn shutdown(&self);
}

pub struct Ask {
    pub width_px: u32,          // the panel's width × scale factor; pages are drawn this wide
    pub pages: Range<usize>,    // pages wanted now; the UI asks for more as it scrolls
    pub sheet: Option<usize>,   // for Sheet
}

pub enum Preview {
    Pages { pages: Vec<Page>, total: usize },
    Table { sheets: Vec<String>, current: usize, rows: Vec<Vec<String>>, more_rows: bool },
    Outline(Vec<Block>),
}
pub struct Page { pub png: PathBuf, pub width_pt: f32, pub height_pt: f32 }
pub enum Block {
    Heading(u8, String),
    Para(String),
    Table(Vec<Vec<String>>),
    Image(PathBuf),
    Slide(usize),               // starts slide n
}
pub enum Refused {
    NoEngine,
    EngineVersion { found: String },
    Password,
    Damaged,
    TooBig,
    Timeout,
    Failed(String),
}
```

Search goes through a separate call, so typing does not re-render pages:
`pub fn find(&self, path: &Path, needle: &str) -> Result<Vec<Hit>, Refused>`
where `Hit { page, rect_pt: [f32; 4] }`. It may use LOK's search, or the
text of an exported PDF; pick the simpler one.

What each kind gives:

| Kind | With the engine | Without it |
| --- | --- | --- |
| Word (`docx`, `doc`, `odt`, `rtf`) | `Pages`: `load`, `pages()` for the page rects, `paint_tile` per page into a PNG | `Outline`, read from `word/document.xml` with `quick-xml` (headings from `pStyle` Heading1–6 or `Title`, tables, images from `word/media`) |
| Slides (`pptx`, `ppt`, `odp`) | `Pages`, one per slide, with `paint_part` | `Outline`: each slide's title and text, plus images, from `ppt/slides/slideN.xml` |
| Sheet (`xlsx`, `xls`, `ods`) | `Table`, the same as without the engine; the grid is the useful view | `Table`, read with [`calamine`](https://crates.io/crates/calamine), showing formatted values |
| Csv (`csv`, `tsv`) | `Table`, read with the `csv` crate; separator sniffed; encoding by `encoding_rs` when not UTF-8 | (same) |
| Pdf | `Pages`. First try the **system renderer**: PDFKit on macOS (`objc2-pdf-kit`, as in ElyOffice's `ely-platform/src/print/mac.rs`), `Windows.Data.Pdf` on Windows (as in `print/win.rs`). On Linux, use `pdftoppm` when it is on `PATH`. Last, the engine (LOK opens PDF in Draw; render with `paint_part`) | The system renderer as above. On Linux without `pdftoppm`: `Refused::NoEngine` |

**Hard limits**, so a preview can never hang the app:

- Files over 200 MB: `TooBig`.
- More than 10 000 rows, or 200 columns, per sheet: return the first ones
  and set `more_rows`.
- An `Outline` stops after 2 000 blocks.
- At most 20 pages per call.
- Each worker request gets a 20 s timeout: return `Timeout`, kill the
  worker, and start a new one on the next call.
- A password-protected file gives `Password`. Do not prompt for it in v1.
- Macros never run. `load` already ensures that (LOK-11), and the
  fallbacks do not execute anything.

### 4. Cache

- Rendered pages go under `cache_dir/<key>/<page>-<width>.png`.
- `<key>` is the hex SHA-256 of: the absolute path, the file's size and
  mtime, and `BUILD_ID` (or `"sys"` for the system renderer).
- So a file that has not changed costs nothing the second time, and one
  that changed gets a new key.
- Keep at most 500 MB in total. When it is over, delete the
  least-recently-used keys (by the directory's mtime; touch it on every
  hit).
- The UI passes `dirs::cache_dir()/Atrium/previews`.

### 5. Worker lifetime

- The worker starts on the first call that needs it.
- Quit it after 5 minutes with no call, and on `shutdown()` (the UI calls it
  when the app quits).
- Give it a profile dir of its own, `cache_dir/../lok-profile`, and never
  share ElyOffice's. The vendored client already sets `SAL_NOLOCK_PROFILE`.
- If the worker dies, log `ended()` and retry the call once with a fresh
  worker.

## Tests

The engine is not available in CI, so most tests do not need it:

1. `kind()` for each extension, and for a file whose name says `.xlsx`
   but whose content is a PDF.
2. `find_engine()` from a fake layout under a temp dir, for each OS's
   layout. Make the roots injectable, for example `find_engine_in(&[roots])`.
3. **Fallbacks**, from small fixture files committed under
   `crates/atrium-preview/tests/fixtures/` (keep each under 50 KB; create
   them with LibreOffice or by hand):
   - the Word `Outline` has the headings in order, a table and an image;
   - the PowerPoint `Outline` has two slides with their titles;
   - `Table` for `.xlsx`, `.ods` and `.csv`, including a GBK-encoded CSV
     and Chinese sheet names.
4. Limits: a generated CSV with 20 000 rows gives 10 000 rows and
   `more_rows = true`.
5. Cache: a second call does not render again (count the calls through a
   test hook); touching the file renders again; going over the size limit
   evicts the oldest key.
6. Engine tests behind `#[ignore]`, run with `ATRIUM_LOK_WORKER` and
   `ATRIUM_LOK_DIR` set to an ElyOffice 0.1.16 install:
   - a `.docx` gives `Pages` with the right page count;
   - a `.pptx` gives one page per slide;
   - a wrong `BUILD_ID` gives `EngineVersion`;
   - killing the worker mid-call recovers on the next call.
   Run these once on a machine with ElyOffice installed, and paste the
   output in the PR.
7. The system PDF renderer on macOS and Windows: a two-page fixture PDF
   gives two PNGs of the asked width. Put these behind
   `cfg(target_os = ...)`; on Linux, test `pdftoppm` when present.

## Done when

- `cargo test -p atrium-preview` passes on Linux without ElyOffice.
- The ignored engine tests pass on at least one OS with ElyOffice 0.1.16
  installed, with the output in the PR.
- `third_party/ely-office/README.md` lists what was taken and what was
  changed.
- `docs/licenses` or the README credits ElyOffice (MIT) and LibreOffice
  (MPL-2.0, used through the user's own install). The PR states that no
  MuPDF code is included.
- No GPUI in `atrium-preview`. The UI side then adds the Preview tab on
  top of this API.
