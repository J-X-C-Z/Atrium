#!/usr/bin/env python3
"""Run one scenario under T3 Code and under atrium-engine, then diff.

Python 3 standard library only. See README.md next to this file.

Exit codes: 0 the streams match, 1 they differ (or the expected final status
differs), 2 an environment/setup problem (missing binary, T3 did not start,
the mock does not satisfy T3's protocol gate, ...).
"""

from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
SCENARIOS_DIR = HERE / "scenarios"
OUT_DIR = HERE / "out"

# Must match crates/atrium-engine/src/diff.rs.
T3_SENDER_NOTE = (
    "[T3 Code: this message came through T3 Code's tools, "
    "not from the user typing in this chat.]"
)

ENVIRONMENT_MARKERS = (
    "does not work with this T3 Code release",
    "invalid payload for method",
    "probe failed",
    "is not installed",
)


class SetupError(Exception):
    """An environment problem: exit 2, not a stream mismatch."""


# --------------------------------------------------------------------------
# paths


def target_dirs() -> list[Path]:
    dirs: list[Path] = []
    configured = os.environ.get("CARGO_TARGET_DIR")
    if configured:
        dirs.append(Path(configured))
    dirs.append(REPO / "target")
    unique: list[Path] = []
    for directory in dirs:
        if directory not in unique:
            unique.append(directory)
    return unique


def find_tool(
    name: str,
    explicit: str | None,
    env_var: str,
    *,
    profiles: tuple[str, ...] = ("debug", "release"),
) -> Path:
    if explicit:
        path = Path(explicit).expanduser()
        if not path.is_file():
            raise SetupError(f"{name}: {path} is not a file")
        return path
    from_env = os.environ.get(env_var)
    if from_env:
        path = Path(from_env).expanduser()
        if not path.is_file():
            raise SetupError(f"{env_var}={path} is not a file")
        return path
    exe = name + (".exe" if os.name == "nt" else "")
    for directory in target_dirs():
        for profile in profiles:
            candidate = directory / profile / exe
            if candidate.is_file():
                return candidate
    on_path = shutil.which(name)
    if on_path:
        return Path(on_path)
    raise SetupError(
        f"{name} not found; build it or pass an explicit path "
        f"(env {env_var})"
    )


def find_t3(explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit).expanduser()
        if not path.is_file():
            raise SetupError(f"--t3-bin {path} is not a file")
        return path
    from_env = os.environ.get("T3_BIN")
    if from_env:
        path = Path(from_env).expanduser()
        if not path.is_file():
            raise SetupError(f"T3_BIN={path} is not a file")
        return path
    pinned = Path.home() / ".t3" / "runtime" / "versions" / "0.1.11" / "t3"
    if pinned.is_file():
        return pinned
    versions = Path.home() / ".t3" / "runtime" / "versions"
    if versions.is_dir():
        found = sorted(versions.glob("*/t3"))
        if found:
            return found[-1]
    on_path = shutil.which("t3")
    if on_path:
        return Path(on_path)
    raise SetupError(
        "T3 0.1.11 not found; install it, set T3_BIN, or use --no-t3"
    )


MOCK_WRAPPER = r'''#!/usr/bin/env python3
"""Forward to the real mock-codex, logging every request line.

T3 probes `<bin> --version` before it starts app-server; that path is passed
through untouched so the probe sees the real binary's output.
"""
import os
import subprocess
import sys
import threading

real = os.environ["MOCK_REAL"]
log = os.environ.get("MOCK_LOG")
args = sys.argv[1:]
if "--version" in args:
    os.execv(real, [real] + args)

child = subprocess.Popen(
    [real] + args,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    bufsize=0,
)
lock = threading.Lock()


def log_line(line: bytes) -> None:
    if not log:
        return
    with lock:
        with open(log, "ab") as sink:
            sink.write(line)


def pump_input() -> None:
    for line in sys.stdin.buffer:
        log_line(line)
        child.stdin.write(line)
        child.stdin.flush()
    try:
        child.stdin.close()
    except OSError:
        pass


def pump_output() -> None:
    for line in child.stdout:
        sys.stdout.buffer.write(line)
        sys.stdout.buffer.flush()


threading.Thread(target=pump_input, daemon=True).start()
threading.Thread(target=pump_output, daemon=True).start()
sys.exit(child.wait())
'''


def make_mock_wrapper(outdir: Path) -> Path:
    wrapper = outdir / "mock-wrap.py"
    wrapper.write_text(MOCK_WRAPPER, encoding="utf-8")
    wrapper.chmod(0o755)
    return wrapper


def provider_methods(log_path: Path) -> list[str]:
    """The JSON-RPC request method names the provider received, in order."""
    methods: list[str] = []
    try:
        lines = log_path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return methods
    for line in lines:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = message.get("method")
        if isinstance(method, str) and message.get("id") is not None:
            methods.append(method)
    return methods


def subset_match(expected: dict, actual: dict) -> bool:
    for key, value in expected.items():
        if key not in actual:
            return False
        candidate = actual[key]
        if isinstance(value, str) and isinstance(candidate, str):
            if value not in candidate:
                return False
        elif value != candidate:
            return False
    return True


def stream_contains(stream_text: str, wanted: list[dict]) -> list[str]:
    lines: list[dict] = []
    for line in stream_text.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            lines.append(value)
    problems: list[str] = []
    for expected in wanted:
        if not any(subset_match(expected, actual) for actual in lines):
            problems.append(f"stream lacks {json.dumps(expected, ensure_ascii=False)}")
    return problems


# --------------------------------------------------------------------------
# scenarios


def scenario_paths(names: list[str]) -> list[Path]:
    if not names:
        paths = sorted(SCENARIOS_DIR.glob("*.json"))
        if not paths:
            raise SetupError(f"no scenarios in {SCENARIOS_DIR}")
        return paths
    paths = []
    for raw in names:
        direct = Path(raw)
        if direct.is_file():
            paths.append(direct)
            continue
        for candidate in (SCENARIOS_DIR / raw, SCENARIOS_DIR / f"{raw}.json"):
            if candidate.is_file():
                paths.append(candidate)
                break
        else:
            raise SetupError(f"scenario not found: {raw}")
    return paths


def turn_texts(scenario: dict) -> list[str]:
    turns = scenario.get("turns") or []
    if not isinstance(turns, list) or not turns:
        raise SetupError("scenario has no turns")
    texts = []
    for turn in turns:
        if isinstance(turn, str):
            texts.append(turn)
        elif isinstance(turn, dict) and isinstance(turn.get("text"), str):
            texts.append(turn["text"])
        else:
            raise SetupError(f"bad turn in scenario: {turn!r}")
    return texts


# --------------------------------------------------------------------------
# normalization (mirrors crates/atrium-engine/src/diff.rs)


def normalize_text(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    if text.startswith(T3_SENDER_NOTE):
        text = text[len(T3_SENDER_NOTE) :]
    return text.strip()


def normalize_status(status: str) -> str:
    return status.strip().lower()


def item(ordinal: int, role: str, text: str) -> dict:
    return {
        "kind": "item",
        "ordinal": ordinal,
        "role": role,
        "text": normalize_text(text),
    }


def run_ordinal(run_id: str | None, fallback: int) -> int:
    if run_id:
        match = re.search(r"ordinal:(\d+)$", run_id)
        if match:
            return int(match.group(1))
    return fallback


def t3_stream(results: list[dict], show: dict | None) -> list[dict]:
    """Fold the CLI results and `thread show` into normalized lines."""
    lines: list[dict] = [{"kind": "thread.created"}]
    thread = (show or {}).get("thread") or {}
    messages = (show or {}).get("messages") or []
    by_run: dict[str, list[dict]] = {}
    for message in messages:
        by_run.setdefault(message.get("runId") or "", []).append(message)

    last_status = "completed"
    for index, result in enumerate(results):
        ordinal = run_ordinal(result.get("runId"), index + 1)
        status = normalize_status(result.get("status") or "failed")
        last_status = status
        lines.append({"kind": "run.started", "ordinal": ordinal})
        for message in by_run.get(result.get("runId") or "", []):
            role = message.get("role")
            if role in ("user", "assistant"):
                lines.append(item(ordinal, role, message.get("text") or ""))
        if status == "waiting" or int(thread.get("pendingRequestCount") or 0) > 0:
            lines.append({"kind": "approval.requested", "ordinal": ordinal})
        lines.append({"kind": "run.finished", "ordinal": ordinal, "status": status})

    final_status = normalize_status(thread.get("status") or last_status)
    # `thread show` keeps status "running" while a person must answer; the run
    # itself already ended as "waiting" and the pending request says so.
    if int(thread.get("pendingRequestCount") or 0) > 0:
        final_status = "waiting"
    elif final_status == "running" and last_status != "running":
        final_status = last_status
    lines.append({"kind": "thread.finished", "status": final_status})
    return lines


def render(lines: list[dict]) -> str:
    return "".join(
        json.dumps(line, separators=(",", ":"), ensure_ascii=False) + "\n"
        for line in lines
    )


def final_status(stream_text: str) -> str | None:
    for line in reversed(stream_text.splitlines()):
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if value.get("kind") == "thread.finished":
            return value.get("status")
    return None


# --------------------------------------------------------------------------
# engine side


def run_engine(
    scenario: dict,
    scenario_path: Path,
    outdir: Path,
    args: argparse.Namespace,
    mock: Path,
    wrapper: Path,
    mock_log: Path,
) -> str:
    engine = find_tool("atrium-engine-driver", args.engine_bin, "ENGINE_BIN")
    scratch = outdir / "scratch-engine"
    env = os.environ.copy()
    env["MOCK_CODEX_BIN"] = str(wrapper)
    if str(scenario.get("kind", "")).startswith("acp"):
        env["MOCK_ACP_BIN"] = str(
            find_tool("mock-acp", None, "MOCK_ACP_BIN", profiles=("debug", "release"))
        )
    env["MOCK_REAL"] = str(mock)
    env["MOCK_LOG"] = str(mock_log)
    env.setdefault("NO_PROXY", "*")
    command = [
        str(engine),
        "--scratch",
        str(scratch),
        "--keep-scratch",
        "--timeout-ms",
        str(int(args.timeout * 1000)),
    ]
    try:
        completed = subprocess.run(
            command,
            input=json.dumps(scenario),
            text=True,
            capture_output=True,
            timeout=args.timeout,
            env=env,
        )
    except subprocess.TimeoutExpired as error:
        raise SetupError(f"engine driver timed out after {args.timeout:.0f}s") from error
    (outdir / "engine.stderr.log").write_text(completed.stderr or "", encoding="utf-8")
    (outdir / "engine.exit").write_text(str(completed.returncode), encoding="utf-8")
    if completed.returncode != 0:
        raise SetupError(
            f"engine driver exited {completed.returncode}: "
            f"{(completed.stderr or '').strip() or 'no stderr'}"
        )
    text = completed.stdout or ""
    (outdir / "engine.jsonl").write_text(text, encoding="utf-8")
    if not text.strip():
        raise SetupError("engine driver wrote no stream")
    for line in text.splitlines():
        try:
            json.loads(line)
        except json.JSONDecodeError as error:
            raise SetupError(f"engine wrote a non-JSON line: {line!r}") from error
    if not args.keep:
        shutil.rmtree(scratch, ignore_errors=True)
    return text


# --------------------------------------------------------------------------
# T3 side


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run_t3_command(
    command: list[str],
    env: dict[str, str],
    timeout: float,
    log: list[str],
) -> subprocess.CompletedProcess:
    log.append("$ " + " ".join(command) + "\n")
    try:
        completed = subprocess.run(
            command,
            text=True,
            capture_output=True,
            timeout=timeout,
            env=env,
        )
    except subprocess.TimeoutExpired as error:
        log.append(f"<timed out after {timeout:.0f}s>\n")
        raise SetupError(
            f"T3 command timed out: {' '.join(command[1:])}"
        ) from error
    log.append(f"<exit {completed.returncode}>\n")
    if completed.stdout:
        log.append(completed.stdout if completed.stdout.endswith("\n") else completed.stdout + "\n")
    if completed.stderr:
        log.append("[stderr]\n" + completed.stderr)
    return completed


def extract_json(text: str) -> dict | list | None:
    text = (text or "").strip()
    if not text:
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    start = text.find("{")
    if start >= 0:
        try:
            return json.loads(text[start:])
        except json.JSONDecodeError:
            pass
    for line in reversed(text.splitlines()):
        line = line.strip()
        if line.startswith(("{", "[")):
            try:
                return json.loads(line)
            except json.JSONDecodeError:
                continue
    return None


def redact_serve_log(path: Path) -> None:
    """The server prints a pairing token; never keep it in `out/`."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return
    for label in ("Connection string", "Token", "Pairing URL", "App link"):
        text = re.sub(rf"(?m)^{re.escape(label)}: .*$", f"{label}: [redacted]", text)
    path.write_text(text, encoding="utf-8")


def stop_server(server: subprocess.Popen | None) -> None:
    if server is None or server.poll() is not None:
        return
    try:
        os.killpg(os.getpgid(server.pid), signal.SIGTERM)
    except (ProcessLookupError, PermissionError, OSError):
        try:
            server.terminate()
        except OSError:
            return
    try:
        server.wait(timeout=10)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(server.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError, OSError):
            server.kill()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass


def wait_for_server(
    t3: Path,
    env: dict[str, str],
    server: subprocess.Popen,
    logs: list[str],
    timeout: float,
    project: Path,
) -> None:
    deadline = time.monotonic() + timeout
    last_error = "no attempt"
    while time.monotonic() < deadline:
        if server.poll() is not None:
            raise SetupError(
                f"t3 serve exited early ({server.returncode}); see t3-serve.log"
            )
        try:
            completed = subprocess.run(
                [str(t3), "thread", "list", "--json", "--project", str(project)],
                text=True,
                capture_output=True,
                timeout=10,
                env=env,
            )
        except subprocess.TimeoutExpired:
            last_error = "thread list timed out"
            time.sleep(0.3)
            continue
        if completed.returncode == 0:
            logs.append("$ t3 thread list --json --project <project>\n<server ready>\n")
            return
        last_error = (completed.stderr or completed.stdout or "").strip()
        time.sleep(0.3)
    raise SetupError(f"t3 serve did not become ready in {timeout:.0f}s: {last_error}")


def run_t3(
    scenario: dict,
    scenario_path: Path,
    outdir: Path,
    args: argparse.Namespace,
    mock: Path,
    wrapper: Path,
    mock_log: Path,
) -> tuple[list[dict], dict | None]:
    t3 = find_t3(args.t3_bin)
    texts = turn_texts(scenario)
    provider = scenario.get("provider") or "mockcodex"
    model = scenario.get("model") or "mock-1"

    t3_home = outdir / "t3-home"
    codex_home = outdir / "codex-home"
    (t3_home / "userdata").mkdir(parents=True, exist_ok=True)
    codex_home.mkdir(parents=True, exist_ok=True)

    project_name = scenario.get("project") or "scratch"
    if project_name == "scratch":
        project = outdir / "project"
        project.mkdir(parents=True, exist_ok=True)
    else:
        project = Path(project_name).expanduser()
        if not project.is_absolute():
            project = (scenario_path.parent / project).resolve()
        if not project.is_dir():
            raise SetupError(f"scenario project is not a directory: {project}")

    if str(scenario.get("kind", "")).startswith("acp"):
        mock_acp = find_tool("mock-acp", None, "MOCK_ACP_BIN", profiles=("debug", "release"))
        settings = {
            "providerInstances": {
                provider: {
                    "driver": "acpRegistry",
                    "displayName": "Mock ACP",
                    "config": {
                        "source": "local",
                        "commandPath": str(mock_acp),
                        "commandArgs": [
                            "--name",
                            scenario.get("mock_name") or "claude",
                        ],
                    },
                }
            }
        }
    else:
        settings = {
            "providerInstances": {
                provider: {
                    "driver": "codex",
                    "displayName": "Mock Codex",
                    "config": {
                        "binaryPath": str(wrapper),
                        "homePath": str(codex_home),
                    },
                }
            }
        }
    (t3_home / "userdata" / "settings.json").write_text(
        json.dumps(settings, indent=2) + "\n", encoding="utf-8"
    )

    env = os.environ.copy()
    env["T3CODE_HOME"] = str(t3_home)
    env["HOME"] = str(t3_home)
    env["MOCK_REAL"] = str(mock)
    env["MOCK_LOG"] = str(mock_log)
    env.setdefault("NO_PROXY", "*")

    logs: list[str] = []
    server: subprocess.Popen | None = None
    serve_log = open(outdir / "t3-serve.log", "w", encoding="utf-8")
    results: list[dict] = []
    show: dict | None = None
    try:
        added = run_t3_command(
            [str(t3), "project", "add", str(project), "--title", scenario.get("name") or scenario_path.stem],
            env,
            args.timeout,
            logs,
        )
        if added.returncode != 0:
            raise SetupError(
                "t3 project add failed: "
                + (added.stderr or added.stdout or "").strip()
            )

        port = free_port()
        server_command = [
            str(t3),
            "serve",
            "--port",
            str(port),
            "--host",
            "127.0.0.1",
            "--no-browser",
            "--base-dir",
            str(t3_home),
            str(project),
        ]
        logs.append("$ " + " ".join(server_command) + "\n")
        server = subprocess.Popen(
            server_command,
            env=env,
            stdout=serve_log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        wait_for_server(t3, env, server, logs, args.timeout, project)

        thread_id: str | None = None
        for index, text in enumerate(texts):
            if thread_id is None:
                command = [
                    str(t3),
                    "thread",
                    "start",
                    "--provider",
                    provider,
                    "--model",
                    model,
                    "--wait",
                    "--json",
                    str(project),
                    text,
                ]
            else:
                command = [
                    str(t3),
                    "thread",
                    "send",
                    "--wait",
                    "--json",
                    thread_id,
                    text,
                ]
            completed = run_t3_command(command, env, args.timeout, logs)
            payload = extract_json(completed.stdout)
            if not isinstance(payload, dict) or not payload.get("threadId"):
                raise SetupError(
                    "T3 returned no thread JSON for turn "
                    f"{index + 1}: {(completed.stdout or '').strip()!r} "
                    f"{(completed.stderr or '').strip()!r}"
                )
            payload["_turn"] = index + 1
            payload["_text"] = text
            payload["_exit"] = completed.returncode
            results.append(payload)
            thread_id = payload.get("threadId") or thread_id
            status = normalize_status(payload.get("status") or "")
            error_text = payload.get("error") or ""
            lowered = error_text.lower()
            if status == "failed" and any(marker in lowered for marker in ENVIRONMENT_MARKERS):
                raise SetupError(f"T3/provider environment problem: {error_text}")
            if status == "waiting" or completed.returncode == 2:
                break
            if status == "failed":
                break
        if thread_id is None:
            raise SetupError("T3 never created a thread")

        message_count = max(10, 2 * len(texts) + 4)
        shown = run_t3_command(
            [
                str(t3),
                "thread",
                "show",
                "--json",
                "--messages",
                str(message_count),
                thread_id,
            ],
            env,
            args.timeout,
            logs,
        )
        payload = extract_json(shown.stdout)
        if not isinstance(payload, dict):
            raise SetupError(
                "T3 returned no thread JSON for show: "
                + (shown.stderr or shown.stdout or "").strip()
            )
        show = payload
    finally:
        stop_server(server)
        serve_log.close()
        redact_serve_log(outdir / "t3-serve.log")
        (outdir / "t3.log").write_text("".join(logs), encoding="utf-8")
        (outdir / "t3-results.json").write_text(
            json.dumps(results, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        if not args.keep:
            shutil.rmtree(t3_home, ignore_errors=True)
            shutil.rmtree(codex_home, ignore_errors=True)
            if project_name == "scratch":
                shutil.rmtree(project, ignore_errors=True)

    return results, show


# --------------------------------------------------------------------------
# comparison


def write_engine_stream(outdir: Path, text: str) -> str:
    (outdir / "engine.jsonl").write_text(text, encoding="utf-8")
    return text


def compare_and_report(
    scenario: dict,
    outdir: Path,
    t3_text: str,
    engine_text: str,
) -> tuple[bool, list[str]]:
    t3_lines = t3_text.splitlines()
    engine_lines = engine_text.splitlines()
    diff = list(
        difflib.unified_diff(
            t3_lines,
            engine_lines,
            fromfile="t3.jsonl",
            tofile="engine.jsonl",
            lineterm="",
        )
    )
    problems: list[str] = []
    if diff:
        problems.append("streams differ")
    expected = ((scenario.get("expect") or {}).get("final_status") or "").strip()
    actual = final_status(t3_text) or final_status(engine_text)
    if expected and actual and normalize_status(expected) != actual:
        problems.append(f"expected final status {expected}, got {actual}")
    elif expected and actual is None:
        problems.append(f"expected final status {expected}, but the stream has none")

    report: list[str] = []
    if diff:
        report.extend(diff)
    report.extend(f"# {problem}" for problem in problems)
    (outdir / "diff.txt").write_text(
        ("\n".join(report) if report else "MATCH") + "\n", encoding="utf-8"
    )
    return not problems, problems


def check_expectations(
    scenario: dict,
    engine_text: str,
    *,
    require_final_status: bool,
) -> list[str]:
    problems: list[str] = []
    expect = scenario.get("expect") or {}
    expected = (expect.get("final_status") or "").strip()
    actual = final_status(engine_text)
    if require_final_status and expected:
        if actual and normalize_status(expected) != actual:
            problems.append(f"expected final status {expected}, got {actual}")
        elif actual is None:
            problems.append(f"expected final status {expected}, but the stream has none")
    wanted = expect.get("stream_contains") or []
    if wanted:
        problems.extend(stream_contains(engine_text, wanted))
    return problems


def run_scenario(path: Path, args: argparse.Namespace) -> bool:
    scenario = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(scenario, dict):
        raise SetupError(f"{path} is not a JSON object")
    outdir = OUT_DIR / path.stem
    if outdir.exists():
        shutil.rmtree(outdir)
    outdir.mkdir(parents=True)
    (outdir / "scenario.json").write_text(
        json.dumps(scenario, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    mock = find_tool("mock-codex", args.mock_codex_bin, "MOCK_CODEX_BIN")
    wrapper = make_mock_wrapper(outdir)
    engine_log = outdir / "engine-mock.log"
    t3_log = outdir / "t3-mock.log"
    engine_text = run_engine(scenario, path, outdir, args, mock, wrapper, engine_log)
    engine_methods = provider_methods(engine_log)
    (outdir / "engine-methods.json").write_text(
        json.dumps(engine_methods, indent=2) + "\n", encoding="utf-8"
    )

    skip_reason = scenario.get("t3")
    if args.no_t3 or (isinstance(skip_reason, str) and skip_reason.startswith("skip:")):
        reason = (
            "(--no-t3)"
            if args.no_t3
            else skip_reason.removeprefix("skip:").strip()
        )
        (outdir / "diff.txt").write_text(
            f"SKIPPED {reason}: no T3 side and no diff\n", encoding="utf-8"
        )
        problems = check_expectations(scenario, engine_text, require_final_status=True)
        wanted_methods = (scenario.get("expect") or {}).get("provider_methods") or []
        for method in wanted_methods:
            if method not in engine_methods:
                problems.append(f"engine never called provider method {method}")
        for problem in problems:
            print(f"  {problem}")
        print(
            f"  engine-only: {len(engine_text.splitlines())} lines, "
            f"final status {final_status(engine_text) or 'unknown'}"
        )
        return not problems

    results, show = run_t3(scenario, path, outdir, args, mock, wrapper, t3_log)
    t3_methods = provider_methods(t3_log)
    (outdir / "t3-methods.json").write_text(
        json.dumps(t3_methods, indent=2) + "\n", encoding="utf-8"
    )
    t3_lines = t3_stream(results, show)
    t3_text = render(t3_lines)
    (outdir / "t3.jsonl").write_text(t3_text, encoding="utf-8")
    write_engine_stream(outdir, engine_text)
    ok, problems = compare_and_report(scenario, outdir, t3_text, engine_text)
    wanted_methods = (scenario.get("expect") or {}).get("provider_methods") or []
    for method in wanted_methods:
        if method not in t3_methods:
            problems.append(f"T3 never called provider method {method}")
        if method not in engine_methods:
            problems.append(f"engine never called provider method {method}")
    if problems:
        (outdir / "diff.txt").write_text(
            "\n".join(problems) + "\n", encoding="utf-8"
        )
    if ok and not problems:
        print(
            f"  MATCH ({len(t3_text.splitlines())} lines, "
            f"{len(engine_methods)} provider methods)"
        )
    else:
        print("  MISMATCH")
        for problem in problems:
            print(f"  {problem}")
        print(f"  see {outdir / 'diff.txt'}")
    return ok and not problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenarios", nargs="*", help="scenario names or paths (default: all)")
    parser.add_argument("--no-t3", action="store_true", help="run the engine side only")
    parser.add_argument("--t3-bin", help="T3 0.1.11 binary (env T3_BIN)")
    parser.add_argument("--mock-codex-bin", help="mock-codex binary (env MOCK_CODEX_BIN)")
    parser.add_argument("--engine-bin", help="atrium-engine-driver (env ENGINE_BIN)")
    parser.add_argument("--keep", action="store_true", help="keep T3/engine scratch dirs")
    parser.add_argument(
        "--timeout",
        type=float,
        default=180.0,
        help="per-command timeout in seconds (default 180)",
    )
    args = parser.parse_args()

    try:
        paths = scenario_paths(args.scenarios)
    except SetupError as error:
        print(f"setup error: {error}", file=sys.stderr)
        return 2

    failures = 0
    setup_failures = 0
    for path in paths:
        print(f"[{path.stem}]")
        try:
            if not run_scenario(path, args):
                failures += 1
        except SetupError as error:
            setup_failures += 1
            print(f"  setup error: {error}")
        except Exception as error:  # noqa: BLE001 - report, do not traceback
            setup_failures += 1
            print(f"  setup error: {type(error).__name__}: {error}")
    print(
        f"\n{len(paths)} scenario(s): {len(paths) - failures - setup_failures} ok, "
        f"{failures} mismatched, {setup_failures} setup error(s)"
    )
    if setup_failures:
        return 2
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
