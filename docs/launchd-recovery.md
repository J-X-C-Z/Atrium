# macOS service failure recovery

This follow-up is based on `faac71a5d19815aa09c91d9e48b04717153c5b87`.
It changes only the macOS service-management path, with a scripted runner used by
Unix unit tests. Running tests does not execute launchctl or install a service.

## Recovery rules

- Install never overwrites an existing plist. It first requires a positively
  absent job; loaded or unknown state stops without creating a definition.
- If bootstrap fails (or reports success without loading), cleanup is allowed
  only when a subsequent probe positively confirms absence and the published
  definition still has its original device, inode and contents. Retry can then
  create a fresh definition.
- Loaded or uncertain postconditions retain the definition for diagnosis. A
  failed bootstrap never triggers a blind bootout.
- Uninstall recognizes the exact generated template, including the legacy
  unmarked template. It does not trust marker comments alone. An absent job can
  have its unchanged stale definition removed without bootout, permitting
  recovery from a failed older install.
- A loaded job can be unloaded only with a matching reported definition path,
  an owned template, and an unchanged snapshot immediately before bootout.
  Even if bootout returns an error, confirmed subsequent absence permits
  cleanup. Loaded or unknown results retain the file and return an error.
- Symlinks, foreign definitions, altered contents and replaced inodes are not
  deleted. Cleanup stages and revalidates before deleting. If a replacement
  wins that race it is retained at the staging path printed in the error, not
  restored over a possibly newer destination. Inspect the recovery path before
  making any manual changes.

## Deliberately conservative limits

The real adapter uses `/bin/launchctl` with child-only `LC_ALL=C` and strict UTF-8
parsing. It classifies absence only for exit 113, the exact missing-service line
for the requested label and GUI user domain, and a successful domain probe.
Other diagnostics, command errors and unavailable paths are unknown. This CLI
format is not a guaranteed stable API and has **not been validated on a live
Mac in this follow-up**; an unrecognized version fails closed rather than
removing a definition. Loaded jobs with unparseable paths cannot be unloaded.

Filesystem snapshot checks and staged cleanup are not a transaction against
uncooperative concurrent same-user writers. A same-UID actor can race pathname
resolution by launchctl or the final staged removal. Do not manage the same
service concurrently; this is accidental replacement protection, not a sandbox
against another process with the user's full filesystem permissions.

This does not add node-restart process recovery, cross-process exactly-once
project transactions, automatic launchd installation, or a GUI change.

## Isolated validation

On Linux with Rust 1.100.0:

- `cargo test --locked --offline -p atrium-node`: 47 unit tests and one protocol
  integration test passed; two explicit native mock-agent tests remain ignored
  in this run. Sixteen of the unit tests exercise this recovery path.
- `cargo clippy --locked --offline -p atrium-node --all-targets -- -D warnings`:
  passed.
- `cargo clippy --locked --offline -p atrium-node --target aarch64-apple-darwin --all-targets -- -D warnings`:
  passed (cross-compilation only, no macOS runtime).
- `cargo fmt --all -- --check`: passed.

Fixtures cover failed-bootstrap retry, loaded/unknown preflight and aftermath,
failed-bootout absence recovery, legacy cleanup, original/replaced/in-place
edited/missing files, same-content replacement inodes, symlinks, foreign and
wrong-path jobs, diagnostic classification, replacement during probing, and
comment/duplicate-key/extra-command ownership decoys. Real launchd acceptance
and the unchanged published PR's CI do not validate this unpublished patch.
