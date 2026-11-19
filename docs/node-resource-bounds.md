# Node retained-output and replay bounds

This follow-up to `faac71a` adds a shared retained-output budget and paged replay,
without changing opaque agent messages or the protocol version.

## Implemented

- Existing `keep_output_bytes` remains per process, including its legacy effective
  minimum needed for one maximum-sized line. `total_output_bytes` adds a shared
  ceiling, default 128 MiB. Old config files do not need a new field.
- Accounting counts retained UTF-8 stdout/stderr payload plus 128 bytes per frame.
  When over budget, the largest consumer loses its oldest frame first. Every
  eviction is a sequence prefix and the existing `gap` message describes loss.
- A fixed-shape terminal record survives eviction. Removing the final process
  handle releases that ring's accounted allocation; exit alone retains history
  until existing retention cleanup.
- Replay selects up to 256 KiB of accounted frames per page, with one legal
  oversized first frame allowed so large messages remain usable. Cheap shared
  references are captured under the store lock; payload cloning happens outside.
- The server drains remaining pages without waiting for new output. Only sent
  sequence numbers advance the cursor. Terminal closure happens after the final
  page, and `after=-1` still skips historical output but returns an ended exit.

## Verified in isolated Linux tests

- 52 node unit tests and 2 protocol integration tests passed, including small
  budgets shared across multiple producers, consumer eviction, allocation
  release, legacy config compatibility, terminal survival, paged gaps and large
  first frames.
- Two native processes each retain 500 KiB of history, exit, then replay over
  concurrent WebSockets. Tests verify all five data frames in order and exit as
  frame six, followed by an `after=-1` attachment receiving only exit.
- Both explicit Codex/ACP mock-client tests passed. Existing client/core suites
  passed 46 tests (two remain ignored in the ordinary command); the existing
  NodeClient acceptance was additionally run successfully against a foreground
  isolated node at port 7382. Three demo-helper tests passed.
- Strict native Linux and aarch64-macOS-target Clippy passed. The latter is a
  cross-compile, not execution on macOS. No real provider account, tailnet peer,
  GUI, or service-manager installation was used by these checks.

Commands (locked dependency versions, Rust 1.100.0 in this cloud run):

```sh
cargo test --locked --offline -p atrium-node
cargo test --locked --offline -p atrium-node --test agents -- --ignored
cargo test --locked --offline -p agent-rpc -p codex-client -p acp-client -p atrium-core
cargo clippy --locked --offline -p atrium-node --all-targets -- -D warnings
cargo clippy --locked --offline -p atrium-node --target aarch64-apple-darwin --all-targets -- -D warnings
python3 scripts/test_prepare_node_demo.py
```

## Not a whole-process memory cap

Neither limit is a process RSS ceiling. Value/container overhead, unused vector
capacity, partial stdout/stderr reader buffers, stdin queues, JSON encoding,
socket buffers and active replay copies are separate. A legal large frame may
exceed one replay page. Repeated concurrent attachments and per-process input
queues are not subject to a new global admission/input budget in this change.
An allocator may retain freed capacity; an RSS plateau alone is not proof of a
leak or proof that all memory was released.

There is no persisted stdout, node-restart process recovery or automatic command
re-execution. Process history still survives client disconnection only while
that node remains alive. For bounded functional load observations, see
[node-bounded-load.md](node-bounded-load.md); those are not saturation benchmarks
or real model latency measurements.
