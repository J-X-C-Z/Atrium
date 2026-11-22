# atrium-preview

Read-only previews for the Atrium side panel: Word, PowerPoint, Excel, CSV
and PDF. Everything runs in-process, in Rust, on macOS, Windows and Linux.

- **Word, PowerPoint, Excel → PDF**: [office2pdf](https://github.com/developer0hye/office2pdf) 0.11 (Apache-2.0), layout through Typst.
- **PDF → PNG and text search**: [hayro](https://github.com/LaurenzV/hayro) 0.9 (MIT OR Apache-2.0).
- **xlsx, xls, xlsb and ods cells**: [calamine](https://github.com/tafia/calamine) (MIT).
- **CSV and TSV**: the `csv` crate and `encoding_rs` for UTF-8, UTF-16 BOM, GBK and Shift-JIS.

The crate has no GPUI dependency, so it is tested headless. The UI calls
`Previewer::preview` from a background task and `Previewer::find` for search
highlights. Search returns at most 500 hits per call.

Converted PDFs and drawn pages are cached under
`cache_dir/<sha-256 of path, size, mtime and RENDER_VERSION>/`, trimmed to
500 MB by least-recently-used folder. Files over 100 MB, packages that
unpack over 500 MB, and work that takes over 30 seconds are refused rather
than allowed to exhaust Atrium.

Chinese documents use the system CJK font: PingFang/Hiragino on macOS,
Microsoft YaHei on Windows, Noto Sans CJK on Linux.
