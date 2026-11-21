# Ely GPUI Components, as Atrium builds it

A copy of [Ely GPUI Components](https://github.com/ZacharyZhang-NY/Ely-GPUI-Components)
at revision `5d6efd46ec0c93b381ec7578561a1273c366025b` (MIT, see `LICENSE`),
used through `[patch]` in the workspace's `Cargo.toml`.

Atrium's changes, meant to go upstream:

- Chat and agent components read their copy through `crate::i18n::text`
  instead of fixed English, with English and Simplified Chinese entries in
  `src/i18n/builtin.rs`: conversation list, welcome greeting, composer,
  thinking and error rows, message actions, permission prompt, tool calls,
  file changes.
- `ToolCallCard::expanded` and `ThinkingBlock::expanded` open or fold the
  card regardless of the reader's last click, for a "verbose" view of a
  whole conversation.
- `typography::fit` joins line breaks before shaping, so `Ellipsis` and
  friends no longer panic on multi-line text (such as a multi-line command
  in a tool call's summary).

Examples and the web gallery are left out of this copy.
- `typography/fit.rs`: one-line labels (`Ellipsis`, `MiddleEllipsis`,
  `EllipsisTooltip`) join a multi-line text into one line, trimming each
  line, for both the painted line and the hidden text that sizes the box.
  Before, a multi-line command kept one blank line of height per line.
- `documents/blocks`: `BlockEditor::keyed_blocks` gives each block with its
  key, so an owner can map edits onto its own model; `set_locked` makes a
  block read-only (Atrium shows Word paragraphs it cannot edit this way).
- `chat/conversations.rs`: `Conversation::status` (`ConversationStatus`:
  idle, working, waiting on the user, or a count of unread replies) shows a
  spinner, a dot or a count at the end of the row.
