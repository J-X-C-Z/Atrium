# macOS: first local node, without a service or real account

**A node started with no config has no roots.** Metadata may work, but file
operations and process working directories are refused. This is intentional,
not an indication that the agents are broken. The steps below create one
explicit root and matching client credentials, entirely under a temporary
folder. They do not register launchd, change network settings, use a real
provider account, or edit your normal Atrium settings.

Requirements: the repository's Rust toolchain (MSRV 1.95), Python 3, and a shell.
Run from the repository root. The examples use repository mocks, not real agents.

## 1. Terminal A: build and prepare the isolated configuration

```sh
cargo build --locked -p atrium-node --bin atrium-node
cargo build --locked -p codex-client --bin mock-codex -p acp-client --bin mock-acp
BIN="$(pwd -P)/target/debug"
PORT=7381 # Choose once here; use 7382 instead if 7381 is already occupied.
DEMO="$(mktemp -d /tmp/atrium-demo.XXXXXX)"
python3 scripts/prepare-node-demo.py --output "$DEMO" --bin-dir "$BIN" --port "$PORT"
```

If using `CARGO_TARGET_DIR`, set `BIN` to its absolute `debug` directory instead.
The helper refuses to overwrite a nonempty directory. It creates:

- `$DEMO/work`: the **only** allowed root; choose this as the remote working folder.
- `$DEMO/node.toml`: `loopback_only=true`, `discover_magpie=false`, a random bearer
  token and explicit mock commands for all built-in agents.
- `$DEMO/home`: isolated client settings with the same `nodes` and `node_token`.
  No existing user configuration is read or replaced by this helper.

The helper writes `port` from `$PORT` into both node and client settings.
Its other essential node settings are:

```toml
loopback_only = true
discover_magpie = false
roots = ["/absolute/path/to/an/existing/work-folder"]
token = "a-new-local-token-shared-with-the-client"
```

Do not paste those placeholders literally. The helper generates valid absolute
paths and a fresh token. Its full config also overrides all default agents with
mocks; removing those overrides could invoke installed real agents.

## 2. Terminal A: start the node in the foreground

Continue in the same terminal, preserving `BIN`, `DEMO` and `PORT`. Keep it open:

```sh
HOME="$DEMO/home" SHELL=/bin/sh PATH=/usr/bin:/bin \
  "$BIN/atrium-node" serve --config "$DEMO/node.toml"
```

The first line must report `listening on 127.0.0.1:` followed by your `$PORT`.
If that address is occupied, do not kill an unrelated process. Before restarting,
prepare a **new** folder in Terminal A with a different port:

```sh
PORT=7382
DEMO="$(mktemp -d /tmp/atrium-demo.XXXXXX)"
python3 scripts/prepare-node-demo.py --output "$DEMO" --bin-dir "$BIN" --port "$PORT"
```

Then rerun the foreground command above. In every other terminal, use this new
`DEMO` directory. Changing only a shell variable does not rewrite an existing
config; the helper must regenerate both node and client settings together.

`atrium-node config path` prints the normal config location, but this demo uses
an explicit `--config` and does not modify that normal file.

## 3. Terminal B: verify the existing client without opening GPUI

In a second terminal, set `DEMO` to the exact printed demo directory and run
from the same repository. Shell variables are not shared between terminals:
set `DEMO` here and read `PORT` from its generated settings, rather than typing
an independent fallback port. This exercises the repository's existing NodeClient
against the foreground server, including a file write/read, process spawn,
WebSocket attachment and clean stop:

```sh
export DEMO=/tmp/atrium-demo.REPLACE_WITH_YOUR_DIRECTORY
PORT="$(python3 -c 'import json,os,pathlib; print(json.loads((pathlib.Path(os.environ["DEMO"])/"home/Library/Application Support/Atrium/settings.json").read_text())["nodes"][0].rsplit(":",1)[1])')"
ATRIUM_TEST_NODE="http://127.0.0.1:$PORT" \
ATRIUM_TEST_TOKEN="$(python3 -c 'import json,os,pathlib; print(json.loads((pathlib.Path(os.environ["DEMO"])/"home/Library/Application Support/Atrium/settings.json").read_text())["node_token"])')" \
  cargo test --locked -p agent-rpc --test node -- --ignored
```

Expected: `a_real_node_answers ... ok`. The token is read from the generated
private settings file, not a real account. Do not post it in an issue or log.
For complete scripted Codex/ACP turns (including approval/permission and output):

```sh
cargo test --locked -p atrium-node --test agents -- --ignored
```

These two tests start and stop their **own** temporary nodes. They do not attach
to the foreground demo node or depend on your real provider accounts.

## 4. Terminal C (optional): connect the existing macOS Atrium app

This section is a source-derived manual handoff, not a claim that GUI acceptance
has been performed. In this terminal, set the same `DEMO` directory and derive
`PORT` again. Build the existing app before changing HOME:

```sh
export DEMO=/tmp/atrium-demo.REPLACE_WITH_YOUR_DIRECTORY
PORT="$(python3 -c 'import json,os,pathlib; print(json.loads((pathlib.Path(os.environ["DEMO"])/"home/Library/Application Support/Atrium/settings.json").read_text())["nodes"][0].rsplit(":",1)[1])')"
cargo build --locked -p atrium-app
BIN="$(pwd -P)/target/debug"
HOME="$DEMO/home" SHELL=/bin/sh \
ATRIUM_NO_TAILSCALE=1 ATRIUM_NODES="127.0.0.1:$PORT" \
CODEX_BIN="$BIN/mock-codex" "$BIN/atrium"
```

The existing app reads `nodes` and `node_token` from the isolated macOS settings
file. `ATRIUM_NODES` supplies addresses only; it is **not a token variable**.
In Settings → Devices, confirm the demo device, then select its remote Codex or
ACP agent and use the printed `$DEMO/work` path as its working folder. A prompt
containing `run` exercises the scripted approval flow. No GPUI code change is
needed for these connection settings. No real account is required.

## Stop, limits and validation record

Close the demo app and press Ctrl-C in the foreground node terminal. Node
shutdown closes owned child input and cleans up its owned processes. Keep the
temporary folder if you want to inspect it; do not remove it while the node/app
is running. None of this installs or validates a launchd service.

Client disconnection/reconnection can resume retained process output while the
same node is alive. **Restarting the node loses that in-memory process table and
buffer; these instructions do not promise daemon-restart resume.** Roots confine
file APIs and starting directories, not arbitrary agent execution. Project
updates are not a cross-process exactly-once transaction.

The helper, generated config, foreground node and existing-client acceptance
were exercised in a temporary Linux environment while the Mac was offline,
including port 7382. Each client address used the port read from the generated
settings; no client command assumes port 7381. The port-fallback recheck used
the existing compiled NodeClient acceptance binary because Cargo was unavailable
in that later cloud session; it was not a new compilation or a macOS GUI run.
Previous native macOS node/client tests are recorded in `backend-status.md`.
The optional GUI commands and launchd installation were **not** executed as part
of this quickstart verification.

A subsequent integrated Linux follow-up rebuilt the node and mock binaries with
`cargo build --locked --offline` on Rust 1.100.0, then reran the helper at port
7382 and `cargo test --locked --offline -p agent-rpc --test node -- --ignored`
against that foreground node: 1/1 passed. The node was shut down afterward.
`python3 scripts/test_prepare_node_demo.py` additionally passes three tests for
non-default port/settings consistency, private settings, refusal to overwrite
nonempty destinations, invalid ports and missing binaries. This later run still
does not constitute GUI or real launchd acceptance.
