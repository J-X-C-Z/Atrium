# PowerPoint and Excel as their apps show them

A brief for backlog items 16 and 17. Read `AGENTS.md` and
[`layout-engine-implementation.md`](layout-engine-implementation.md) first.
Item 14c (slide text looks) and item 15's paragraph code come before this.

## What this is for

Items 14 and 15 make a slide's *text* look and edit right. That is not
enough for real decks, which are built from drawn shapes, pictures,
tables and charts. A spreadsheet, for its part, is not text flow at all:
it is a grid whose look comes from cell styles.

- **Item 16 (yours):** slides show everything PowerPoint draws: shape
  outlines, fills, lines, groups, pictures, tables and charts.
- **Item 17 (yours):** sheets show cells as Excel does: fonts, fills,
  borders, alignment, number formats, column widths, row heights, hidden
  rows and columns, frozen panes and conditional formats. The user's
  formatting is written back.
- **The UI side (not yours):**
  - painting the new layout items (already done for paths and gradients);
  - the sheet grid drawn from these styles;
  - the formatting toolbars.

## The contract

Read the new types in `crates/atrium-office/src/look.rs` and the changes
beside them:

| Where | What |
| --- | --- |
| `ShapeLook` | `fill: Fill` (none, solid with opacity, gradient, picture with crop), `line: Option<Stroke>` (width, colour, opacity, dash, arrow heads), `path: Vec<PathCmd>` (the outline, in twips inside the frame), `flip`, `shadow`, plus the existing insets, anchor, rotation, autofit |
| `Shape::kind: ShapeKind` | `Text`, `Picture`, `Table(Table)` (the Word table type, so its cells' paragraphs edit like Word's), `Chart(Chart)`, `Fallback(picture)` |
| `Chart`, `Series`, `ChartKind` | a chart from its cached values |
| `CellStyle`, `HAlign` | one cell style: font as `Marks`, fill, borders, alignment, wrap, shrink, indent, rotation, number format |
| `Workbook::styles`, `with_styles`, `add_style` | each distinct style once; index 0 is the default |
| `Cell::style` | the cell's style index; a styled empty cell is a `Cell` with `Value::Empty` |
| `Sheet::look: SheetLook` | column widths and row heights in points, defaults, hidden rows and columns, row and column styles, frozen panes, gridlines, zoom, conditional rules, drawings, tab colour |
| `CondRule`, `CondKind`, `Compare` | conditional formats |
| `Anchored`, `Drawing` | pictures, charts and shapes over a sheet |
| `SheetOp::SetStyle` | gives a cell another style; the binding refuses it until 17b |
| `Workbook::shown(sheet, r, c) -> Shown` | the style, the text and the data bar a cell shows. **The UI paints from this.** It applies the style alone for now. |

In `crates/atrium-layout`:
- `Item::Path` (segments in page points, fill with opacity, `Pen`) and
  `Item::Gradient`;
- `chart(&Chart, Rect, &dyn Shaper) -> Vec<Item>`, with a reference that
  draws columns, bars and lines;
- slide tables, charts and fallbacks laid out by `layout_slide`.

The rules are those of the first brief:
- Values arrive resolved. Theme colours with `lumMod`/`lumOff`/`tint`/
  `shade`/`alpha` become `Rgb` plus opacity; inherited placeholder values
  are filled in.
- You may add fields with defaults. Do not change what the existing ones
  mean.
- No GPUI in these crates.

## Item 16: slides

Do it in four PRs.

### 16a: shapes

- **Shape tree**: read every `p:sp`, `p:pic`, `p:cxnSp`, `p:grpSp` and
  `p:graphicFrame` in z-order. Today the binding keeps only shapes that
  hold text.
- **Groups**: apply each group's `a:xfrm` (`chOff`/`chExt` scaling) to its
  children, so every `Shape::frame` is in slide points. Keep a group as
  its members, in order.
- **Outlines**: `prstGeom` → `path`, by evaluating the preset's formulas
  from ECMA-376 `presetShapeDefinitions.xml` with the shape's `avLst`
  adjustments. Cover at least:
  - `rect`, `roundRect`, `ellipse`, `triangle`, `rtTriangle`, `diamond`,
    `parallelogram`, `trapezoid`, `pentagon`, `hexagon`, `octagon`;
  - `chevron`, `homePlate`, the arrows (`rightArrow`, `leftArrow`,
    `upArrow`, `downArrow`, `leftRightArrow`);
  - the callouts (`wedgeRectCallout`, `wedgeRoundRectCallout`,
    `wedgeEllipseCallout`), `cloud`, `star5`, `plus`, `line`;
  - the connectors `straightConnector1`, `bentConnector2`/`3`,
    `curvedConnector3`.

  `custGeom` → `path` from its own path list.
- **Fill and line**:
  - `fill` from `solidFill`, `gradFill` (`lin ang`, `path`), `blipFill`
    (with `srcRect` → `crop`) and `noFill`;
  - `line` from `a:ln` (`w`, fill, `prstDash`, `headEnd`/`tailEnd`).
  - When neither is set, take them from the shape's `p:style` references
    (`fillRef`, `lnRef`) into the theme's format scheme.
- **Shadow**: `outerShdw`.
- **Pictures**: `p:pic` → `ShapeKind::Picture` with `Fill::Picture`. Use
  the media extraction that Word pictures already use.
- **Backgrounds**: `p:bg` (`bgPr` or `bgRef`), inherited from the layout
  and master. When the background is a picture or a gradient, set
  `Slide::background` to its main colour and add a shape for it at the
  back.
- **Masters and layouts**: their shapes that are not placeholders (logos,
  bars, footers) are drawn behind the slide's own shapes. Mark them
  `locked` so their text is not edited from the slide.

### 16b: tables

- `a:tbl` → `ShapeKind::Table` built from the Word table types:
  - `gridCol` → `TableLook::columns`;
  - `gridSpan`/`hMerge` → `merged`;
  - `rowSpan`/`vMerge` → `merged_up`;
  - cell fills, borders (`lnL`/`lnR`/`lnT`/`lnB`), margins and `anchor`.
- **Table styles**: the table style (`tableStyleId` against
  `tableStyles.xml`, plus the built-in default styles) gives the banded
  rows and the header row, resolved into each cell's look.
- **Ids**: each cell's paragraphs get ids like any slide paragraph.
  `SlideOp::SetContent` writes them back.

### 16c: charts

- **Reading**: a `graphicFrame` holding `c:chart` → `ShapeKind::Chart`,
  from the chart part:
  - the kind: `barChart` with `barDir`, `lineChart`, `areaChart`,
    `pieChart`, `doughnutChart`, `scatterChart`;
  - `grouping` (stacked, percent);
  - each `c:ser` with its `c:tx` name and its colour from `spPr`;
  - the cached values (`c:numCache`, `c:strCache`);
  - the categories, the title, the legend.
- **Drawing**: replace the reference `chart()` in atrium-layout:
  - axes with nice tick steps and labels;
  - gridlines;
  - stacked and percent bars;
  - line markers, area fills, pie and doughnut wedges as `Item::Path`
    arcs;
  - a scatter;
  - the legend;
  - data labels when the file turns them on.

  Use the same code for charts in sheets (17c).
- **SmartArt and OLE**: SmartArt (`dgm`) is drawn from its saved drawing
  part (`dsp:drawing`), read as shapes. OLE objects and anything else use
  their fallback picture (`ShapeKind::Fallback`).

### 16d: text in shapes, rotation

- **Rotation**: rotated shapes (`rot`, `flipH`/`flipV`) and vertical text
  (`vert`).
- **The rotation field**: add one to `Item::Text` and `Item::Path`, with
  the rotation centre. Coordinate the field in the PR; the painter
  follows.
- **Autofit**: `spAutoFit` grows the shape to its text.

### Tests

- **Outlines**: for each preset in the list, the path's bounding box
  equals the frame, and a few adjust values move the right points.
- **A fixture deck**, built in PowerPoint or WPS, with:
  - grouped shapes, a picture with crop, gradient fills;
  - a styled table with merged cells;
  - a column chart and a pie chart;
  - a master with a logo.

  Assert the model.
- **Layout**: `layout_deck` on the deck gives the expected number of
  `Path`, `Picture` and `Text` items per slide. Hit tests land in table
  cells.
- **Round trips**: edit a table cell's text, save, reopen. The rest of
  the slide's XML is byte for byte the same.

## Item 17: sheets

Do it in three PRs.

### 17a: read styles and layout

- **Styles**: `styles.xml` → `Workbook::styles`:
  - `cellXfs` with their `fonts`, `fills` (pattern `solid` → `fill`;
    gradient fills → their first stop), `borders`, `alignment` and
    `numFmtId`/`numFmts`;
  - the built-in number formats, including the Chinese locale's built-ins
    (ids 27–36, 50–58) for date and currency;
  - theme and indexed colours, with `tint`, resolved.
- **Cells**: every `<c s="…">`, including empty styled cells, gets its
  `Cell::style`.
- **Rows and columns**:
  - `cols` → `SheetLook::columns`, `hidden_columns`, `column_styles`;
  - `row ht`/`hidden`/`s` with `customFormat` → `rows`, `hidden_rows`,
    `row_styles`;
  - `sheetFormatPr` → the defaults.
- **Converting widths**: column widths in characters become points with
  the workbook's default font's maximum digit width:
  `points = trunc((chars × digit + 5) / digit × 256) / 256 × digit × 0.75`,
  where `digit` is 7 px for Calibri 11. Add a table for the common
  default fonts, including 宋体 and 等线.
- **Sheet view**: `sheetView` → `frozen` (`pane` `xSplit`/`ySplit` with
  `state="frozen"`), `gridlines` (`showGridLines`), `zoom`
  (`zoomScale`); `tabColor`.
- **Drawings**: `drawing` parts (`xdr:twoCellAnchor`, `oneCellAnchor`)
  → `drawings`, with pictures, charts (shared with 16c) and shapes.
- **Conditional formats**: `conditionalFormatting` → `conditional`:
  - `cellIs`, `containsText`, `top10`, `aboveAverage`, `duplicateValues`
    / `uniqueValues`, `colorScale`, `dataBar`;
  - their `dxfId` style, read as a partial `CellStyle`;
  - `priority` and `stopIfTrue`;
  - anything else → `CondKind::Other`.

### 17b: show and write back

- **`Workbook::shown`**: complete it:
  - apply the number format to the value, for `General`, decimals,
    thousands separators, percent, scientific, fractions, currency with
    symbols, colours and sections (`[Red]`, `;` sections), text `@`, and
    dates and times in Excel's serial system, including `yyyy"年"m"月"d"日"`,
    `[$-804]`, AM/PM and `上午/下午`;
  - then the conditional rules in priority order. A colour scale
    interpolates the fill; a data bar fills `bar`.

  The text for the grid comes only from here; `Cell::display` stays the
  file's cached text for formulas until Excel recalculates.
- **`SheetOp::SetStyle`**: write the cell's `s` attribute. Add the style
  to `cellXfs` (and to `fonts`/`fills`/`borders`) when it is new; reuse
  existing records when equal. Its inverse puts back the old index.
- **Formatting a range**: add `SheetOp::SetStyles { sheet, from, to,
  style }`, or apply `SetStyle` per cell in one undo step (the UI
  batches). Say which in the PR.
- **Sizes and frozen panes**: add `SheetOp::SetColumnWidth`,
  `SetRowHeight` and `SetFrozen`, and write them back.
- **Edited values**: typing a value into a styled cell keeps its style
  (`parse_input` already keeps it). Its display then follows the number
  format, so typing `0.25` into a percent cell shows `25%`.

### 17c: charts and pictures on sheets

Charts in drawing parts use 16c's reading and drawing. Add a function in
atrium-layout that gives the items for a sheet's drawings, placed from
their anchors with the column widths and row heights, so the grid can
paint them over the cells.

### Tests

- **Fixtures**: a workbook from Excel and one from WPS, with:
  - fonts, fills, borders and merged cells;
  - every number format family above, with expected `shown` text;
  - hidden rows and columns, frozen panes;
  - each conditional kind with expected styles or bars;
  - a chart and a picture.
- **Round trips**: format a cell, save, reopen, then `shown` is the same.
  `styles.xml` gains only the new records; everything else is byte for
  byte.
- **Widths**: the default fonts' widths compare with the values Excel
  reports.

## Done when

- **Fixtures**: the fixture deck and workbooks show what PowerPoint and
  Excel show. The owner compares them on real machines, and the
  acceptance matrix gains a row for each.
- **Round trips**: formatting a cell, and editing a slide table's text,
  survive a save and reopen.
- **Checks**: `cargo test --workspace` and clippy are clean.
