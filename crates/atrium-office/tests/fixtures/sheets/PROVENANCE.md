# Sheet fixtures

`excel-style.xlsx` and `wps-style.xlsx` are synthetic OOXML packages built by
`generate.py` using only the Python standard library. They follow the parts,
styles and drawing/chart conventions Excel and WPS emit, but **they were not
produced, opened or validated by Excel or WPS** on a real machine.

- `excel-style.xlsx`: Calibri/宋体 fonts, solid and theme-tinted fills, thin
  and double borders, merged cells, styled empty cells, custom and Chinese
  built-in number formats, hidden rows/columns, frozen panes, gridlines off,
  zoom, tab colour, every conditional-format kind in the backlog brief, a
  picture, a column chart and a text shape.
- `wps-style.xlsx`: the same content with WPS-style indexed colours and a
  different theme/font scheme, used to check indexed-colour resolution and the
  Chinese built-in formats independently of the Excel-style workbook.

Regenerate both files with:

```sh
python3 crates/atrium-office/tests/fixtures/sheets/generate.py
```

`excel-style.xlsx` is also the base for the formatting round-trip tests in
`tests/sheets.rs`.
