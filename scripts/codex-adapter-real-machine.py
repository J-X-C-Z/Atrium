#!/usr/bin/env python3
"""Item 22 real-machine check: the Codex adapter against a real Codex.

Runs the adapter kind of ``atrium-engine-driver`` with the user's own Codex
installation (its real ``CODEX_HOME`` and credentials), and checks the paths
the mock cannot prove:

* ``skills/list`` and ``config/read`` at session start,
* a real turn,
* ``thread/settings/update`` changing model, reasoning effort and approval
  policy on a loaded thread,
* a second turn, and
* ``thread/read`` returning the history without taking the writer lock.

Nothing secret is printed or stored: the script passes only the codex path,
prints the driver's normalized trace lines, and deletes its scratch folder.
Codex talks to its own home in place; the script never copies it.

Usage:

    python3 scripts/codex-adapter-real-machine.py [--codex-bin PATH]
        [--model ID] [--timeout-ms N]

Build first:

    cargo build -p atrium-engine --bin atrium-engine-driver
    cargo build -p codex-client --bin mock-codex   # not used here, but the
                                                  # driver's default lookup

Exit codes: 0 the checks passed, 1 a check failed, 2 setup/run error.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent


def target_dirs() -> list[Path]:
    dirs: list[Path] = []
    if configured := os.environ.get("CARGO_TARGET_DIR"):
        dirs.append(Path(configured))
    dirs.append(REPO / "target")
    return dirs


def find_driver(explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit).expanduser()
        if not path.is_file():
            raise SystemExit(f"--driver {path} is not a file")
        return path
    for directory in target_dirs():
        for profile in ("debug", "release"):
            candidate = directory / profile / (
                "atrium-engine-driver" + (".exe" if os.name == "nt" else "")
            )
            if candidate.is_file():
                return candidate
    raise SystemExit(
        "atrium-engine-driver not found; build it with "
        "`cargo build -p atrium-engine --bin atrium-engine-driver` "
        "or pass --driver"
    )


def find_codex(explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit).expanduser()
        if not path.is_file():
            raise SystemExit(f"--codex-bin {path} is not a file")
        return path
    if configured := os.environ.get("CODEX_BIN"):
        path = Path(configured).expanduser()
        if path.is_file():
            return path
    found = shutil.which("codex")
    if found:
        return Path(found)
    raise SystemExit(
        "Codex not found; install it or set CODEX_BIN / --codex-bin"
    )


def scenario(model: str | None) -> dict:
    return {
        "name": "codex-adapter-real-machine",
        "kind": "adapter",
        "provider": "codex",
        "model": model,
        "t3": "skip: real-machine check, no T3 comparison",
        "actions": [
            {"op": "skills"},
            {"op": "config"},
            {"op": "turn", "text": "Reply with exactly: pong"},
            {
                "op": "settings",
                "model": model,
                "effort": "high",
                "runtime_mode": "approval-required",
            },
            {"op": "turn", "text": "Reply with exactly: pong again"},
            {"op": "read_snapshot"},
        ],
        "expect": {"final_status": "completed"},
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex-bin", help="the real codex binary")
    parser.add_argument("--driver", help="atrium-engine-driver binary")
    parser.add_argument(
        "--model",
        help="model to switch to mid-thread (default: the catalog's default)",
    )
    parser.add_argument("--timeout-ms", type=int, default=180_000)
    args = parser.parse_args()

    driver = find_driver(args.driver)
    codex = find_codex(args.codex_bin)
    scratch = Path(tempfile.mkdtemp(prefix="atrium-codex-real-"))
    payload = json.dumps(scenario(args.model))
    try:
        completed = subprocess.run(
            [
                str(driver),
                "--real",
                "--codex-bin",
                str(codex),
                "--scratch",
                str(scratch),
                "--timeout-ms",
                str(max(args.timeout_ms, 1_000)),
            ],
            input=payload,
            text=True,
            capture_output=True,
            timeout=max(args.timeout_ms / 1000 + 60, 60),
            cwd=str(scratch),
        )
    except subprocess.TimeoutExpired:
        print("the driver timed out", file=sys.stderr)
        return 2
    finally:
        shutil.rmtree(scratch, ignore_errors=True)

    lines = [line for line in completed.stdout.splitlines() if line.strip()]
    traces = []
    for line in lines:
        print(line)
        try:
            traces.append(json.loads(line))
        except json.JSONDecodeError:
            pass

    if completed.returncode != 0:
        # The driver's stderr has our own messages only; Codex's child output
        # is not forwarded without a logger.
        print(completed.stderr, file=sys.stderr)
        print(f"driver exited {completed.returncode}", file=sys.stderr)
        return 2

    failures = []
    kinds = {trace.get("kind"): trace for trace in traces}
    if not kinds.get("adapter.skills"):
        failures.append("skills/list was not exercised")
    if not kinds.get("adapter.config"):
        failures.append("config/read was not exercised")
    settings = kinds.get("adapter.settings")
    if not settings or not settings.get("applied"):
        failures.append("thread/settings/update did not reach a loaded thread")
    snapshot = kinds.get("adapter.readSnapshot")
    if not snapshot or snapshot.get("read") != "ok":
        failures.append("thread/read did not return a snapshot")
    if not any(trace.get("kind") == "thread.finished" and trace.get("status") == "completed" for trace in traces):
        failures.append("the run did not finish completed")

    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print("OK: skills, config, a real turn, mid-thread settings and thread/read")
    return 0


if __name__ == "__main__":
    sys.exit(main())
