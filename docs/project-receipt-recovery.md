# Completion receipt recovery: bounded single-writer repair

This change affects only the project persistence model. It does not change
Project public method signatures, `project.json`, task file formats, the gateway,
or UI. New completion notices carry a textual `[atrium-completion:...]` suffix.
No model output stream is persisted and no agent command is replayed.

## Reproduced defects before the fix

The isolated Linux model harness loaded the actual `project.rs`. With the two
new regression tests and unchanged production methods, it reported **16 passed,
2 failed**:

1. Replace `project.json` with a directory to force the metadata-save failure
   after the completion notice append succeeds. Restore the original metadata,
   edit the task's Result, reopen, and retry: **2 notices instead of 1**.
2. Successfully persist the completion, archive/delete the task file, rename
   its workstream, reopen: **no pending parent instead of `lead`**, despite an
   unread completion notice remaining on disk.

The former implementation used the entire recomputed display text both as its
receipt identity and for rediscovering pending wakes. That text depends on
mutable task Result/status, workstream name, and the task file still existing.

## Repair

A new receipt identifies the workstream/task lifecycle:
workstream ID, creation timestamp, task key, and parent ID.
Name, Result, status and mutable session binding are excluded. The normal lifecycle
binds one task to a workstream; changing its task key changes its identity. A late
None-to-session binding must not invalidate an already persisted completion.
The persisted `reported` flag retains its existing meaning.

Retries match the identity, not a regenerated summary. Pending wake recovery
matches the stored receipt without reading its task file. Delivery still follows
existing unread/`notices_seen` semantics; marking read suppresses future wakes.

For legacy notices without a marker, compatibility requires the exact `atrium`
sender, a single exact parent recipient, role/name/task-path prefix, and fixed
trailing sentence. New marked notices never fall back to this legacy matcher.
This is not an authenticity mechanism: project files remain locally editable.
Legacy receipts cannot distinguish identical reused names/task paths or recover
a renamed workstream as reliably as the new identity-bearing receipts.

## Verification and boundaries

Run on Linux without GPUI or a real agent/account:

```sh
python3 scripts/test-project-model.py
```

The script creates an isolated temporary Cargo manifest/target, points its
library directly at the repository's real `project.rs`, and runs Cargo offline.
Requires Rust and the dependencies already cached. Final result after review: **21 model tests passed twice consecutively**
through the script itself, including the reproduced defects and recipient/sender/lifecycle
negative matching tests. This is model coverage, not a full app or GUI test.

No cross-process lock, write-ahead journal, or multi-file transaction is added.
Two schedulers writing simultaneously, partial/corrupted append writes, manual
removal of receipt history/workstream metadata, and mutations to lifecycle
identity itself are outside this repair. There is no claim of cross-process
exactly-once delivery or exactly-once execution. Remote delivery acknowledgement
and the already-bound-session-before-kickoff crash boundary remain separate work.

## Follow-up fault review and repeatability

The initial 19-test claim was insufficient. Independent review found that the
PID-named fixtures could reuse residue from a failed run, and identified two
additional cases. After changing all model fixtures to random names reserved by
atomic `create_dir` (never deleting somebody else's pre-existing fixture), the
new tests reproduced **19 passing / 2 failing** before the production fix:

- Receipt append succeeds, saving `reported` fails, then the task is archived.
  Recovery now checks the existing receipt before requiring a task. The existing
  startup recovery hook reconciles receipt-only workstreams before scanning
  tasks, persists `reported`, and returns the parent to wake. Repeating startup
  does not append a second receipt. Missing task **without** a receipt does not
  fabricate a completion.
- A legacy Result contains literal `[atrium-completion:` prose. It is no longer
  mistaken for a new receipt solely because of that substring. Exact identity
  suffix matching and the old fixed sentence suffix distinguish the formats.

Both full script runs now pass **21/21**, and model-test Clippy with `-D warnings`
passes. The fixture helper retries collisions without removing any existing
path, so failed runs cannot contaminate a subsequent run. The independent
harness still does not run the workspace/GPUI event loop; startup recovery is
covered through the exact model method called by that loop.
