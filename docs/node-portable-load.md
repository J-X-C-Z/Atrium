# Portable bounded native-node load acceptance

`scripts/node-portable-load.py` uses Python 3.11 standard library only. It imports
the separately reviewed `node-package.py` verifier and executes only a native
release archive whose supplied SHA-256 matches. The report retains the complete
build manifest (source commit, dirty flag, compiler, target, executable hash,
profile and dependencies), archive hash, driver and helper hashes.

```sh
python3 -m unittest discover -s scripts -p test_node_portable_load.py -v
python3 scripts/node-portable-load.py --archive NODE.tar.gz \
  --expected-sha256 TRUSTED_ARCHIVE_HASH --output NEW_RESULT.json
# Deliberately changed retention boundary, not an optimization comparison:
python3 scripts/node-portable-load.py --archive NODE.tar.gz \
  --expected-sha256 TRUSTED_ARCHIVE_HASH --output NEW_SMALL_RESULT.json \
  --small-retention --rounds 60
```

Two concurrent owned `/bin/cat` processes receive 64 lines of 16 KiB per round.
Each client sends one line then verifies its exact echo and sequence. Each round
reconnects at its last observed sequence. After DELETE, both clients replay from
zero concurrently and verify a single missing-prefix gap (if present), contiguous
retained sequence/payload and terminal exit at the expected final sequence.
Small-retention mode sets only `keep_output_lines=8` and must demonstrate a gap
and exactly eight retained data frames, plus the separately retained terminal
frame. Default mode leaves retention defaults unchanged and requires every data
frame with no gap: even the maximum workload is below its line/byte limits. This is not a global-memory-budget pressure test. Changing modes is
not a fair performance baseline; comparisons require the same mode, binary
profile, offered load and runner, with alternating repeated runs.

The driver keeps at most one payload per client in flight, enforces 64-KiB
receive frames, 8-KiB handshake headers, absolute three-second complete
WebSocket handshake/frame deadlines, three-second socket/HTTP/ps timeouts,
1–60 rounds and a 180-second load deadline. A monitor samples `ps` every nominal
100 ms and terminates only its owned node if observed node+registered cat RSS
exceeds 512 MiB or sampling fails. It records actual sample gaps. This is a
sampled guard, not a hard memory limit or exact peak measurement. Sockets are
registered before handshake; abort shuts down registered sockets and rejects
late registration atomically. HTTP bodies use a three-second absolute loop
deadline plus at most one bounded underlying read. The 180-second guard is
checked after each bounded ps sample, so it is not an exact whole-process kill
deadline. Startup/HTTP setup and final owned cleanup have separate deadlines.
Samples are timestamp-sorted before computing maximum sampling gaps. The custom
WebSocket client intentionally rejects fragmented/control frames: this bounded
fixture uses short active intervals and no negotiated extensions.

Node RSS and cat RSS are separate. Cats must match both registered PID and the
owned node's PPID; no arbitrary processes are signalled. `ps time` is cumulative
CPU, with platform-dependent coarse resolution. The report sums each observed
PID's maximum-minus-minimum sampled CPU and divides by total wall time to give
single-core percent. CPU before the first sample and after the final live sample
is missing, especially exit tails. Wall time includes startup, network and
Python driver work; driver RSS/CPU is not included in node/cat samples. These
values are diagnostic, not a microbenchmark or evidence of speedup.

Every run uses an exclusive output file and private temporary HOME/config/root,
random loopback auth, disabled discovery, false login shell and cat overrides
for all agents. Failure records are not accepted samples. Cleanup uses only the
owned node Popen handle; graceful node shutdown owns child cleanup. As in package
smoke, forced cleanup failure is not proof all descendants were reclaimed.
No services, accounts, UI or tailnet devices are touched.

## macOS acceptance workflow (pending execution)

The package workflow builds once, verifies its native release archive, then
runs nine fixtures and three alternating default/small pairs against that
exact archive/hash, plus the bounded 60-round small-retention run. Keep
results in RUNNER_TEMP and inspect completed status and all assertions. No extra
Python dependency installation is necessary. A hosted ARM64 macOS run validates
that runner, not the user's Mac; it cannot be compared to an old Linux debug run.
The workflow retains its read-only permissions and 30-minute cap. This is
not a claim of macOS execution until that exact-head run succeeds. A 60-round
run is bounded multi-round coverage, not a guaranteed long-duration soak.

## Local evidence (Linux, not macOS)

Historical pre-review driver SHA-256 (not the corrected driver):
`6d76a8da252baa82e0aad43cf2f283e1757682d26d4681d7c45156533230f3e5`.
The package source is `04a3e16931ee774173c6e6d8c8651beec63fc852`, native
x86_64 Linux release, executable SHA-256
`c775cb47254fcdff03be1d91ffab1f94ccfa09de8f8f3c3395aa1cbdee84483f`.
This deliberately reuses an existing verified package, not a claim that the
latest application source was rebuilt. Three alternating default/small pairs
all verified 256 echoes per run. The 60-round small-retention run verified 7,680
echoes (120 MiB input payload) and all terminal replays in 14.682 seconds total
wall; sampled node peak was 6,556 KiB and summed cat peak 1,360 KiB. These are
functional workload observations, not optimization gains or CPU microbenchmarks.
The injected monitor failure aborted and recorded failed status; all sampled
owned PIDs were absent after the runs. Four pure protocol/sampling fixtures pass.

## Correctness review follow-up

The historical runs above used weaker retention assertions. The corrected driver
requires no default-mode gap and exactly eight small-mode retained data frames;
its regressions reject empty history masquerading as a successful replay. Four
additional fixtures cover these assertions, byte-trickle deadlines, HTTP-body
trickle deadlines, and abort during a blocked receive plus late registration.
Eight fixtures passed locally. The corrected driver was run once per mode on
the same verified Linux archive: both verified 256 echoes; default replay
retained 128 data frames per cat without gaps, small replay retained 8 with
gap 1–120, both ending at sequence 129. Injected monitor failure exited nonzero
and recorded `failed`, not `completed`. Only the sampled owned PIDs were checked
after cleanup; none remained. These are correctness rechecks, not replacement
performance data, a long-run revalidation, or a macOS execution claim.

The control HTTP opener rejects redirects and disables proxies. The owned-node
monitor starts immediately after Popen, before the startup announcement or any
spawn/control HTTP request, so the whole-load guard also covers those stages.
HTTP uses a three-second socket timeout and body elapsed-time checks; unlike
WebSocket exact reads, this is not a strict three-second header deadline. The
180-second watchdog terminates the owned node to interrupt control HTTP, and
cleanup joins the monitor. CI additionally caps each driver process at 210
seconds as a last resort; forced runner/process death is not a cleanup guarantee.

## Final integrated driver recheck

Driver SHA-256 `af8648f4a344fb81c253120a1eb695d29fc75d0ec16c74373f0a24535b0e66bf`:
nine isolated fixtures and independent review passed. Against the historical
verified Linux release archive identified above, the exact workflow Python block
passed all three alternating default/small pairs and its 60-round small case
using one archive. A separate final 60-round run verified 7,680 echoes and
retained exactly eight frames per child in 16.86 seconds wall time. This is
multi-round acceptance, not a long-duration leak test or speed comparison.
Injected monitor failure returned nonzero, recorded `failed`, and left no
observed owned PIDs alive. Helper hash matched the published packaging helper.
The new macOS run must build and identify its own executable/archive; it is not
assumed bit-identical to an earlier macOS artifact. Existing historical numbers
are not attributed to the newly built macOS executable.
