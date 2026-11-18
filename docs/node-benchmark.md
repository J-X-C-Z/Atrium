# Native node microbenchmark

Measured 2026-11-18 UTC. This is a baseline of the new Rust backend, **not a claim of improvement** over Hermes, another production backend, or `mock_node`. The repository mock is not functionally/security equivalent and was not benchmarked.

## Reproduce

```sh
cargo build --release -p atrium-node
python3 scripts/bench-node.py --samples 1000 --process-samples 100 --output /tmp/atrium-node-bench.json
```

The Python-stdlib harness requires Unix and `/bin/cat`. It starts only an owned node on an ephemeral loopback port, uses an isolated temporary root and HOME, `/bin/sh`, a benchmark bearer token, and `discover_magpie=false`. Every configured agent, including built-ins, is overridden to `/bin/cat`. It neither invokes real agents/accounts nor installs services. Temporary fixtures are removed; node shutdown reaps its agents. RSS is read with `ps -o rss= -p <owned-node-pid>` only. No machine-wide process scan.

## Machine and build

- Apple M2, arm64, 8 logical CPUs, 16 GiB RAM; macOS 27.2.1.
- rustc 1.98.1, release optimized build; Python 3.14.7.
- Working tree based on `c7e121ff67c4f4d690af4d42873b5631cf77f642`, with uncommitted native-node implementation. Base commit alone does not identify this implementation.
- Measured binary SHA-256: `7e2bcc2169eedce9381b4dbf0196edc3862839cea55a237aa1dc720a7e62da1c`.
- Binary size: 4,360,304 bytes (4.16 MiB); this includes no model or Python runtime.
- Two consecutive executions of the same binary. No CPU pinning, cache flushing, power-mode control or workload isolation. Other development processes may affect timing.

## Method

HTTP/1.1 persistent connection, loopback, bearer authorization, concurrency **1**. Each metadata/read/write case has 50 warmups and 1,000 measured requests per run. Read/write body is 1,024 bytes; reads are warm-cache. Writes include the implementation's same-directory atomic rename and file `sync_all`; this is not an unsynced memory-only write benchmark. No claim of crash durability of the containing directory.

Client-observed latency includes Python HTTP overhead, localhost networking and response-body consumption. HTTP throughput is completed sequential requests divided by wall-clock time, including client validation; it is **not maximum server capacity**. Percentiles use nearest-rank samples, without pooling runs. Spawn and stop are measured as successive phases for 100 `/bin/cat` processes after 5 warmup pairs. POST is process-spawn acknowledgment, not application/model readiness. DELETE waits for node-owned child reap/output cleanup. Process phase reciprocal means in raw output are not independent sustained spawn/stop throughput.

## Results

Ranges below are the two runs' individual statistics, not confidence intervals.

| Operation | p50 (ms) | p95 (ms) | Sequential requests/s |
|---|---:|---:|---:|
| GET `/v1/node` | 0.066–0.068 | 0.100–0.130 | 13,655–14,129 |
| GET `/v1/fs/read`, 1 KiB | 0.112 | 0.125–0.134 | 8,681–8,797 |
| PUT `/v1/fs/write`, 1 KiB | 3.934–3.983 | 5.215–5.413 | 255–266 |
| Spawn `/bin/cat` | 1.493–2.184 | 2.706–3.574 | Not capacity-tested |
| Stop `/bin/cat` until reaped | 105.417–106.342 | 107.399–107.417 | Not capacity-tested |

- Idle RSS: **4,352–4,416 KiB (4.25–4.31 MiB)**. Five immediate readings per run after metadata access and one second idle; all five readings matched within each run. These are snapshots, not a long-duration memory study.
- RSS after workload: **5,888–6,032 KiB (5.75–5.89 MiB)**, including retained exited-process metadata. RSS is node-only, excludes Python driver and transient child memory; not peak aggregate memory.
- Spawn-to-listen startup: **377.98 ms first run / 10.61 ms second run**. No cold/warm-cache attribution has been experimentally isolated, so do not advertise either as a reliable startup percentile.
- Both executions completed successfully, checked response status and read content, and shut down their owned node successfully.

## Interpretation and remaining measurements

Metadata and warm small-file reads are submillisecond on this local machine. Write latency is dominated by stronger file-I/O semantics and cannot be fairly compared with an unsynced mock. Stop latency contains an explicit **100 ms process-group TERM grace period** before KILL/reap; do not remove that safety step solely to improve a benchmark. Whether that delay is avoidable without weakening descendant cleanup needs separate design and regression tests.

Not measured: CPU utilization, peak RSS, long-running retention behavior, maximum concurrency/throughput, large files, WebSocket replay/backpressure, real agent startup/model inference, Tailscale/remote authentication and network latency, Windows/Linux execution, or GPUI. No percentage speedup is defensible without an equivalent production reference under identical workloads and security/durability semantics.

## Raw results

The original raw process `sequential_ops_s` keys below have been renamed to `reciprocal_mean_phase_s` to avoid implying independent sustained throughput. Values are unchanged. The checked-in harness uses that corrected key.

### Run 1

```json
{
  "platform": "macOS-27.2.1-arm64-arm-64bit-Mach-O",
  "machine": "arm64",
  "logical_cpus": 8,
  "python": "3.14.7",
  "binary_sha256": "7e2bcc2169eedce9381b4dbf0196edc3862839cea55a237aa1dc720a7e62da1c",
  "binary_bytes": 4360304,
  "payload_bytes": 1024,
  "transport": "HTTP/1.1 persistent loopback, bearer token, concurrency 1",
  "warmup_requests": 50,
  "machdep.cpu.brand_string": "Apple M2",
  "hw.memsize": "17179869184",
  "startup_to_listen_ms": 377.97862500156043,
  "idle_rss_kib": [
    4416,
    4416,
    4416,
    4416,
    4416
  ],
  "node": {
    "samples": 1000,
    "p50_ms": 0.066,
    "p95_ms": 0.129916,
    "sequential_ops_s": 13654.758893132139
  },
  "read_1k": {
    "samples": 1000,
    "p50_ms": 0.111583,
    "p95_ms": 0.125375,
    "sequential_ops_s": 8797.247609017733
  },
  "write_1k_atomic_sync": {
    "samples": 1000,
    "p50_ms": 3.982833,
    "p95_ms": 5.412666,
    "sequential_ops_s": 255.4087403228611
  },
  "spawn_cat": {
    "samples": 100,
    "p50_ms": 2.183917,
    "p95_ms": 3.574041,
    "reciprocal_mean_phase_s": 443.0024674750136
  },
  "stop_cat_until_reaped": {
    "samples": 100,
    "p50_ms": 106.341959,
    "p95_ms": 107.416959,
    "reciprocal_mean_phase_s": 9.41017792404009
  },
  "post_workload_rss_kib": 6032
}
```

### Run 2

```json
{
  "platform": "macOS-27.2.1-arm64-arm-64bit-Mach-O",
  "machine": "arm64",
  "logical_cpus": 8,
  "python": "3.14.7",
  "binary_sha256": "7e2bcc2169eedce9381b4dbf0196edc3862839cea55a237aa1dc720a7e62da1c",
  "binary_bytes": 4360304,
  "payload_bytes": 1024,
  "transport": "HTTP/1.1 persistent loopback, bearer token, concurrency 1",
  "warmup_requests": 50,
  "machdep.cpu.brand_string": "Apple M2",
  "hw.memsize": "17179869184",
  "startup_to_listen_ms": 10.605041003145743,
  "idle_rss_kib": [
    4352,
    4352,
    4352,
    4352,
    4352
  ],
  "node": {
    "samples": 1000,
    "p50_ms": 0.067625,
    "p95_ms": 0.100083,
    "sequential_ops_s": 14128.783864005769
  },
  "read_1k": {
    "samples": 1000,
    "p50_ms": 0.111958,
    "p95_ms": 0.1335,
    "sequential_ops_s": 8680.596396589746
  },
  "write_1k_atomic_sync": {
    "samples": 1000,
    "p50_ms": 3.934458,
    "p95_ms": 5.215208,
    "sequential_ops_s": 266.2429853297813
  },
  "spawn_cat": {
    "samples": 100,
    "p50_ms": 1.492958,
    "p95_ms": 2.706125,
    "reciprocal_mean_phase_s": 671.9344536003726
  },
  "stop_cat_until_reaped": {
    "samples": 100,
    "p50_ms": 105.416917,
    "p95_ms": 107.399209,
    "reciprocal_mean_phase_s": 9.511556903790634
  },
  "post_workload_rss_kib": 5888
}
```

## Independent Linux validation baseline

One additional isolated run on Debian 13 / Linux 6.18.57 x86_64, Xeon 8573C
cloud allocation (9 logical CPUs), Rust/Cargo 1.100, Python 3.12.14 used the same
harness and sample counts. The filesystem is overlay-backed and shared host
load is uncontrolled. **Do not compare these timings with the Mac as a language
or code improvement.** The measured release binary predates the final equivalent
stdin-budget CAS replacement, so these are not final-binary benchmark claims.

Binary SHA-256: `98f215e9d6bbff4edef241c08d16a9096889dd2706d7c1ddb947e66d35870c63`.
Binary size: 5,083,320 bytes. Idle RSS: 4,200 KiB; post-workload RSS: 5,632 KiB.

| Operation | p50 ms | p95 ms | Sequential requests/s |
|---|---:|---:|---:|
| Metadata | 0.113 | 0.445 | 5,973 |
| 1 KiB read | 0.356 | 2.611 | 1,342 |
| 1 KiB atomic sync write | 0.227 | 0.809 | 3,092 |
| Cat spawn | 1.129 | 4.349 | Not capacity-tested |
| Cat stop | 103.158 | 108.521 | Not capacity-tested |

This is a single Linux microbenchmark run, not a distribution of repeated
runs. It measures no real agent, provider, tailnet or GUI workload. The much
lower observed write latency on overlay storage is not evidence of stronger
persistent-storage performance or durability than the Mac's filesystem.
