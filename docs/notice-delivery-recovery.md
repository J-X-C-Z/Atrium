# Notice-delivery journal

An optional `notice_deliveries` map in `.atrium/project.json` keeps one
reservation per workstream. Older files without the field still load. **Do
not downgrade while entries are unresolved:** an older writer ignores and
drops the field on save. The journal contains attempt ID, workstream/backend/
session identity, captured start/end cursors, a SHA-256 of the length-framed
parsed notice prefix, and phase. It contains no composed message or notice
body. The notices themselves remain in the existing `notices.md`.

1. Once a backend/session is ready for a real call, prepare a batch and save
   the reservation before dispatch. A second automatic attempt for that
   stream must not dispatch while reserved or held.
2. Confirmed RPC acceptance advances only the captured cursor and removes
   the reservation in one metadata replacement. New notices after the
   capture remain unread. If the prefix, cursor, stream or session can no
   longer be confirmed, the cursor must not advance.
3. Explicit pre-acceptance rejection releases the reservation without marking
   the notices read. A lost response, disconnect, or unclassifiable result
   holds the reservation as uncertain rather than replaying it. At fresh
   Workspace initialization any persisted reservation is uncertain, even
   if a crash actually happened before sending.
4. A live reservation cannot be manually reset. An uncertain entry can be
   explicitly resolved by marking that exact capture received (only if it
   still matches), or by allowing the notices to be retried. Retrying
   notices does not replay any previous manual user text. Ordinary manual
   sends can proceed without attaching held notices.

The cursor and journal are written by the same atomic replacement; memory
updates only after the save succeeds. A save can fail before replacement or
report a directory sync error after replacement. In the latter case the
snapshot on disk might already be the new one: an accepted cursor is safe
if that snapshot survives, and the prior snapshot's reservation conservatively
prevents replay if it survives. There is no multi-writer lock or cross-process
exactly-once guarantee. A build that moves only this model does not change
when Workspace actually commits receipts; dispatch hooks require separate
integration tests.

The headless Project tests cover acceptance, a concurrent append, explicit
rejection with retry, stale callbacks and replaced sessions, edited notice
prefixes, restart, deliberate user resolution, and I/O failures before and
after publication. These tests do not launch GPUI or real providers.

## Mixed-version writers are unsupported, not just rollback

**Do not let old and new writers share the same project directory.** This
includes a concurrently open older desktop, a rollback build, or an old utility
that opens and saves `.atrium/project.json`. Stopping only the new desktop is
not sufficient if another old writer still accesses that project. This rule is
about project-file writers: an older HTTP/WebSocket client connecting to the
node does not itself read or remove this metadata field.

A synthetic Linux release fixture compared the actual `37c86bfe` Project model
(before the journal) with published `30a5c029`:

1. New code saves a Reserved entry for one small notice. Reopening with new
   code confirms it is blocked.
2. Old code opens that project and calls `save`, without even calling
   `mark_read`. Serde ignores the unknown map and its save omits it.
3. New code opens the saved file again: the hold is absent and preparation
   returns Ready for the same unread notice. Meanwhile the original new
   in-memory Project still has its hold.

This reproduces loss of the anti-retry protection and inconsistent in-memory
versus disk state. It does not send a second real command. Concurrent writers
of any version remain outside the single-writer contract; a mixed old writer
additionally cannot preserve the journal even when writes are sequential.
Opening and saving with the old version is **not** a safe migration procedure.
Resolve outstanding delivery ambiguity with the new version and stop all
project writers before any deliberate rollback; old code still lacks the new
failure guarantees after rollback. Do not delete the journal to silence a hold.

The isolated reproduction is in
[evidence/notice-mixed-writer](evidence/notice-mixed-writer): run
`python3 docs/evidence/notice-mixed-writer/reproduce.py` with cached Rust crates.
It uses a temporary synthetic project, reads the old source from Git, and
imports the current source directly. The recorded observation applies to the
revisions above; running it later tests that later source, not automatically
`30a5c029`. There is no project-format version fence or interprocess lock in
this slice, so the application does not automatically enforce this prohibition.
