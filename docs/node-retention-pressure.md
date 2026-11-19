# Bounded retention-pressure acceptance (Linux, 2026-11-19)

**Result: both runs completed, with every echo, sequence, replay gap/exit and
cleanup assertion passing.** This validates the shared retention cap and paged
replay under pressure. It is not a fair speed competition, a Rust-language
speedup claim, a maximum-throughput benchmark or a hard RSS-limit guarantee.

## Inputs and provenance

- Baseline: exact `faac71a5d19815aa09c91d9e48b04717153c5b87`, immutable binary
  SHA256 `e47a3771d4bbc809234f411a06f2af8b491b5cd7ff149629ec153a89ec4492dc`.
- New: `de6f66d1569ed274e6a6e67904b06b31783a3e89`, immutable binary SHA256
  `f983a6c27046c07a27f18f5dab9449bdeb1b1d7dfe67b629a5aa3bbb4e1c192c`.
- Both are **debug** builds, Rust 1.100.0, locked offline Cargo build. Exact
  source-file/archive, Cargo.lock and binary hashes are in the build manifests.
- Driver: `scripts/node_retention_pressure.py`. Its SHA256 is embedded in each
  run's raw JSON and listed in `docs/evidence/node-retention-pressure/sha256.json`.
- Raw samples/results: `docs/evidence/node-retention-pressure/{baseline,de6}.json`.
  No model output or real user work was collected; payloads are synthetic `x`.

Linux x86_64 cloud, kernel 6.18.57 / glibc 2.41. The environment has **no mounted
`/sys/fs/cgroup`**, and mountinfo exposes no cgroup mount. Thus `memory.max` and
`memory.current` are unavailable, not unlimited. `/proc/meminfo` showed roughly
9 GiB available before each run. The script requires at least 768 MiB visible
headroom and rechecks during the test. It stops further load/replay and writes
a truncated report if visible headroom falls below 768 MiB, sampled node + owned
cat RSS exceeds 512 MiB, or the monitor fails. Abort shuts down only owned
WebSocket sockets and sends TERM to the owned node to interrupt blocked writers.
Each socket has a 10-second deadline and the monitor enforces a 120-second
whole-round deadline. The 50 ms sampling interval is a target, not a guarantee
or a kernel hard limit; unmounted cgroup quotas cannot be verified here.

## Exact workload and deliberate configuration difference

Each run uses one owned loopback-only node, temporary HOME/config/roots, Magpie
discovery disabled, and all agent commands overridden to `/bin/cat`. Eight cats
each receive 64 frames of exactly 256 KiB, **128 MiB total payload**. Eight
closed-loop senders verify each echo byte-for-byte and each sequence before
sending their next frame; the driver does not retain all payloads in a list.
After all 512 echoes are verified, DELETE stops each cat. Eight concurrent
connections then replay from sequence 0 through the terminal frame.

Both configurations retain 256 lines and 128 MiB per process, with a 30-second
retention timeout. The baseline has **no `total_output_bytes` field** because
that implementation does not support it. The new node explicitly sets
`total_output_bytes=67108992` (64 MiB + 128 bytes). This intentional difference
makes the same admitted output exceed the new shared budget, demonstrating
bounded history and gap semantics rather than claiming equivalent retained
work or faster execution.

## Observations

| Observation | Baseline | New shared cap |
|---|---:|---:|
| Echoes checked | 512 | 512 |
| Retained payload after fill | 128 MiB | 63.75 MiB |
| Replay output frames | 512 | 255 |
| Gap prefixes | None | One 1–33; seven 1–32 |
| Terminal sequence on all 8 streams | 65 | 65 |
| Peak sampled node + owned cat RSS | 267,124 KiB | 107,612 KiB |
| Node + owned cat CPU time, workload interval | 3.77 s | 3.12 s |
| Workload wall interval | 2.417 s | 2.471 s |
| Concurrent replay wall interval | 0.542 s | 0.275 s |
| RSS after record expiry | 65,884 KiB | 19,332 KiB |
| Node FD count, baseline → after expiry | 14 → 12 | 14 → 12 |

The new retained frames plus 128 bytes/frame accounting are below the shared
cap. Every replay contains at most one initial contiguous `gap`, followed by
strictly contiguous retained sequence numbers and an exit at 65. A different
thread schedule may distribute the final 255 frames differently; the result is
not specified as a per-process exact quota.

All eight owned child PIDs disappeared after stop. All process records expired
within the bounded wait. Node FDs returned near baseline. RSS need not return
to the initial value because allocators may retain freed pages. The smaller
new replay is deliberately less work, so its lower replay duration/CPU must
**not** be advertised as a fair speed improvement.

## Measurement limits and discarded attempt

RSS/CPU samples inspect only the spawned node PID and eight PIDs returned by
its spawn API; node FD enumeration uses only the node PID. RSS excludes the
Python driver, and scheduled sampling may miss transients. CPU totals retain the
last observed ticks for exited cats, so exit-tail ticks between samples may be
missed. Shared-cloud scheduling and overlay storage are uncontrolled. No real
agents, services, credentials, unrelated PIDs, GUI, macOS or Windows were used.

An earlier exploratory baseline attempt hit a `/proc/<exiting-cat>/fd`
permission race in the monitor. Its workload finished, but its resource data
was **discarded**, not reported as a valid benchmark. The driver was corrected
to enumerate FDs only for the node and to propagate monitor failures to the
abort path; both evidence runs above use that corrected, hashed driver.

Reproduce sequentially, never run both pressure cases simultaneously:

```sh
python3 scripts/node_retention_pressure.py --binary /path/to/atrium-node-baseline \
  --provenance /path/to/baseline-provenance.json --output /tmp/pressure-old.json
python3 scripts/node_retention_pressure.py --binary /path/to/atrium-node-de6-pressure \
  --provenance /path/to/de6-pressure-provenance.json --global-budget \
  --output /tmp/pressure-new.json
```

Safety paths were exercised with the same final driver: an intentional monitor
failure produced `truncated_or_failed`, interrupted owned sockets/node, and left
no owned PID paths after cleanup. Reusing an existing output filename failed
before starting a node and preserved the original bytes. The driver exclusively
creates an `initializing` output before preflight, so an early preflight failure
cannot be mistaken for a previous completed report. See `safety-checks.json` and
`injected-failure.json` in the evidence directory.

Actual maximum consecutive sampling gaps: baseline 168.7 ms; new 276.1 ms. These measured gaps further limit transient-peak claims.
