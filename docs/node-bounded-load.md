# Native node bounded-load evidence (Linux cloud)

This is a **bounded 120-second functional soak**, not a maximum-capacity benchmark.
It uses an existing **debug** build, eight closed-loop clients capped at 100
iterations/second each, and approximately 1 KiB payloads. No model inference,
real agent accounts, system service installation, or external network is involved.
All node configuration, HOME, roots and mock `/bin/cat` children are temporary.

## Reproduce

Requires Linux `/proc`, Python 3 and `websockets >= 14` (16.0 used here).
Build the node first using the repository's pinned Rust toolchain, then:

```sh
python3 scripts/node_bounded_load.py \
  --binary target/debug/atrium-node --seconds 120 --clients 8 \
  --label baseline --output docs/evidence/node-bounded-load-linux.json
```

The script records the executable SHA-256. Later runs also record current HEAD
and a hash of tracked node source changes; this identifies checkout state, not
proof the executable was built from it. Preserve the corresponding diff when
comparing locally modified builds.

Each client verifies every echo's content and strictly increasing sequence.
Every 250 echoes it closes and reattaches, explicitly replaying the previous
acknowledged frame by cursor. This does not test uncertain delivery of an
unacknowledged input during a network failure.

Resources are sampled every 200 ms from the node's `/proc` records. CPU is
percent of **one core**, and excludes the Python load generator and cat mocks.
The test verifies owned child PIDs disappear, retained rows expire, and open
file descriptors return within two of the baseline. Linux allocator RSS is not
required to return to baseline. This run does not prove absence of all leaks.

## Exploratory existing-binary result (not a controlled baseline)

Raw evidence: `evidence/node-bounded-load-linux.json`.

- Duration: 120.028 seconds; 94,732 verified echoes; 789.25 echoes/s.
- Echo round-trip: p50 0.370 ms, p95 0.749 ms, maximum 53.56 ms.
- Reconnect/replay cycles: 376, all verified.
- Peak sampled node RSS: 12,876 KiB; average CPU 13.36% of one core.
- Peak sampled CPU: 43.16% of one core (200 ms interval, not instantaneous peak).
- Open descriptors: 11 baseline, 43 after load, 11 after stop, 12 after retention.
- All owned children exited and all retained process rows expired.
- RSS after retention: 12,492 KiB, versus 5,560 KiB before load.

The exploratory executable hash identifies what ran, not its exact source commit; the checkout was concurrently
edited and no exact build-time source diff was captured for that existing binary.
Results are **Linux-only** and do not imply macOS or Windows runtime validation.
Debug/local echo latency is not release performance or real LLM latency. Slow
readers, large-frame memory pressure, crash recovery and saturation are separate
acceptance cases.


## Controlled source provenance

A second, formal baseline was rebuilt from an **unmodified git archive** of
`faac71a5d19815aa09c91d9e48b04717153c5b87` into an isolated source directory and
isolated target directory. Both formal binaries use Rust 1.100.0 and
`cargo build --locked --offline -p atrium-node`, debug profile. The final binary
was copied immediately after the local locked offline build completed.
Its manifest captures the tracked source patch and hashes of every node file,
including the new launchd module, along with the Cargo.lock hash. Each measured
binary was copied to a fixed artifact path before starting the soak.

- Formal baseline: `evidence/node-bounded-load-baseline.json`.
- Build manifests: `evidence/node-bounded-load-{baseline,final}-provenance.json`.
- Final source patch: `evidence/node-bounded-load-final-source.patch`.

To reproduce from the saved build artifacts, add `--provenance <manifest.json>`
to the script command. The script rejects a binary whose SHA differs from the
manifest. Without a manifest, checkout hashes are informational only, not
proof of build provenance.

The exploratory result above is retained for transparency and **excluded** from
formal new/old comparisons. Formal runs use identical load parameters and run
sequentially. These are single runs in a shared cloud environment; small deltas
cannot establish statistical improvement or regression.


## Formal baseline versus final result

Final raw evidence: `evidence/node-bounded-load-final.json`.

| Metric | Clean faac71a baseline | Global budget + paged replay final |
| --- | ---: | ---: |
| Duration | 120.028 s | 120.026 s |
| Verified echoes | 94,383 | 94,160 |
| Throughput (800/s offered ceiling) | 786.34/s | 784.50/s |
| Round-trip p50 | 0.367 ms | 0.381 ms |
| Round-trip p95 | 0.897 ms | 1.102 ms |
| Maximum round-trip | 68.93 ms | 72.28 ms |
| Peak sampled node RSS | 12,324 KiB | 13,272 KiB |
| Average CPU (% of one core) | 13.75% | 14.73% |
| Peak sampled CPU (% of one core) | 43.24% | 44.14% |
| Verified reconnect/replay cycles | 376 | 376 |
| FDs immediately after stopping mocks | 11 | 11 |
| FDs after retention cleanup | 11 | 12 |
| RSS after retention cleanup | 11,292 KiB | 12,492 KiB |

Both runs passed every content/order/replay check, expired all retained process
rows, and left no owned mock child PIDs. Their Cargo.lock SHA-256 values match.
The harness SHA-256 is recorded in both reports and matches.

**No performance improvement is claimed.** In this single comparison the final
build had a slightly lower throughput (-0.23%), higher p95 (+0.205 ms), about
948 KiB higher peak sampled RSS, and +0.98 percentage points of one-core average
CPU. This workload is rate limited and small enough that it does not exercise
the global output cap or large replay pagination; it cannot measure their worst-
case memory benefit. The changes add bounds, not a demonstrated speedup here.
Repetitions, a release build, controlled CPU affinity, larger payloads and slow
readers are needed before attributing these deltas or making capacity claims.

Recorded commands (fixed artifact paths abbreviated):

```sh
python3 scripts/node_bounded_load.py --binary <artifacts>/atrium-node-baseline \
  --seconds 120 --clients 8 --label clean-faac71a-baseline \
  --provenance <artifacts>/baseline-provenance.json \
  --output docs/evidence/node-bounded-load-baseline.json
python3 scripts/node_bounded_load.py --binary <artifacts>/atrium-node-final \
  --seconds 120 --clients 8 --label global-budget-paged-final \
  --provenance <artifacts>/final-provenance.json \
  --output docs/evidence/node-bounded-load-final.json
```
