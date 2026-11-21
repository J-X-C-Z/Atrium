Synthetic openpyxl 3.1.5 workbook: A1:A3 = 1,2,3, D1:D3 fixed array
`A1:A3*2`; original anchor cache empty, children caches 4 and 6.
No user data. MIT. Source SHA-256:
`440dd53e84eba9a307cbaa198c552001078de175dfff89432eae2e3115219fd6`.

Verification of the Rust-edited `A1:A3*3` file: LibreOfficeDev
26.8.0.0.alpha0 headlessly reopened/re-exported it with fixed array ref D1:D3
and values 3,6,9. Calamine independently checked formula metadata and
the blank-to-recalculated value transition. Not a native Excel GUI check.
