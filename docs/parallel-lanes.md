# Side lanes (items 33–37)

Work that does not touch the T3 port (items 20–32) and can run while a port
PR waits for review or a real-machine check. Each item is one PR. Checks
and rules are those in `AGENTS.md`.

## 33. Team: close the known gaps (M)

`crates/atrium-team` and `crates/atrium-app/src/workspace_team.rs` (see
`docs/team-implementation.md`). Today:

- a member's `model` and `effort` in `team.toml` are not applied when its
  task starts — pass them to the session (Codex `model`/`effort`, ACP
  config options by category);
- task-pool workers are not placed through `atrium_team::pick`, so a spent
  member's pool keeps starting it — route `workers_needed` starts through
  `pick` like leads;
- the "Stand-in · {member}" badge is lost on restart — persist which
  member a stream stands in for (app state, not `.atrium/`);
- a stand-in on an ACP limit happens only with the autopilot on — mark the
  member spent from the failed turn and restart the task on the stand-in
  whenever the team is on.

Tests: pure tests for each rule; one e2e with mock-acp failing with a limit
message and the task restarting on the stand-in.

## 34. Soak and footprint (M)

The owner chose Rust partly for memory and 24-hour stability. Measure it.

- `tools/soak`: run the app headless (Xvfb) with mock agents for a project
  under the autopilot for N hours (default 2 in CI-like runs, 24 on the
  owner's machine): steady turns, tool calls, notices, device link drops
  and reconnects (mock-node killed and restarted).
- Record RSS, open files, threads and CPU every minute for the app and
  `atrium-node`; fail on growth beyond a threshold after warm-up; write a
  CSV and a short report to `docs/evidence/`.
- Same measurement for the T3 binary pinned in `docs/t3-spike.md` with the
  same mocks, so the two can be compared once the engine lands.
- Fix leaks you find (unbounded maps, transcripts kept for closed threads,
  tasks not dropped).

## 35. Office: compare against LibreOffice on real files (L)

`crates/atrium-layout`, `crates/atrium-office`, `crates/atrium-preview`.

- `tools/office-diff`: for each file in a corpus, render pages with
  LibreOffice headless (`soffice --headless --convert-to pdf`, then
  rasterize) and with Atrium's layout; compare per page (page count, text
  positions from both sides, and a perceptual image diff); write an HTML
  report with side-by-side pages and a score per file.
- Corpus: generate files that exercise the features in
  `docs/layout-engine-implementation.md` and
  `docs/slides-sheets-implementation.md` (Chinese and English text, lists,
  tables across pages, sections and columns, headers/footers, floating
  pictures, footnotes, slide shapes and charts, sheet styles and number
  formats) with a script; add public-domain samples only with a clear
  licence. Nothing private.
- Fix the worst differences, largest first, each with a regression test.
  Run it on Linux in CI (LibreOffice from apt), nightly or on demand.

## 36. Installers and updates (M)

- macOS: a signed and notarized `.dmg` when signing secrets are present in
  CI (skip cleanly when they are not); Windows: an `.msi` or installer
  `.exe`; Linux: an AppImage and a `.deb`.
- `atrium-node` installs as a service on each (launchd, Windows service,
  systemd user unit), with uninstall.
- The app's auto-update reads GitHub Releases for the platform; a
  `release` workflow builds all of the above from a tag.
- Document the steps in `docs/atrium-workspace-setup.md` (Chinese).

## 37. Small cleanups (S)

- The clippy `&PathBuf` warning in `workspace_project_pool.rs`.
- `crates/atrium-memory`: `project_root` now canonicalizes the root it
  finds; check the other places paths become keys (sync, projects, spread)
  for the same macOS `/var` → `/private/var` mismatch, with tests using a
  symlinked temp folder.
- CI runs on pushes to `main` as well as pull requests (today the desktop,
  node and package workflows run only on PRs or by hand).
