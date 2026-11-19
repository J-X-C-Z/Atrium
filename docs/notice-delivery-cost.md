# Notice-delivery persistence cost observation

This is a small **cost reference, not an equal-guarantee speed comparison**.
The reservation path persists before dispatch and commits an accepted cursor
with the reservation removal. The older `mark_read` + `save` path does not offer
that failure boundary. Lower cost for the latter is not a reason to retain its
early-read behavior.

## Captured source and method

[evidence/notice-delivery-cost](evidence/notice-delivery-cost) contains the
portable Rust driver, standalone Cargo manifest and lock, raw nanosecond samples,
environment/source manifest and SHA-256 checksums. The driver imports the actual
repository `project.rs` by path; it does not copy a model implementation.

The measured source is `37c86bfe` **plus unpublished notice-delivery changes**,
identified by exact source hashes in `manifest.json`. It is not a measurement of
the unmodified commit, nor automatically of any later commit. Match those hashes
before attributing these figures to a published revision. The portable driver
was rerun after packaging; these figures supersede the earlier exploratory
absolute-path harness figures. No target directory or executable is included.

- Linux x86_64, kernel 6.18.57, Rust/Cargo 1.100.0, optimized release profile.
- Workspace **overlay** filesystem, not `/tmp` tmpfs. Underlying storage and
  physical durability are unknown; this is not a macOS/APFS observation.
- Independent cached/offline harness dependencies, pinned by its own lockfile,
  not the workspace lockfile.
- One sequential run: reservation path first, cursor reference second. No
  warmup samples discarded, randomization, or repeated statistical trials.
- Each path creates its own temporary Project and one synthetic workstream.
  For each of 100 iterations it appends one fixed-width small notice, then
  either prepares and settles Accepted, or marks read and saves. The log grows
  from 1 to 100 notices, ending at 8,211 bytes on each path. This measures that
  specific growing-prefix workload, not constant-size or large-history cost.
- Append is outside individual operation timings, but inside each round's wall
  total. Prepare includes prefix parsing/hashing, formatting, and persistence;
  settle includes prefix checking and persistence. Neither includes a real RPC.
- p50/p95 use nearest rank, sorted sample indices 49/94. There is no payload
  retention benchmark, resource-peak claim, or guarantee about tail latency.

## Result

| Timed operation | Samples | p50 | p95 | Sum |
|---|---:|---:|---:|---:|
| prepare reservation | 100 | 1.476 ms | 2.056 ms | 150.761 ms |
| settle Accepted | 100 | 1.367 ms | 2.279 ms | 147.975 ms |
| legacy mark_read + save reference | 100 | 1.276 ms | 1.750 ms | 133.792 ms |

Full round wall time including notice appends: reservation path **368.710 ms**,
cursor reference **193.589 ms**. Shared-cloud scheduling, filesystem caches and
fixed order may affect these observations. Do not add separate p95 values and
call that a measured combined p95. No inference about real-provider latency,
macOS performance or equivalent correctness is supported.

## Reproduce

From the repository root, with Rust and the locked dependencies available:

```sh
# Choose a disposable working directory on the filesystem you intend to measure.
work=$(mktemp -d "$PWD/notice-cost-run.XXXXXX")
export NOTICE_COST_FIXTURE_PARENT="$work"
export CARGO_TARGET_DIR="$work/target"
cargo run --offline --locked --release \
  --manifest-path docs/evidence/notice-delivery-cost/Cargo.toml \
  > "$work/result.json"
```

The driver removes only its own UUID-named fixture after success. The selected
working directory retains the build and your new result for inspection. A failed
run may leave its synthetic fixture for diagnosis. Do not overwrite the checked-in
result when reproducing. Verify the packaged evidence with:

```sh
(cd docs/evidence/notice-delivery-cost && sha256sum -c SHA256SUMS)
```

No user projects, provider accounts, service installation, network connections,
credentials, or manual message contents were involved. Dispatch integration and
real macOS acceptance remain separate from this persistence-cost observation.
