# Page layout: editing on the real pages

A brief for backlog items 14 and 15. Read `AGENTS.md` first.

## What this is for

Word and PowerPoint files open in the side panel on their real pages, laid
out as Word or PowerPoint would lay them out, and you edit right on those
pages: click to put the caret, type, select, format. The pages are not
pictures. Atrium lays the document out itself and paints the text with the
app's own text system, so it can redraw after each keystroke.

Three layers:

```
.docx / .pptx ──atrium-office──▶ WordDoc / Deck, with looks ──atrium-layout──▶ pages ──GPUI──▶ screen
                 (item 14)                                     (item 15)               (UI side)
```

- **Item 14, the file binding (yours):** read every value the layout needs
  into the model's look types, and write the user's formatting back to the file.
- **Item 15, the layout engine (yours):** replace the reference engine in
  `crates/atrium-layout` with one that lays documents out as Word does.
- **The UI (built on the UI side, not yours):** the shaper over GPUI's text
  system, painting pages, the caret, selection, typing, IME, toolbars.

The office2pdf page images in `atrium-preview` stay as they are. They are
still used for PDFs, and for files the editor cannot open.

## The contract

### The model's looks: `crates/atrium-office/src/look.rs`

Each type there says what its fields mean. They hang off the existing model:

| Where | Field | Holds |
| --- | --- | --- |
| `Marks` | `fonts`, `size`, `color`, `highlight`, `baseline` | a run's effective formatting, next to `bold`/`italic`/… |
| `Para` | `look: ParaLook` | spacing, line spacing, indents, keeps, tabs, shading, borders, list, grid, paragraph mark |
| `Table` | `look: TableLook` | grid columns, per-cell shading/borders/alignment/vertical merge, row heights, header rows, padding |
| `Atom` | `look: AtomLook` | a picture's size and float; what a field or footnote reference shows |
| `WordDoc` | `sections()`, `defaults()` (set with `with_look`) | page size, margins, header/footer distances, columns, line grid, headers and footers; the default run |
| `Shape` | `look: ShapeLook` | fill, outline, insets, vertical anchor, rotation, autofit scale, picture |
| `Slide` | `background` | |
| `AtomKind::Field(String)` | | the field's instruction, such as `PAGE` |
| `WordOp::SetLook` | | replaces a paragraph's `ParaLook`; the binding refuses it until 14b |

The rules:
- **Units.** Lengths are twips (1/20 pt) and colours are 0xRRGGBB. Convert
  EMU as `emu / 635`, and half-points as `× 10`.
- **Values are effective.** The loader resolves:
  - for paragraphs: document defaults, then the style chain, the numbering
    level's `pPr`, and direct `pPr`;
  - for runs: document defaults, then the paragraph style's `rPr`, the
    character style, and direct `rPr`;
  - theme fonts (`+minorEastAsia` and the others) through the theme part.

  The layout never reads styles.
- **Fonts.** A run's `Marks::fonts` names the effective Latin and East Asian
  family. Empty means `WordDoc::defaults()`.
- **Untouched fields stay at their defaults.** A paragraph whose `look` is
  still `ParaLook::default()` gets the reference engine's style hints
  (larger, bold headings). Once you resolve real values, the hints never
  apply.

You may add fields to the look types, with defaults. Do not change what
the existing ones mean; the UI reads them too.

### The layout API: `crates/atrium-layout/src/lib.rs`, `nav.rs`

Keep these signatures; the UI is built on them:
- `layout(&WordDoc, &dyn Shaper) -> Layout`
- `relayout(&Layout, &WordDoc, changed, &dyn Shaper) -> Layout`
- `layout_slide` and `layout_deck`
- the `Shaper` trait and `FontKey`
- the `Item`, `Line` and `Page` types
- the `Layout` methods: `caret`, `hit`, `selection`, `vertical`,
  `line_start`, `line_end`, `step`, `compare`, `ends` and `paras_between`

Their doc comments are the specification. In particular:
- **Positions.** A caret position is a byte offset into `Para::text()`,
  where atoms are U+FFFC and tabs and breaks are one byte.
- **`Line::stops`.** Every caret stop, left to right. A list label has no
  stops.
- **Furniture.** Header and footer lines carry `furniture: true`.
- **Painting.** Items paint in order. A `Text` item is one shaped run, and
  the painter shapes `text` with `font` again. So split runs wherever the
  engine places glyphs apart: justification, letter spacing, a tab.
- **The engine never loads fonts.** All measuring goes through `Shaper`.
  Do not add a GPUI dependency to the crate.

You may add functions, and fields to `Line` and `Page`.

## Item 14: the binding reads and writes looks

Do it in three PRs.

### 14a: read Word looks

Fill every field in the table above from the package:
- **Paragraphs and runs**:
  - `styles.xml`: `docDefaults`, the `basedOn` chains, `rPr`, `pPr`;
  - direct formatting;
  - `contextualSpacing`, `snapToGrid`;
  - `spacing` with `lineRule` (`auto`, `atLeast`, `exact`);
  - `ind`, including `firstLine`, `hanging`, and the character units
    `firstLineChars` (a hundredth of a character of the paragraph's size);
  - `keepNext`, `keepLines`, `pageBreakBefore`, `widowControl`, `tabs`,
    `shd`, `pBdr`.
- **Theme fonts**: `rFonts` with `asciiTheme`/`eastAsiaTheme`, through
  `theme/theme1.xml`.
- **Lists**: `numbering.xml`:
  - `num` → `abstractNum`, with `lvlOverride`;
  - `numFmt` → `NumFormat` (`chineseCounting` → `Chinese`,
    `chineseLegalSimplified` → `ChineseLegal`,
    `decimalEnclosedCircle` → `Circled`);
  - `lvlText` → `pattern`, plus `start`, `suff`;
  - the level's `pPr` indents and `rPr`.
- **Sections**: every `sectPr`, both the body's and those in paragraph
  properties:
  - `pgSz`, `pgMar` (with header, footer and gutter), `cols`, `docGrid`
    `linePitch`, `titlePg`, `pgNumType start`;
  - `type` (`continuous` → `new_page: false`);
  - the header and footer parts through their relationships, loaded as blocks.

  Header and footer paragraphs need ids that are unique in the document.
- **Tables**:
  - `tblGrid` → `columns`;
  - `gridSpan` → `merged` (as now);
  - `vMerge` → `merged_up`;
  - `tcBorders`, `tblBorders` (with `insideH`/`insideV`), the table
    style's borders;
  - `shd`, `tcMar`/`tblCellMar`, `trHeight` with `hRule`, `tblHeader`,
    `jc`, `tblInd`;
  - `vAlign`.
- **Pictures**:
  - `wp:extent` → `size`;
  - `wp:anchor` → `Float`: `positionH`/`positionV` with `relativeFrom`,
    the `wrap*` element, `dist*`.
- **Fields**:
  - the instruction from `instrText` or `fldSimple/@w:instr` →
    `AtomKind::Field`;
  - the cached result → `AtomLook::shown`;
  - footnote references → `shown` as their number.

### 14b: write Word looks back

- **Formatting inside a paragraph.** A `SetContent` whose inlines change
  `Marks` writes the smallest `rPr` that gives those effective values over
  the paragraph's style. When a run's value equals the style's, the direct
  property is removed.
- **`SetLook`.** It writes `pPr` the same way. A list change edits `numPr`
  only, with an existing `num` or a new one.
- **`InsertPara`.** The new paragraph takes the look and the paragraph mark
  of the paragraph it follows, in the model and in the file. Splitting a
  paragraph with Enter therefore keeps its formatting.

### 14c: slide looks

- **Shapes**: `spPr` `solidFill`, `ln`, `xfrm rot`, `blipFill` → `picture`.
- **Text boxes**: `bodyPr` `lIns`/`tIns`/…, `anchor`,
  `normAutofit fontScale`.
- **Inheritance**: placeholder text sizes, fonts, colours and bullets come
  from the layout and master (`p:txStyles`, `lstStyle`), resolved into
  `Marks` and `ParaLook` as for Word.
- **Backgrounds**: the slide's background, inherited from the layout and
  master.

Write back `SetContent` marks as `a:rPr` the same way.

### Tests

- **Resolution**: for each source (defaults, style chain, numbering, direct
  formatting, theme fonts), a fixture with a known effective value.
- **Round trips**: change a run's size and colour, and a paragraph's
  spacing. Save, reopen, compare the model. The XML outside the touched
  `rPr`/`pPr` stays byte for byte.
- **Real documents**: the existing fixtures plus a Chinese government-style
  report (仿宋/黑体, 三号/四号, `docGrid`, headers with `PAGE`), a
  numbered contract, and a slide deck built from a master. Assert the looks
  of some paragraphs.

## Item 15: the layout engine

Replace `engine.rs` behind the same API. The reference engine shows the
shape of the output. Read its module comment for what it leaves out. Do
it in five PRs.

### 15a: lines

- **Line breaking**: UAX #14 (`unicode-linebreak`), with the kinsoku rules
  in `engine.rs`. Word's "allow punctuation to hang" (`overflowPunct`) lets
  a closing mark sit past the edge.
- **Justification**:
  - Latin text: stretch the spaces;
  - CJK text: stretch the gaps between characters;
  - the last line and lines before a break are not justified;
  - `distribute` stretches every line.
- **Spacing between scripts**: `autoSpaceDE`/`autoSpaceDN` add a quarter em
  between CJK and Latin text or numbers, as Word does.
- **Tabs**: left, centre, right and decimal stops, with leaders. The
  default stops are the document's `defaultTabStop`; add it to the model
  if it is missing.
- **Line height**: Word's rules for `auto`, `atLeast` and `exact`, from the
  shaper's metrics:
  - `auto` multiplies the font's line height;
  - `exact` cuts glyphs that do not fit;
  - snapping to the grid rounds each line up to a multiple of
    `line_pitch`, with the text centred in it.
- **Text effects**: superscript and subscript, underline position and
  thickness from the font, highlight.

### 15b: pages

- **Paragraph spacing**: `before` and `after` add up. They are not
  collapsed, except with `contextual`. `before` is dropped at the top of a
  page, except on the first page.
- **Keeping lines together**: `keep_next`, `keep_lines`, `widow_control`
  (at least two lines at the bottom and at the top), and
  `page_break_before`.
- **Sections**:
  - `new_page: false` continues on the same page;
  - columns with `column_gap`;
  - a different first page;
  - page numbers that restart.
- **Headers and footers**:
  - laid out per page with `PAGE` and `NUMPAGES` filled in;
  - the body moves down when a header is taller than the top margin, as in
    Word.

### 15c: tables

- **Column widths**: the grid, scaled to fit when wider than the text
  area. Autofit to contents only when the file has no grid.
- **Rows across pages**: a row may split across pages unless
  `cantSplit`. Header rows repeat at the top of each page.
- **Merged cells**: vertically merged cells are drawn as one cell, with
  their text in the first.
- **Nested tables**.
- **Borders**: where two borders meet, the wider one wins.

### 15d: pictures and notes

- **Floating pictures**:
  - placed by their `Float`;
  - `Square` wrap shortens the lines beside them;
  - `TopAndBottom` pushes the text below;
  - `Behind` and `InFront` take no space.
- **Footnotes**: at the bottom of the page that references them, above a
  separator. Add the footnote text to the model as blocks in 14a's style;
  coordinate the new fields in the PR.

### 15e: speed

`relayout` lays out again from the page that holds the first changed
paragraph. It stops when a page break lands where it did before, then
copies the rest, renumbering only when the page count changed.

Budgets, measured in release mode with the test shaper:
- one keystroke in a 50-page document: under 8 ms;
- a full layout of 100 pages: under 300 ms.

Cache shaping per (text, font) inside one layout.

### Slides

The same paragraph code lays out text in shapes. Autofit shrinks the text
until it fits, in steps of 10 %, when `normAutofit` is set without a
`fontScale`.

### Tests

The reference tests in `src/tests.rs` must keep passing.

- **Deterministic tests with `FixedShaper`**:
  - one per rule above: kinsoku, justification, tabs and leaders, each
    line spacing rule, the grid;
  - keeps and widows;
  - section and column breaks;
  - table splits and header repeat;
  - floats.
- **A fidelity test with real fonts**, in `tests/fidelity.rs`. It is a dev
  dependency only:
  - a `rustybuzz` + `fontdb` shaper over Noto Sans/Serif CJK, downloaded
    in CI or found on the system;
  - the test is skipped when the fonts are missing;
  - for each fixture whose `docProps/app.xml` holds Word's `<Pages>`, the
    page count must match.

  Report the corpus and the matches in the PR.
- **Navigation**: `hit` and `caret` round-trip at every stop of every line
  of a fixture.

## Done when

- **The corpus**: the documents in `crates/atrium-office/tests/fixtures`
  and those added in 14a.
- **Page counts**: they match Word's on that corpus.
- **Side by side with Word**: the owner compares the corpus on real
  machines with the UI side's page editor (the acceptance matrix gains a
  row). Line breaks match Word on Chinese body text.
- **Speed**: the budgets in 15e hold.
- **Checks**: `cargo test --workspace` and clippy are clean.
