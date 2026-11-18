# Backend change inventory

Baseline: `c7e121ff67c4f4d690af4d42873b5631cf77f642`.

This is an independent checkout; existing UI files are unchanged. The changes are isolated
from main for draft review; consult the pull request for publication/check status. The native node is new:
the baseline has a test mock and client, not a production node implementation.
Therefore no comparable official source-size denominator exists, and a code
rewrite percentage cannot be calculated. Physical/nonblank lines include tests,
comments and declarations, not only production statements.

| New Rust file | Physical lines | Nonblank lines |
|---|---:|---:|
| `crates/atrium-node/src/auth.rs` | 200 | 198 |
| `crates/atrium-node/src/config.rs` | 234 | 232 |
| `crates/atrium-node/src/fs.rs` | 472 | 468 |
| `crates/atrium-node/src/magpie.rs` | 101 | 101 |
| `crates/atrium-node/src/main.rs` | 108 | 108 |
| `crates/atrium-node/src/procs.rs` | 980 | 961 |
| `crates/atrium-node/src/server.rs` | 409 | 409 |
| `crates/atrium-node/src/service.rs` | 340 | 340 |
| `crates/atrium-node/src/windows_process.rs` | 170 | 164 |
| `crates/atrium-node/tests/agents.rs` | 213 | 209 |
| `crates/atrium-node/tests/protocol.rs` | 194 | 194 |
| **Total** | **3421** | **3384** |

Tracked-file delta at this snapshot (`git diff --numstat`; additions/removals,
not rewritten LOC):

```text
4	0	.gitignore
236	1	Cargo.lock
1	0	Cargo.toml
510	16	crates/atrium-app/src/project.rs
2	0	crates/atrium-app/src/workspace.rs
498	79	crates/atrium-app/src/workspace_project.rs
```

Also added: native node manifest, backend status/benchmark reports, isolated
benchmark script, and a local-only minimum-permission CI workflow. No real credentials, generated build directories, or user
project files are included. Review commands: `git status --short`, `git diff`,
and `git diff --no-index /dev/null <new-file>` for untracked files.
