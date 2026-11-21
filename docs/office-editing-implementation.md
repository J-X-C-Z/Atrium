# Building the office core: open, edit and save Word, Excel and PowerPoint

A brief for **`crates/atrium-office`**: Atrium's own document core, in Rust.
It lets you edit a file an agent wrote, in the side panel, save it, and hand
your changes back to the agent. Read `AGENTS.md` first, then
`docs/file-preview-implementation.md` (item 8), which this builds on.

## Status: what exists, what is yours

**Built on the UI side, with tests: everything that does not touch the file.**

- **`crates/atrium-office`**, without GPUI:
  - the model: `WordDoc`/`Block`/`Para`/`Inline`, `Workbook`/`Sheet`/`Cell`,
    and `Deck`/`Slide`/`Shape`;
  - the edit ops, each returning its exact inverse;
  - `Document`, which records the ops since the last save in `pending`;
  - `snapshot`, `changes` and `describe` (§6).
- **The ops, as built** (they replace the finer-grained list in §2):
  - Word: `SetContent { para, inlines }`, `InsertPara { at: Place, id,
    style, inlines }`, `DeletePara`, `SetStyle`, `SetAlign`;
  - Excel: `SetCell { sheet, r, c, input }` and `Restore`;
  - PowerPoint: `SetContent` on a paragraph in a text box.

  The editor works a paragraph at a time, so `SetContent` replaces a whole
  paragraph's inlines.
- **The side panel's Preview view** (`ui/preview.rs`, `ui/document.rs`,
  `docmap.rs`):
  - Word and PowerPoint in Ely's block editor, with locked paragraphs
    read-only;
  - Excel in Ely's spreadsheet;
  - save, "save when the agent stops", undo save (from a backup copy);
  - the changed-on-disk prompt (load theirs, keep mine, save a copy);
  - the change report put before your next message.

**Yours: the file binding.** The bodies of `atrium_office::open` and
`Document::save_as` / `save_over`, which today return `Unsupported`.

- **`open`**:
  - read the package (§1);
  - build the model with stable `NodeId`s: Word paragraph ids must also
    work for paragraphs inside table cells;
  - extract images to a temp folder for `AtomKind::Image`;
  - set `Document.hash` to the file's SHA-256.
- **`save_as`**:
  - check the hash (`ChangedOnDisk`, except in `save_over`);
  - replay `pending` onto the XML, touching only the paragraphs and cells
    the ops name (the lossless rules below);
  - write atomically;
  - clear `pending` and update `hash`.
- **`InsertPara` gets no XML of its own.** Copy the `w:pPr` of the
  paragraph it follows (or the first in its container) and change only the
  style. A paragraph that was deleted and then restored by undo (`id:
  Some(..)`) must come back with its original XML: keep deleted
  paragraphs' bytes until save.
- **Mapping `SetContent` onto runs.** Diff the paragraph's old text
  against the new one. Keep every run whose text and marks are unchanged,
  with its `w:rPr`. For changed spans, copy the `w:rPr` of the neighbouring
  run, then set or clear b, i, u, strike, and `w:hyperlink`.

The tests in §7 stand, run against `open` and `save`. In-memory op
semantics are already covered by the crate's tests.

## What we take from LibreOffice, and what we leave

LibreOffice's core has four layers. We rebuild the first three, cut down to
simple editing, and borrow the fourth:

| LibreOffice layer | What it does there | Ours |
| --- | --- | --- |
| Filters (import/export) | read and write every format | **OOXML only** (docx, xlsx, pptx), **lossless**: what we do not understand is kept byte for byte |
| Document model | everything a document can hold | **paragraphs, runs, tables, cells, text boxes**; everything else kept as opaque, read-only pieces |
| Commands (UNO dispatch) and undo | hundreds of commands | **about 15 edit operations**, each with an exact inverse |
| Layout and rendering | Writer's page layout engine | **not rebuilt**. The UI draws a reflowing editable view itself. The page view comes from item 8 (office2pdf + Typst) |

Rules:

- **Do not copy or translate LibreOffice source.** It is MPL-2.0, which is
  file-level copyleft. Borrow its ideas and architecture only.
- The ECMA-376 (OOXML) specification is the reference for the XML.

**Out of scope:**

- Page layout of our own, and formula calculation (Excel recalculates on
  open, §5).
- Editing images, charts, shapes, fields, footnotes, comments or tracked
  changes.
- `.doc`, `.xls`, `.ppt` and the ODF formats.
- Creating new documents (agents create them; we edit).

## The non-negotiable: lossless round trip

A user who changes one word must get back the same file with one word
changed. Everything else in this brief serves that.

1. **Package.** Read every zip entry and keep its raw compressed bytes,
   compression method, order and timestamps. On save, copy every unchanged
   entry verbatim, without inflating it again. Only changed parts are
   written fresh, with the same entry name and method.
2. **XML.** Parse a part with `quick-xml` into a tree whose nodes keep the
   **byte range they came from**. Serializing a node that was not touched
   writes its original bytes. A touched node is written from the model,
   keeping:
   - its namespace prefixes, attribute order and every attribute we do not
     model;
   - its unknown child elements, in place.
   The XML declaration, `mc:Ignorable` content, and any whitespace outside
   touched nodes stay as they were.
3. **Granularity.** The smallest rewritten unit is one paragraph (`w:p`,
   `a:p`) or one cell (`c`). Editing paragraph 12 leaves the bytes of every
   other paragraph alone.

## 1. Package and XML (shared)

```rust
pub struct Package { /* entries: name → raw bytes + method; parsed parts on demand */ }
impl Package {
    pub fn open(path: &Path) -> Result<Package, OpenError>;
    pub fn part(&mut self, name: &str) -> Result<&mut XmlPart, OpenError>;
    pub fn relationships(&self, part: &str) -> Vec<Rel>;
    pub fn save(&self, to: &Path) -> Result<(), SaveError>;
}
```

- Limits as in item 8:
  - files over 100 MB are refused (`TooBig`);
  - any OOXML whose total uncompressed size is over 500 MB is refused (zip
    bomb);
  - XML nesting over 256 levels is refused.
- `OpenError`:
  - `Encrypted` (an OLE file with `EncryptionInfo`);
  - `Legacy` (`.doc`, `.xls` or `.ppt`);
  - `Damaged`;
  - `TooBig`.

**Saving**

1. Write a temp file in the same folder, `fsync` it, then rename it over
   the original. Keep the file's permissions.
2. Before writing, compare the file's SHA-256 with the one taken at open
   or at the last save. If it differs, the file changed on disk (usually
   the agent): return `SaveError::ChangedOnDisk`. The UI then asks whether
   to reload, overwrite, or save a copy.

## 2. Word (`docx`)

### Model

```rust
pub struct WordDoc { /* package + body view */ }

pub enum Block {
    Paragraph(Para),
    Table(Table),
    Opaque(OpaqueBlock),          // sdt, sectPr, altChunk, anything unknown: shown, not editable
}
pub struct Para {
    pub id: NodeId,
    pub style: ParaStyle,          // Normal | Heading(1..=6) | Title | ListItem{level, ordered} | Quote | Other(name)
    pub align: Align,
    pub inlines: Vec<Inline>,
    pub locked: bool,              // see "Locked" below
}
pub enum Inline {
    Text { text: String, marks: Marks },
    Break,                         // w:br
    Tab,
    Atom(AtomKind),                // image, field, footnote ref, bookmark…: one character wide, kept whole
}
pub struct Marks { pub bold: bool, pub italic: bool, pub underline: bool, pub strike: bool, pub link: Option<String> }
pub struct Table { pub id: NodeId, pub rows: Vec<Vec<CellBody>> /* cells hold Vec<Block> */, pub merged: Vec<(usize, usize)> }
```

**Headings**

- Take the heading level from the style's `w:outlineLvl` in
  `styles.xml`, following `w:basedOn`, not from the style's name.
  Chinese Word names them 标题 1, with ids such as `1` or `a1`.
- When no style has an outline level, fall back to the built-in ids
  `Heading1`–`Heading6` and `Title`.

**Lists** come from `w:numPr`; read `numbering.xml` only to know whether
the list is ordered.

**Marks** are the run's effective bold, italic, underline and strike:
direct formatting first, then the character style, then the paragraph
style. Show only these four, plus hyperlinks. Keep every other run property
untouched (fonts, sizes, colours, East Asian settings).

**Locked paragraphs** are shown but not edited in v1. They are the ones
that contain:

- tracked changes (`w:ins`, `w:del`) or comment ranges;
- fields spanning several runs (`w:fldChar`);
- content controls (`w:sdt`);
- math (`m:oMath`).

They show a lock icon with "Open in Word to edit this paragraph".

### Positions and selection

`Pos { block: BlockPath, offset: usize }`. The offset counts UTF-8 bytes in
the paragraph's text, where each `Break`, `Tab` and `Atom` counts as one
char (U+FFFC). GPUI uses UTF-8 byte offsets, so the UI can use them
directly.

`BlockPath` reaches into tables as `[table, row, col, block]`.

### Edit operations

Every edit is an `Op`. `apply(op) -> Op` returns its exact inverse. Undo
and redo are stacks of inverses. Grouping (one undo step per typed word)
happens in the UI.

| Op | Effect on the XML |
| --- | --- |
| `InsertText { at, text, marks }` | into the run at `at` when its marks match, else a new `w:r` that copies the neighbour's `w:rPr` with the marks changed |
| `Delete { from, to }` | within one paragraph or across paragraphs (merges them; the first keeps its `w:pPr`); `Atom`s in the range go too, except locked ones, which refuse the op |
| `SetMarks { from, to, mark, on }` | splits runs at the edges; adds or removes `w:b`, `w:i`, `w:u`, `w:strike` |
| `SplitPara { at }` (Enter) | a new `w:p` with a copy of the `w:pPr`; after a heading, the new paragraph is Normal |
| `SetStyle { para, style }` | changes `w:pStyle`; for lists, `w:numPr` with an existing list definition of that kind (create one in `numbering.xml` only if none exists) |
| `SetLink { from, to, url }` | `w:hyperlink` with a new external relationship |
| `InsertPara`, `DeletePara` | whole paragraphs |
| `SetCellText` | a table cell's first paragraph; whole-cell replacement |
| `InsertRow`, `DeleteRow` | copies the row above's `w:trPr` and `w:tcPr` |

Rules when writing runs:

- After an edit, merge adjacent runs in the touched paragraph whose
  `w:rPr` are byte-identical. Drop `w:proofErr` and the touched runs'
  `w:rsid*` attributes.
- `xml:space="preserve"` is set on `w:t` whenever the text has
  leading or trailing spaces.

### Reading

- `blocks() -> &[Block]` for the UI.
- `to_markdown() -> String`, the text the agent is given (§6). Atoms
  become `[图片]`, `[image]` and the like, in the UI language.
- Images are read out of `word/media` into a temp folder, so the UI can
  show them.

## 3. Excel (`xlsx`)

- Read with the same Package and XML layer. Do not use `umya-spreadsheet`
  or calamine for writing: neither keeps unknown content byte for byte.
  calamine may still be used for reading in item 8.
- **Model:** sheets (name, hidden), each as a sparse grid of
  `Cell { r, c, value: Value, formula: Option<String>, style: u32 }`.
  - `Value` is `Empty | Number | Text | Bool | Error | Date`. A date is
    a number with a date format.
  - Shared strings are resolved; merged ranges and column widths are read.
- **Display:** format numbers with the cell's number format, as a subset:
  - General, `0`, `0.00`, `#,##0` and `#,##0.00`;
  - percentages;
  - `yyyy-mm-dd` and the built-in date and time ids 14–22;
  - Chinese dates: `yyyy"年"m"月"d"日"`;
  - currency prefixes and suffixes.

  Anything else shows the raw value.

**Edit operations:**

- `SetCell { sheet, r, c, input }`, where `input` is what the user typed:
  - `=…` is a formula. Write `<f>`, remove the cached `<v>`, and set
    `fullCalcOnLoad="1"` on `workbook.xml`'s `calcPr`, so Excel
    recalculates on open.
  - A number or a date in the cell's format is written as a number.
  - Anything else is text, written as an inline string (`t="inlineStr"`),
    so `sharedStrings.xml` stays untouched.
  - Keep the cell's style index.
- `ClearCell`.
- No inserting or deleting rows or columns in v1. Shifting references
  would break formulas.
- Cells inside a table (`tableParts`), array formulas and data validations
  stay editable. Pivot table areas are locked.

## 4. PowerPoint (`pptx`)

- **Model:** slides in order (skip hidden ones only in the view, not in the
  model). On each slide, its text shapes (`p:sp` with `p:txBody`) as
  `Vec<Para>`, using DrawingML's `a:p`, `a:r` and `a:rPr` (`b="1"`,
  `i="1"`, `u="sng"`, `strike="sngStrike"`), together with each shape's
  position and size.
- **Ops:** the Word text ops, inside one shape: `InsertText`, `Delete`,
  `SetMarks`, `SplitPara`. Text in tables (`a:tbl`) is edited the same
  way.
- **Not in v1:** moving shapes, adding slides, editing notes.

## 5. Opening and saving, for the app

```rust
pub enum Office { Word(WordDoc), Sheet(Workbook), Slides(Deck) }
pub fn open(path: &Path) -> Result<Office, OpenError>;
impl Office {
    pub fn apply(&mut self, op: Op) -> Result<Op, EditError>;   // returns the inverse
    pub fn is_dirty(&self) -> bool;
    pub fn save(&mut self) -> Result<Saved, SaveError>;         // in place; Saved has the new hash
    pub fn save_as(&mut self, to: &Path) -> Result<Saved, SaveError>;
    pub fn snapshot(&self) -> Snapshot;                         // text of every block/cell, keyed by NodeId
}
```

- `EditError`: `Locked`, `OutOfRange` or `Invalid`. An op that fails
  changes nothing.
- The UI calls `open` and `save` on the background executor. Edits are
  cheap and run in place.

## 6. Handing your changes to the agent

This is the reason to edit inside Atrium rather than in Word.

```rust
pub fn changes(before: &Snapshot, after: &Snapshot) -> Vec<Change>;
pub fn describe(changes: &[Change], lang: Lang) -> String;
```

**`Change`** is one of:

- `Edited { where_: Location, old: String, new: String }`;
- `Added { … }`;
- `Removed { … }`;
- `Restyled { … }`.

`Location` names the place the way a person would:

- `第 2 节「预算」第 3 段`, or `"Budget" section, paragraph 3`;
- `表格 1 第 4 行第 2 列`;
- `Sheet1!B7`;
- `第 3 张幻灯片「目标」`.

Match blocks across the two snapshots by `NodeId` first, then by text
similarity (`similar` crate) for paragraphs that were split or merged.

**`describe`** writes a short report in the UI's language, Chinese first:

```
我在 报告.docx 里改了这些地方：
- 「预算」第 3 段：「预计 50 万」→「预计 65 万，含外包」
- 删除了「风险」下的第 2 段
- Sheet1!B7：120 → 150
请在这个基础上继续。
```

- Cut each quote to 200 characters, and list at most 30 changes, with
  "还有 N 处改动".
- The UI gives this to the agent with your next message, as project
  notices are handed over now.
- `before` is the snapshot taken when the agent's last turn ended, or when
  you opened the file.

**On the app side, built by the UI:**

- Saving takes a checkpoint first (`repo::checkpoint`), so it can be undone
  like an agent's turn.
- While the agent is running, the file cannot be saved. "Save when the
  agent stops" queues the save.

## 7. Tests

**Corpus.** Use office2pdf's `tests/golden_mocks` (Apache-2.0, copied
with attribution) plus our own Chinese fixtures under
`crates/atrium-office/tests/fixtures/`. For each format, include files
written by Word, Excel and PowerPoint, by WPS, by LibreOffice, and by
python-docx and openpyxl, which is what agents mostly use.

1. **Identity.** Open and save with no edit: every file in the corpus comes
   out **byte-identical**.
2. **Locality.** Make one edit, then compare:
   - only the edited part's zip entry differs;
   - inside it, only the edited paragraph's or cell's byte range differs.
3. **Re-read.** After each op type, the saved file:
   - opens again in `atrium-office` with the expected model;
   - converts with `office2pdf` without error;
   - for xlsx, reads with `calamine` and shows the new value.
4. **Undo.** Random sequences of 200 ops, generated with proptest from the
   corpus, then undo all of them: the model equals the original, and saving
   gives the original bytes.
5. **Heading detection.**
   - Chinese Word styles (标题 1), WPS styles, python-docx defaults.
   - Locked paragraphs refuse edits and stay byte-identical.
6. **Excel.**
   - Typing `=SUM(A1:A3)` writes `<f>`, removes `<v>` and sets
     `fullCalcOnLoad`.
   - Text goes in as an inline string; `sharedStrings.xml` is untouched.
   - Number-format display for each supported format.
7. **Save safety.**
   - A file changed on disk gives `ChangedOnDisk`.
   - A failed write leaves the original intact (inject a failure after
     the temp file is written).
8. **Change reports.** Fixed before/after pairs give the expected
   `describe` text in Chinese and English: an edit, a split paragraph, a
   deleted row and a cell.
9. **Real apps, by hand, once per PR.** Open edited files in Word or WPS,
   Excel and PowerPoint (or LibreOffice when those are not available).
   Check there is no "repair this file" prompt and the edit shows. Say in
   the PR which apps were used.

## Backlog split

Do these as separate PRs, in order:

1. **Package, XML and Word read-only model**, with identity tests and
   `to_markdown`. This also gives item 8's reading view its data.
2. **Word edit ops, undo and save**, with locality, undo and save-safety
   tests.
3. **Excel model, display and `SetCell`.**
4. **Change reports** (§6).
5. **PowerPoint text.**

## Done when

- All tests above pass in `cargo test -p atrium-office`, with the corpus
  committed. Keep each fixture under 200 KB.
- The PR for step 2 includes a screen recording or notes of a Word file
  edited and saved through the crate's API, then opened in Word or WPS
  without a repair prompt.
- No GPUI in `atrium-office`. The side panel editor is built on the UI side
  on top of this API, using Ely's `BlockEditor` for the reflowing view.
