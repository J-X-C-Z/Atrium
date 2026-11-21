# Office file binding on the existing Atrium editor

The existing `atrium-office` document models, op enums, reports and GPUI
integration remain intact. This adds private OOXML adapters behind their
existing `open` and `Document` save APIs; this is not a second editor.
Page/PDF previews belong to the separate `atrium-preview` item.

- ZIP32 open checks size, inflate budget, entry names, actual lengths, CRCs,
  relationship type and OOXML main part. XML is bounded and namespace-aware.
  No-op/undone files retain the original archive bytes; unchanged raw
  entry records are not recompressed by an edited save.
- Word binds the existing paragraph IDs through table cells and changes
  selected text, properties or structure while retaining supported untouched
  runs. Inverse insertion restores the retained original XML where available.
- Excel projects the sparse grid, format, links to styles and formula locks
  into the existing model. A fixed numeric array can be edited at its formula
  anchor; empty XML cache placeholders are retained so Calc can recompute.
- PowerPoint projects ordered slides and ordinary text, then maps changed
  paragraphs back onto their source ranges. Revalidation before save and
  rebasing preserve existing editor IDs for supported structure.

Save in place compares the private observed digest, not a public display
string. Keep Mine still checks for subsequent changes after it observes the
destination. Save a Copy uses the cached source and never overwrites another
path, including when the original was edited externally. Validation failure
does not change the model, digest, pending edits or persisted file. There is
still a race between final source digest read and OS rename for external
noncooperating writers.

Current intentionally conservative exclusions: ZIP64/split archives,
non-UTF-8 XML, signed package modification, unsupported complex Word/PPT
mixed-content transformations, dynamic/shared-array nonanchor mutation,
and fidelity of layouts not representable by the upstream model. External
Office links are not downloaded. MS Office and WPS GUI validation and
whole-workspace checks are separate from this crate's isolated suite.

Current Word image extraction returns bounded, internal PNG media through a
private temporary folder kept alive by the document; external or other
formats are represented as opaque placeholders. Word hyperlink creation,
target changes, removal, and numbering reuse or creation use the existing
public ops. Unsupported URLs and ambiguous content edits fail closed. Full
unsaved inverse restores exact bytes, including auxiliary parts; obsolete
relationship/numbering entries may remain after undoing a previously saved
change, to avoid dropping referenced opaque markup.

Verification: `cargo test -p atrium-office --offline --locked`, crate-scoped
Clippy with warnings denied, and exact archive identity over the 11-document
corpus. Independent array smoke on the public API used headless Calc; further
native app and office2pdf integration checks are reported separately.
