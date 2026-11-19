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
