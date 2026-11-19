# Headless Workspace timer validation

This acceptance test uses GPUI's TestAppContext without opening a window.
It constructs Workspace using explicit temporary settings, runs the real
two-second project timer, and verifies that the real Codex client sends a
JSON-RPC wake to a UnixStream mock and updates the notice cursor after
acceptance. It does not start a real agent, load user settings, require a
provider account, install system packages, or alter HOME.

Run on a macOS build host:

```sh
cargo +stable test --locked -p atrium-app --bin atrium \
  --features workspace-ticker-test \
  workspace::ticker_test::scheduler_tick_sends_notice_and_commits_rpc_acceptance \
  -- --exact --nocapture
```

The GPUI test is opt-in, executes once, and bounds the wire read and cursor
wait with real-clock deadlines. CI uses the same Workspace package selection and features in both test
steps to reuse the build. The first step runs all pre-existing tests and
skips just this fixture, then the second requires its exact passing test
line, not merely a successful command with zero matches. It does not validate a real
GUI/window, ACP, Codex steer, a Tailnet, or service managers. The Linux cloud
build used for review could not run the test because native fontconfig
development metadata was unavailable; macOS CI is the first execution gate.
