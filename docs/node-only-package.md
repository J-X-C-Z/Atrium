# Native node packaging

The packaging scripts build unsigned `atrium-node` archives and run local
smoke tests.

## Build, verify, smoke

Requirements: Python 3.11+, the repository's Rust toolchain and cached Cargo
dependencies; Linux `readelf`, or macOS `otool`; `/bin/cat` for smoke.

```sh
python3 scripts/node-package.py build \
  --output /your/new/output/atrium-node.tar.gz \
  --target-dir /your/independent/build-cache
python3 scripts/node-package.py verify /your/new/output/atrium-node.tar.gz \
  --expected-sha256 THE_HASH_RETURNED_BY_BUILD
python3 scripts/node-package.py smoke /your/new/output/atrium-node.tar.gz \
  --expected-sha256 THE_HASH_RETURNED_BY_BUILD
python3 -m unittest discover -s scripts -p test_node_package.py -v
```

The output parent must already exist. Existing output files and symlinks are
refused, not overwritten or removed. Native `cargo build --release --locked
--offline -p atrium-node --bin atrium-node --target <rustc host>` builds into
the separate target directory. No cross-build is inferred from a filename.
The verifier checks ELF/Mach-O CPU headers against the target triple; smoke
refuses a different host OS/architecture before starting any process.

## Exactly two archive entries

- `bin/atrium-node`
- `manifest.json`

There are no tokens, user settings, sample configs, mocks, source checkout,
`target` tree, launch agents or installer scripts in the archive. Manifest
fields include package version, native target, Rust toolchain, source commit,
dirty-tree flag, Cargo.lock hash, binary SHA-256/size/header architecture, and
statically inspected dynamic dependencies. Source dirty state is disclosed,
not represented as a clean/tagged release. This repository has no root LICENSE
file to bundle; this slice does not manufacture license text or satisfy a
public-distribution license-notice audit.

Tar owner/time fields and gzip timestamp/name are fixed. Repacking the same
binary and metadata produces byte-identical archives. This **does not claim
reproducible Rust binaries across different compilers, OS images, flags or
source trees**. The returned archive and manifest hashes verify integrity,
not publisher authenticity. An attacker who can replace both binary and
manifest can recompute their hashes; use a separately trusted expected archive
hash. The package is unsigned. Verification never extracts archive-provided
paths, symlinks or hardlinks. Only the exact canonical two-entry USTAR encoding emitted by this script is
accepted. Raw 512-byte headers are checked before any extension processing;
PAX/GNU extension records are rejected without reading their payloads. Member
bodies and the terminal zero-padding read are bounded, including one-byte
trailing-data detection. Generic tar repacks are intentionally not supported.
Inputs are opened once with O_NOFOLLOW/O_NONBLOCK, checked with fstat and read
with a limit plus one byte, so final-symlink replacement, FIFOs and concurrent
file growth cannot bypass these checks. This is not protection against a
malicious same-user mutation of all source/toolchain files; build directories
and tools must be trusted. Dynamic-library inspection uses the build path.
On output failure a partial exclusive-created file is left for inspection;
there is no stat/unlink cleanup race that could delete a replacement. Select a
new output path on retry.

## Foreground smoke isolation

Smoke verifies the archive first, creates a private temporary HOME/config/work
root, and executes only the verified node with `serve --config`. Configuration
uses random bearer auth, `port=0`, `loopback_only=true`, and
`discover_magpie=false`. Every configured agent is overridden with `/bin/cat`;
no real agent/account is used. Ambient HTTP proxy settings are ignored.

Checks cover missing-token 401, authenticated node version/protocol, file
roundtrip, outside-root rejection, an owned cat process POST/DELETE lifecycle,
and clean node shutdown. Requests and startup have deadlines, including an
incomplete startup announcement. No WebSocket or end-to-end model/GUI acceptance
is claimed. Cleanup signals only the Popen node this run created; node graceful
shutdown is responsible for its spawned cat. If graceful cleanup exceeds its
deadline, the owned node is killed and the run fails, not reported as accepted;
that failure does not establish that all descendant resources were reclaimed.
No process-name scans or arbitrary PID kills occur.

## Linux evidence for this candidate

Isolated base: `04a3e16931ee774173c6e6d8c8651beec63fc852`, version `0.1.0`.
Built natively with rustc `1.100.0`, target `x86_64-unknown-linux-gnu`.

- Binary size: 5,129,040 bytes.
- Binary SHA-256: `c775cb47254fcdff03be1d91ffab1f94ccfa09de8f8f3c3395aa1cbdee84483f`.
- Archive size: 1,748,188 bytes.
- Archive SHA-256: `c462ce523c28eeff90de7edda579056c0dc62cad8d45126041b5b96b4698f799`.
- Manifest SHA-256: `415f99671655b4068cf411a8e8a420ade0b1666a923ffa889b9f95c7c5e13791`.
- Twelve safety/reproducibility fixtures pass; a real same-input repack is
  byte-identical. All six native foreground smoke checks pass.

This Linux binary is **not static**. Readelf records dependencies on
`libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, and `ld-linux-x86-64.so.2`.
Versioned symbol requirements include `GLIBC_2.39`; do not advertise this build
as compatible with older glibc distributions. Different deployment baselines
need their own build and smoke. No musl portability claim is made.

The script has native Mach-O header and `otool -L` handling, but **no macOS
build/smoke, minimum-macOS validation, signing/notarization, launchd install,
second-device tailnet test or desktop interaction was performed here**. Those
remain separate delivery acceptance steps. This candidate is not published.

## Windows x64

From a Windows x64 machine with MSVC Rust and PowerShell 7.4 or later:

```powershell
./scripts/node-package-windows.ps1 -Output ../atrium-node-windows-x64.zip
```

The script builds a release executable and archives it with a source and SHA-256
manifest. It extracts its own archive, checks the binary hash, and runs the
native protocol suite against the extracted executable. It writes the ZIP only
after these checks pass, and refuses an existing output path.

## CI

The node package workflow runs on macOS, Ubuntu and Windows. The macOS and Linux
jobs verify deterministic repacking, foreground smoke and bounded load. The
Windows job verifies its ZIP and runs the native protocol suite. Request the
`paced-baseline` input when dispatching the workflow to run the two duration-based
macOS measurements. See the [acceptance matrix](backend-acceptance-matrix.md#cross-platform-follow-up)
for runtime results.
