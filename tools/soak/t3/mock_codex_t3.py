#!/usr/bin/env python3
"""Run Atrium's mock-codex under T3 Code.

T3 Code 0.1.11 (docs/t3-spike.md) needs a few things the mock does not send
yet. This wrapper sits between T3 and `target/debug/mock-codex`:

* `--version` answers with a Codex version T3 accepts;
* `initialize` reports `codex_cli_rs/0.160.0`;
* `thread/start` and `thread/resume` results carry approvalPolicy,
  approvalsReviewer, sandbox and a thread with cliVersion/projectId/source;
* `skills/list` and `config/read` get the stubs T3's provider probe wants;
* command and file-change approval requests are accepted here, because a
  headless T3 run has nobody to click them (this is a soak, not a test of
  T3's approval UI).

Set SOAK_MOCK_CODEX to the mock-codex binary. Everything else is passed
through unchanged.
"""

import json
import os
import subprocess
import sys
import threading

VERSION = "0.160.0"
USER_AGENT = f"codex_cli_rs/{VERSION} (Atrium soak)"


def log(message: str) -> None:
    print(f"mock-codex-t3: {message}", file=sys.stderr, flush=True)


def fix_thread(thread: dict) -> None:
    thread.setdefault("cliVersion", VERSION)
    thread.setdefault("projectId", None)
    thread.setdefault("source", "appServer")


def fix_result(method: str, result: dict) -> dict:
    if method == "initialize":
        result["userAgent"] = USER_AGENT
        return result
    if method in ("thread/start", "thread/resume"):
        result.setdefault("approvalPolicy", "never")
        result.setdefault("approvalsReviewer", "user")
        result.setdefault("sandbox", {"type": "dangerFullAccess"})
        if isinstance(result.get("thread"), dict):
            fix_thread(result["thread"])
    return result


def stub_for(method: str):
    if method == "skills/list":
        cwd = os.getcwd()
        return {"data": [{"cwd": cwd, "skills": [], "errors": []}]}
    if method == "config/read":
        return {"config": {}}
    return None


def main() -> int:
    mock = os.environ.get("SOAK_MOCK_CODEX")
    if not mock:
        log("SOAK_MOCK_CODEX is not set")
        return 2
    args = sys.argv[1:]
    if "--version" in args:
        print(f"codex-cli {VERSION}")
        return 0

    child = subprocess.Popen(
        [mock, *args],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=None,
        text=True,
        bufsize=1,
    )
    write_lock = threading.Lock()
    methods: dict[str, str] = {}

    def write_child(line: str) -> None:
        if child.stdin is None:
            return
        with write_lock:
            try:
                child.stdin.write(line)
                child.stdin.flush()
            except (BrokenPipeError, ValueError):
                pass

    def read_client() -> None:
        for line in sys.stdin:
            stripped = line.strip()
            if stripped:
                try:
                    message = json.loads(stripped)
                except ValueError:
                    message = None
                if isinstance(message, dict) and "method" in message and "id" in message:
                    methods[str(message["id"])] = str(message["method"])
            write_child(line)

    threading.Thread(target=read_client, daemon=True).start()

    assert child.stdout is not None
    for line in child.stdout:
        stripped = line.strip()
        if not stripped:
            continue
        try:
            message = json.loads(stripped)
        except ValueError:
            sys.stdout.write(line)
            sys.stdout.flush()
            continue

        # Answers the mock asks T3 for, which nobody can click headless.
        method = message.get("method")
        if method in (
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
        ):
            write_child(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": message.get("id"),
                        "result": {"decision": "accept"},
                    }
                )
                + "\n"
            )
            continue
        if method == "item/tool/requestUserInput":
            # Empty answers keep the scripted turn moving; the soak only needs
            # the request/response path to run, not meaningful values.
            write_child(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": message.get("id"),
                        "result": {"answers": {}},
                    }
                )
                + "\n"
            )
            continue

        request_id = message.get("id")
        if request_id is not None and ("result" in message or "error" in message):
            pending = methods.pop(str(request_id), "")
            if "error" in message:
                stub = stub_for(pending)
                if stub is not None:
                    message = {"jsonrpc": "2.0", "id": request_id, "result": stub}
            elif isinstance(message.get("result"), dict):
                message["result"] = fix_result(pending, message["result"])
        elif method == "thread/started":
            thread = message.get("params", {}).get("thread")
            if isinstance(thread, dict):
                fix_thread(thread)

        sys.stdout.write(json.dumps(message) + "\n")
        sys.stdout.flush()

    return child.wait()


if __name__ == "__main__":
    sys.exit(main())
