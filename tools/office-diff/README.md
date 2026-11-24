# office-diff

Renders every file in a corpus with **LibreOffice headless** and with
**Atrium's own layout engine**, compares the pages, and writes an HTML report
with side-by-side pages, a difference image and a score per file. It is the
tool behind side lane 35 (`docs/parallel-lanes.md`).

```
corpus/*.docx|.pptx|.xlsx
        │
        ├── soffice --headless --convert-to pdf ──▶ PDF ──▶ PNG + text from glyph runs
        └── atrium-office + atrium-layout ────────▶ items ──▶ PNG + words with positions
                                                          │
                                             report/index.html, report.json
```

## Usage

```bash
# one-time: Linux with LibreOffice
sudo apt-get install -y libreoffice fonts-noto-cjk fonts-liberation

python3 tools/office-diff/corpus/generate.py --out tools/office-diff/corpus/out
cargo run -p office-diff -- \
    --corpus tools/office-diff/corpus/out \
    --report target/office-diff-report
```

Options:

| Flag | Meaning |
| --- | --- |
| `--corpus DIR` | where the `.docx`/`.pptx`/`.xlsx` files are (default `tools/office-diff/corpus/out`) |
| `--report DIR` | output directory (default `target/office-diff-report`) |
| `--soffice PATH` | the LibreOffice binary (default `$SOFFICE` or `soffice`) |
| `--width PX` | raster width of every page (default 900) |
| `--filter TEXT` | only files whose name contains `TEXT` |
| `--atrium-only` | skip LibreOffice and render only Atrium's pages |
| `--fail-under SCORE` | exit 2 when any compared file scores below `SCORE` |

`--atrium-only` is useful on machines without LibreOffice (such as the macOS
development machine this lane started on). Files then show
`not compared`; **no score is invented**. The report says LibreOffice was not
available.

## What is compared

* **Page count** — a page missing on one side scores zero, so the file score
  falls with the count mismatch. `page_penalty = min / max`.
* **Text positions** — words with their positions from both sides. Atrium's
  come from `Item::Text` and the shaper; LibreOffice's come from the PDF glyph
  runs (`hayro`), including their transforms. Matching is case-insensitive and
  greedy nearest-neighbour; the report shows multiset F1, the matched fraction
  and the mean normalized distance.
* **Perceptual image diff** — both pages are sampled onto a 32 × 32 luminance
  grid normalized by page size; the mean absolute difference is reported as
  `perceptual_diff`. The red heat map in the report amplifies the difference.

Page score = `100 × (0.55 × image + 0.45 × text)`, where
`text = 0.5 × content_f1 + 0.5 × position`. File score is the mean of page
scores over `max(atrium_pages, lo_pages)`. These weights are deliberately
simple and are visible in `src/diff.rs`.

## The corpus

`corpus/generate.py` writes the three files with the Python standard library
only (raw OOXML; no `python-docx`). It covers:

* **Word** — Chinese and English body text, a title, headings, bullet,
  decimal and Chinese-numbered lists, a 38-row table whose header repeats
  across pages, two sections with one and two columns, a header with `PAGE`
  and `NUMPAGES` fields, a footer, floating and inline pictures, and a
  footnote.
* **Slides** — a title, preset shapes (rect, ellipse, roundRect, triangle,
  arrows, chevron, cloud, star), a connector with a dash and arrow head, a
  group, a bulleted text box, a picture, a table with a merged cell, column
  and pie/line/bar charts, a master with a logo, and a gradient background.
* **Sheets** — fonts, fills, borders, merged cells, hidden rows and columns,
  frozen panes, gridline and zoom settings, number-format families (percent,
  currency, date with Chinese suffixes, fraction, scientific, red negatives,
  thousands separators, AM/PM), conditional formats (cellIs, containsText,
  top10, color scale, data bar), a picture, a chart and a text box.

### Licence and provenance

The generated files are **CC0-1.0**. The text is original except two short
public-domain excerpts: Li Bai's 《静夜思》 (Tang dynasty) and the proverb
“Slow and steady wins the race.” No private document is used.

The corpus output directory is ignored by git; the files are generated in CI
and locally.

## Bugs this lane found and fixed

Each was reproduced with the corpus or a fixture and has a regression test:

| Bug | Fix | Test |
| --- | --- | --- |
| Bullet lists drew no bullet at all: `numFmt=bullet` produced `Bullet("")` and discarded `lvlText`. | The numbering loader keeps the level's `lvlText` as the bullet mark. | `atrium-office`: `a_bullet_level_keeps_its_lvl_text` |
| Every preset/custom slide outline was built in points while the contract is twips, so shapes filled a few points instead of their frame. | The binding passes the frame × 20 to `slides_geom` and `flip_path`. | `atrium-office`: `preset_outlines_are_in_twips_inside_their_frames` |
| Only the last slide's pictures stayed on disk: a per-slide temp folder was dropped when the next slide loaded. | One media folder for the whole deck, owned by the binding. | `atrium-office`: `pictures_on_every_slide_stay_on_disk` |
| Group members were placed in group coordinates when the resolved frame was written back, ignoring the group offset and scale. | Apply the group map to the resolved frame before assigning it. | `atrium-office`: `group_members_are_placed_in_slide_coordinates` |

The tool itself also keeps the open `Document` alive while rendering (its
extracted pictures are files in a temp folder) and carries the text pen
across script groups, so mixed Chinese/Latin runs do not draw on top of each
other.

## Limitations

* **Footnote text.** The Word binding reads the reference and its number, but
  it does not load `footnotes.xml` into `WordDoc::footnotes()` yet, so the
  layout engine cannot print the note body. The corpus therefore shows the
  reference on the page and no note at the bottom; this is a known gap, not a
  LibreOffice difference.
* **Fonts.** LibreOffice substitutes Calibri/宋体 with whatever the system has;
  Atrium measures with `fontdb`/`rustybuzz`. Different faces move text even
  when the layout is right. Install Noto CJK and Liberation fonts in CI for a
  stable baseline.
* **Sheets.** Excel's print settings (scale, fit-to-width, print area) are not
  in `atrium-office`'s model yet. The tool pages sheets on A4 at 100 % with
  default margins, breaking at cell boundaries, and draws cells from
  `Workbook::shown`. Page boundaries can therefore differ from LibreOffice's
  even when the cell styles match.
* **Scoring is a heuristic**, not a specification. Use the side-by-side pages
  and the per-page numbers to judge; use the score to order files.
* Atrium's pages are rasterized by a small `tiny-skia` renderer in
  `src/render.rs` that draws outlines from the same system faces the shaper
  measured. It is meant for comparison, not for editing.

## CI

`.github/workflows/office-diff.yml` runs nightly and on demand on Ubuntu:

1. installs LibreOffice and Noto CJK/Liberation fonts from apt;
2. builds `office-diff`, generates the corpus, runs the comparison;
3. uploads `target/office-diff-report` as an artifact.

A `fail_under` input is available for on-demand runs; the nightly run does
not fail on the score, only on a tool or renderer error.

Without LibreOffice, `cargo test -p office-diff` still exercises the other
half of the bridge: a known PDF is rasterized and its glyph positions read
back (`a_known_pdf_yields_pages_and_text_positions`). Only
`soffice --convert-to pdf` itself needs LibreOffice.
