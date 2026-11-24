#!/usr/bin/env python3
"""Hours-long headless soak for Atrium (and the pinned T3 binary).

The harness starts a throwaway project under the autopilot with mock agents,
injects a steady stream of notices that make the agents use tools, and drops
and restores the device link (mock-node) on a timer. Every minute it records
RSS, open files, thread count and CPU for the app and for the node process,
writes a CSV and a short Markdown report under docs/evidence/, and fails when
growth after the warm-up window passes a threshold.

On Linux the app runs under Xvfb (xvfb-run is used automatically when DISPLAY
is unset). On macOS the app needs the window server; there is no Xvfb there,
so the run is limited to whatever the machine can actually launch. The tool
never invents a result: a run that cannot start is recorded as blocked with
the exact command and error.

T3 mode uses the pinned binary from docs/t3-spike.md with the same mocks. It
drives turns through `t3 thread start/send --wait`; a small wrapper closes
the mock-codex protocol gaps listed in the spike and auto-approves commands so
the unattended run exercises tool calls. T3 itself is not downloaded here:
pass --t3-bin.

Examples:
  python3 tools/soak/soak.py --hours 2
  python3 tools/soak/soak.py --hours 24
  python3 tools/soak/soak.py --target t3 --t3-bin /opt/t3/t3 --hours 2
  python3 tools/soak/soak.py --smoke            # 90 s, 5 s samples
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BYTES_PER_MIB = 1024 * 1024
DEFAULT_HOURS = 2.0


# ---------------------------------------------------------------- utilities


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git_commit() -> str:
    try:
        return subprocess.check_output(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True, stderr=subprocess.DEVNULL
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def git_dirty() -> bool:
    try:
        return bool(
            subprocess.check_output(
                ["git", "-C", str(ROOT), "status", "--porcelain"],
                text=True,
                stderr=subprocess.DEVNULL,
            ).strip()
        )
    except (OSError, subprocess.CalledProcessError):
        return False


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def is_linux() -> bool:
    return platform.system() == "Linux"


def is_macos() -> bool:
    return platform.system() == "Darwin"


def kill_tree(process: subprocess.Popen, timeout: float = 10.0) -> None:
    """SIGTERM the process group, then SIGKILL what is left."""
    if process.poll() is not None:
        return
    try:
        if os.name == "posix":
            os.killpg(os.getpgid(process.pid), signal.SIGTERM)
        else:  # pragma: no cover - Windows is not a soak target
            process.terminate()
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            if os.name == "posix":
                os.killpg(os.getpgid(process.pid), signal.SIGKILL)
            else:  # pragma: no cover
                process.kill()
        except ProcessLookupError:
            pass
        process.wait(timeout=5)


# ---------------------------------------------------------------- sampling


@dataclass
class ProcSample:
    rss_kib: int
    fds: int
    threads: int
    cpu_seconds: float


@dataclass
class RoleSample:
    role: str
    rss_kib: int = 0
    fds: int = 0
    threads: int = 0
    cpu_seconds: float = 0.0
    pids: list[int] = field(default_factory=list)
    alive: bool = False


def linux_children(pid: int) -> list[int]:
    try:
        text = Path(f"/proc/{pid}/task/{pid}/children").read_text().strip()
        return [int(child) for child in text.split()] if text else []
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return []


def linux_tree(pid: int) -> list[int]:
    out, stack = [], [pid]
    while stack:
        current = stack.pop()
        out.append(current)
        stack.extend(linux_children(current))
    return out


def mac_tree(pid: int) -> list[int]:
    try:
        output = subprocess.check_output(
            ["ps", "-axo", "pid=,ppid="], text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError):
        return [pid]
    children: dict[int, list[int]] = {}
    for line in output.splitlines():
        fields = line.split()
        if len(fields) != 2:
            continue
        child, parent = int(fields[0]), int(fields[1])
        children.setdefault(parent, []).append(child)
    out, stack = [], [pid]
    while stack:
        current = stack.pop()
        out.append(current)
        stack.extend(children.get(current, []))
    return out


def sample_linux(pid: int) -> ProcSample | None:
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        status = Path(f"/proc/{pid}/status").read_text()
        fields = stat.rsplit(") ", 1)[1].split()
        cpu = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
        threads = int(fields[17])
        rss = 0
        for line in status.splitlines():
            if line.startswith("VmRSS:"):
                rss = int(line.split()[1])
                break
        fds = len(list(Path(f"/proc/{pid}/fd").iterdir()))
        return ProcSample(rss, fds, threads, cpu)
    except (FileNotFoundError, ProcessLookupError, PermissionError, IndexError, ValueError):
        return None


_TIME_RE = re.compile(r"(?:(\d+)-)?(?:(\d+):)?(\d+):(\d+)(?:\.(\d+))?")


def parse_ps_time(text: str) -> float:
    text = text.strip()
    match = _TIME_RE.search(text)
    if not match:
        return 0.0
    days, hours, minutes, seconds, frac = match.groups()
    total = (
        int(days or 0) * 86400
        + int(hours or 0) * 3600
        + int(minutes) * 60
        + int(seconds)
    )
    return total + (float("0." + frac) if frac else 0.0)


def sample_macos(pid: int) -> ProcSample | None:
    try:
        output = subprocess.check_output(
            ["ps", "-o", "rss=,time=", "-p", str(pid)], text=True, timeout=10
        ).strip()
        if not output:
            return None
        fields = output.split()
        rss = int(fields[0])
        cpu = parse_ps_time(fields[1])
        try:
            thread_output = subprocess.check_output(
                ["ps", "-M", "-p", str(pid)], text=True, timeout=10
            )
            threads = max(0, len(thread_output.splitlines()) - 1)
        except subprocess.SubprocessError:
            threads = 0
        try:
            fd_output = subprocess.check_output(
                ["lsof", "-nP", "-p", str(pid)], text=True, stderr=subprocess.DEVNULL, timeout=15
            )
            fds = max(0, len(fd_output.splitlines()) - 1)
        except (OSError, subprocess.SubprocessError):
            fds = 0
        return ProcSample(rss, fds, threads, cpu)
    except (OSError, subprocess.SubprocessError, ValueError, IndexError):
        return None


def sample_process(pid: int) -> ProcSample | None:
    return sample_linux(pid) if is_linux() else sample_macos(pid)


def tree_pids(root_pid: int) -> list[int]:
    return linux_tree(root_pid) if is_linux() else mac_tree(root_pid)


@dataclass
class Measured:
    role: str
    process: subprocess.Popen | None = None

    def root_pid(self) -> int | None:
        return self.process.pid if self.process and self.process.poll() is None else None


def sample_roles(targets: list[Measured]) -> list[RoleSample]:
    rows: list[RoleSample] = []
    for target in targets:
        row = RoleSample(target.role)
        root = target.root_pid()
        if root is None:
            rows.append(row)
            continue
        for pid in tree_pids(root):
            sample = sample_process(pid)
            if sample is None:
                continue
            row.alive = True
            row.pids.append(pid)
            row.rss_kib += sample.rss_kib
            row.fds += sample.fds
            row.threads += sample.threads
            row.cpu_seconds += sample.cpu_seconds
        rows.append(row)
    return rows


# ---------------------------------------------------------------- fixture


def write_file(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def task_file(title: str, agent: str, scope: str, body: str) -> str:
    return (
        f"# {title}\n"
        "status: open\n"
        f"agent: {agent}\n"
        f"scope: {scope}\n\n"
        f"{body}\n\n"
        "## Roadmap\n"
        "### Soak phase\n"
        "- [ ] exercise a turn\n"
        "- [ ] exercise a tool call\n"
    )


def make_fixture(root: Path, node_name: str, device_lane: bool) -> None:
    """A project whose controller has open lead tasks.

    The scheduler starts one lead per task; the autopilot keeps waking them.
    Notices appended by the harness contain the mock trigger words, so their
    turns include command approvals, file changes and plans.
    """
    now = int(time.time())
    atrium = root / ".atrium"
    project = {
        "name": "Soak fixture",
        "streams": [
            {
                "id": "controller",
                "name": "Controller",
                "backend": "codex",
                "thread_id": None,
                "goal": "Keep the soak fixture moving; do not implement.",
                "scope": [],
                "notices_seen": 0,
                "created_at": now,
                "role": "controller",
                "parent": None,
                "task": None,
                "reported": False,
                "retired": False,
                "pr": None,
            }
        ],
        "max_workers": 2,
        "approve_leads": False,
        "approved": [],
        "reported_tasks": [],
        "autopilot": {
            "on": True,
            "lead_heartbeat_min": 1,
            "controller_heartbeat_min": 2,
            "continue_after_secs": 5,
            "stall_min": 20,
            "tasks_per_session": 5,
            "refill_at": 0,
            "daily_turns": 0,
        },
        "worktrees": False,
    }
    write_file(atrium / "project.json", json.dumps(project, indent=2) + "\n")
    write_file(atrium / "brief.md", "# Brief\n\nSoak fixture. Nothing here is real.\n")
    write_file(atrium / "contracts" / "README.md", "# Contracts\n")
    write_file(atrium / "status" / "controller.md", "# Controller\n\n## Focus\n\nSoak.\n")
    write_file(
        atrium / "notices.md",
        "# Notices\n\n"
        f"- {dt.datetime.now().strftime('%Y-%m-%d %H:%M')} · controller → all: start the soak lanes\n",
    )
    write_file(
        atrium / "tasks" / "controller" / "codex-lane.md",
        task_file(
            "Codex lane",
            "codex",
            "codex/**",
            "Run the codex checks, edit the notes and plan the next step.",
        ),
    )
    write_file(
        atrium / "tasks" / "controller" / "claude-lane.md",
        task_file(
            "Claude lane",
            "claude",
            "claude/**",
            "Run the Claude checks, edit the notes and plan the next step.",
        ),
    )
    write_file(
        atrium / "tasks" / "controller" / "deepseek-lane.md",
        task_file(
            "DeepSeek lane",
            "deepseek",
            "deepseek/**",
            "Run the DeepSeek checks, edit the notes and plan the next step.",
        ),
    )
    if device_lane:
        write_file(
            atrium / "tasks" / "controller" / "node-lane.md",
            task_file(
                "Node lane",
                f"codex@{node_name}",
                "node/**",
                "Watch the node link and report progress in the status file.",
            ),
        )


def settings_json(
    project: Path,
    binary_dir: Path,
    nodes: list[str],
    workspace: Path,
    install_id: str,
) -> dict:
    mock_acp = binary_dir / "mock-acp"
    return {
        "projects": [str(project)],
        "active_project": str(project),
        "cwd": str(project),
        "codex_binary": str(binary_dir / "mock-codex"),
        "agent_commands": {
            "claude": f"{mock_acp} --name claude",
            "deepseek": f"{mock_acp} --name deepseek",
            "antigravity": f"{mock_acp} --name antigravity",
        },
        "sync_enabled": False,
        "remote_view": False,
        "no_update_check": True,
        "quiet_notifications": True,
        "no_predictions": True,
        "mode": "auto",
        "nodes": nodes,
        "install_id": install_id,
        "device_name": "soak-app",
        "workspace_dir": str(workspace),
        "language": "en",
    }


def write_settings(home: Path, data: dict) -> Path:
    # dirs::config_dir() is ~/.config on Linux and $HOME/Library/Application
    # Support on macOS.
    if is_macos():
        path = home / "Library" / "Application Support" / "Atrium" / "settings.json"
    else:
        path = home / ".config" / "Atrium" / "settings.json"
    write_file(path, json.dumps(data, indent=2) + "\n")
    return path


# ---------------------------------------------------------------- launchers


class NodeProcess:
    """A mock-node (or real atrium-node) in its own process group."""

    def __init__(self, role: str, command: list[str], env: dict, log: Path, ready: str):
        self.role = role
        self.command = command
        self.env = env
        self.log = log
        self.ready = ready
        self.process: subprocess.Popen | None = None
        self.port: int | None = None

    def start(self) -> None:
        self.log.parent.mkdir(parents=True, exist_ok=True)
        handle = self.log.open("a")
        handle.write(f"\n# {utc_now()} {' '.join(self.command)}\n")
        handle.flush()
        self.process = subprocess.Popen(
            self.command,
            env=self.env,
            stdout=handle,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self._handle = handle

    def measured(self) -> Measured:
        return Measured(self.role, self.process)

    def stop(self) -> None:
        if self.process:
            kill_tree(self.process, timeout=5)

    def restart(self) -> None:
        self.stop()
        self.start()


def start_mock_node(
    binary: Path, name: str, port: int, roots: Path, binaries: Path, env: dict, log: Path
) -> NodeProcess:
    command = [
        str(binary),
        "--port",
        str(port),
        "--name",
        name,
        "--root",
        str(roots),
        "--agent",
        f"codex=codex-app-server={binaries / 'mock-codex'},app-server",
        "--agent",
        f"claude=acp={binaries / 'mock-acp'},--name,claude",
    ]
    node = NodeProcess("node(mock)", command, env, log, "listening on")
    node.port = port
    node.start()
    return node


def start_real_node(binary: Path, config: Path, env: dict, log: Path) -> NodeProcess:
    node = NodeProcess("node(atrium-node)", [str(binary), "serve", "--config", str(config)], env, log, "listening on")
    node.start()
    return node


def wait_listening(node: NodeProcess, timeout: float = 20.0) -> None:
    """Wait for the listening line and read the bound port back."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if node.process is None:
            raise RuntimeError(f"{node.role} was not started")
        if node.process.poll() is not None:
            raise RuntimeError(f"{node.role} exited during startup")
        text = node.log.read_text(errors="replace") if node.log.exists() else ""
        for line in text.splitlines():
            if line.startswith(node.ready):
                if node.port is None:
                    match = re.search(r"127\.0\.0\.1:(\d+)", line)
                    if match:
                        node.port = int(match.group(1))
                return
        time.sleep(0.2)
    raise TimeoutError(f"{node.role} did not become ready: {node.log}")


def real_node_config(path: Path, port: int, roots: Path, binaries: Path, name: str) -> None:
    text = (
        f"port = {port}\n"
        "loopback_only = true\n"
        "discover_magpie = false\n"
        f"name = {json.dumps(name)}\n"
        f"roots = [{json.dumps(str(roots))}]\n"
        f"token = \"\"\n"
        "keep_output_lines = 2000\n"
        "keep_output_bytes = 134217728\n"
        "total_output_bytes = 268435456\n"
        "[agents.codex]\n"
        f"command = [{json.dumps(str(binaries / 'mock-codex'))}, \"app-server\"]\n"
        'protocol = "codex-app-server"\n'
        "[agents.claude]\n"
        f"command = [{json.dumps(str(binaries / 'mock-acp'))}, \"--name\", \"claude\"]\n"
        'protocol = "acp"\n'
    )
    write_file(path, text)


def app_command(app: Path) -> list[str]:
    if is_linux() and not os.environ.get("DISPLAY") and shutil.which("xvfb-run"):
        return ["xvfb-run", "-a", "-s", "-screen 0 1280x1024x24", str(app)]
    return [str(app)]


# ---------------------------------------------------------------- notices


def append_notice(project: Path, index: int, targets: list[str]) -> None:
    moment = dt.datetime.now().strftime("%Y-%m-%d %H:%M")
    for target in targets:
        text = f"soak heartbeat {index}: run the checks, edit notes and plan the next step"
        with (project / ".atrium" / "notices.md").open("a") as handle:
            handle.write(f"- {moment} · controller → {target}: {text}\n")


# ---------------------------------------------------------------- T3 wrapper


def write_t3_provider(home: Path, wrapper: Path, codex_home: Path) -> Path:
    path = home / "userdata" / "settings.json"
    data = {
        "providerInstances": {
            "mockcodex": {
                "driver": "codex",
                "displayName": "Mock Codex",
                "config": {
                    "binaryPath": str(wrapper),
                    "homePath": str(codex_home),
                    "launchArgs": "",
                },
            }
        }
    }
    write_file(path, json.dumps(data, indent=2) + "\n")
    return path


def t3_wrapper(env: dict, mock_codex: Path) -> Path:
    wrapper = Path(__file__).resolve().parent / "t3" / "mock_codex_t3.py"
    wrapper.chmod(0o755)
    env["SOAK_MOCK_CODEX"] = str(mock_codex)
    return wrapper


def t3_turn_loop(args, t3: Path, project: Path, env: dict, stop: threading.Event, state: dict) -> None:
    """Start one thread, then keep sending tool-triggering turns."""
    base = [str(t3), "thread"]
    common_env = dict(env)

    def run(subcommand: list[str], timeout: float = 180) -> tuple[int, str]:
        try:
            done = subprocess.run(
                subcommand,
                cwd=project,
                env=common_env,
                text=True,
                capture_output=True,
                timeout=timeout,
            )
            return done.returncode, done.stdout.strip() or done.stderr.strip()
        except subprocess.TimeoutExpired:
            return 124, "timeout"
        except OSError as error:
            return 127, str(error)

    code, output = run(
        base
        + [
            "start",
            "--provider",
            "mockcodex",
            "--model",
            "mock-1",
            "--wait",
            "--json",
            ".",
            "plan the soak heartbeat",
        ]
    )
    state["turn_errors"] = state.get("turn_errors", 0)
    try:
        thread_id = json.loads(output.splitlines()[-1])["threadId"]
    except (ValueError, KeyError, IndexError):
        state["thread_error"] = output
        return
    state["thread_id"] = thread_id
    count = 0
    while not stop.wait(args.turn_every):
        count += 1
        state["turns_sent"] = count
        code, output = run(
            base
            + ["send", "--wait", "--json", thread_id, f"run the soak turn {count}; edit notes; plan next"],
        )
        if code != 0:
            state["turn_errors"] += 1
            state.setdefault("last_turn_error", output[:500])
            if state["turn_errors"] >= 5:
                # Start a fresh thread; a wedged one should not stop the soak.
                code, output = run(
                    base
                    + [
                        "start",
                        "--provider",
                        "mockcodex",
                        "--model",
                        "mock-1",
                        "--wait",
                        "--json",
                        ".",
                        "plan the replacement soak heartbeat",
                    ]
                )
                try:
                    thread_id = json.loads(output.splitlines()[-1])["threadId"]
                    state["thread_id"] = thread_id
                    state["turn_errors"] = 0
                except (ValueError, KeyError, IndexError):
                    state["thread_error"] = output


# ---------------------------------------------------------------- statistics


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(round(fraction * (len(ordered) - 1))))
    return ordered[index]


def role_stats(rows: list[dict], role: str, warmup_end: float) -> dict:
    post = [row for row in rows if row["role"] == role and row["elapsed_s"] >= warmup_end]
    baseline = next((row for row in reversed(rows) if row["role"] == role and row["elapsed_s"] <= warmup_end), None)
    if baseline is None:
        baseline = next((row for row in rows if row["role"] == role), None)
    cpu_deltas = []
    previous = None
    for row in post:
        if previous is not None and row["cpu_seconds"] >= previous["cpu_seconds"]:
            elapsed = row["elapsed_s"] - previous["elapsed_s"]
            if elapsed > 0:
                cpu_deltas.append((row["cpu_seconds"] - previous["cpu_seconds"]) / elapsed * 100.0)
        previous = row
    return {
        "baseline": baseline,
        "end": post[-1] if post else (rows[-1] if rows else None),
        "peak_rss_kib": max((row["rss_kib"] for row in post), default=0),
        "peak_fds": max((row["fds"] for row in post), default=0),
        "peak_threads": max((row["threads"] for row in post), default=0),
        "cpu_percent_avg": sum(cpu_deltas) / len(cpu_deltas) if cpu_deltas else 0.0,
        "cpu_percent_p95": percentile(cpu_deltas, 0.95),
        "samples": len(post),
    }


def count_turns(codex_homes: list[Path]) -> tuple[int, int]:
    """Threads and turns persisted by mock-codex."""
    threads = turns = 0
    for home in codex_homes:
        sessions = home / "sessions"
        if not sessions.exists():
            continue
        for path in sessions.rglob("*.jsonl"):
            try:
                data = json.loads(path.read_text())
            except (OSError, ValueError):
                continue
            threads += 1
            turns += len(data.get("turns") or [])
    return threads, turns


# ---------------------------------------------------------------- run


def run(args) -> int:
    started = time.time()
    duration = args.seconds if args.seconds is not None else args.hours * 3600
    if args.smoke:
        duration = args.seconds if args.seconds is not None else 90
        args.sample_every = min(args.sample_every, 5)
        if args.warmup_seconds is not None:
            args.warmup_seconds = min(args.warmup_seconds, 20)
        args.notice_every = min(args.notice_every, 15)
        args.node_restart_every = min(args.node_restart_every, 45)
        args.startup_timeout = min(args.startup_timeout, 45)
    if args.warmup_seconds is None:
        args.warmup_seconds = min(max(duration * 0.1, 30), 1800)
    if args.warmup_seconds >= duration:
        args.warmup_seconds = max(0, duration * 0.5)

    binary_dir = args.binaries.expanduser().resolve()
    project = args.project.expanduser().resolve() if args.project else None
    workdir = (
        Path(args.workdir).expanduser().resolve()
        if args.workdir
        else Path(tempfile.mkdtemp(prefix="atrium-soak-"))
    )
    workdir.mkdir(parents=True, exist_ok=True)
    logs = workdir / "logs"
    logs.mkdir(exist_ok=True)
    if project is None:
        project = workdir / "project"
        project.mkdir(exist_ok=True)
    workspace = workdir / "workspace"
    workspace.mkdir(exist_ok=True)
    codex_home = workdir / "codex-home"
    mock_codex_home = workdir / "mock-node-codex-home"
    home = workdir / "home"
    home.mkdir(exist_ok=True)

    nodes: list[str] = []
    measured: list[Measured] = []
    nodes_started: list[NodeProcess] = []
    status = "passed"
    failure_reasons: list[str] = []
    notes: list[str] = []
    rows: list[dict] = []
    events = {"notices": 0, "node_restarts": 0}
    t3_state: dict = {}
    stop_driver = threading.Event()
    driver_thread = None

    app_log = logs / ("app.log" if args.target == "atrium" else "t3.log")
    platform_text = f"{platform.system()}-{platform.release()}-{platform.machine()}"
    command_text = " ".join(sys.argv)
    binary_info: dict[str, dict] = {}

    try:
        if args.dry_run:
            make_fixture(project, "soak-node", True)
            print(f"fixture: {project}")
            print(f"settings home: {home}")
            print(f"app command: {' '.join(app_command(binary_dir / 'atrium'))}")
            print(f"mock-node: {binary_dir / 'mock-node'}")
            return 0

        # --- the device link(s), started before the app so their ports are known
        launch_monotonic = time.monotonic()
        mock_port = free_port()
        make_fixture(project, "soak-node", not args.no_device_lane)
        node_env = dict(
            os.environ,
            HOME=str(workdir / "node-home"),
            CODEX_HOME=str(mock_codex_home),
        )
        (workdir / "node-home").mkdir(exist_ok=True)
        mock_node = start_mock_node(
            binary_dir / "mock-node",
            "soak-node",
            mock_port,
            workdir,
            binary_dir,
            node_env,
            logs / "mock-node.log",
        )
        nodes_started.append(mock_node)
        wait_listening(mock_node)
        nodes.append(f"http://127.0.0.1:{mock_node.port}")
        measured.append(mock_node.measured())

        if not args.no_real_node and (binary_dir / "atrium-node").exists():
            real_port = free_port()
            config = workdir / "atrium-node.toml"
            real_node_config(config, real_port, workdir, binary_dir, "soak-node-real")
            real_env = dict(node_env, ATRIUM_NODE_TOKEN="")
            real = start_real_node(binary_dir / "atrium-node", config, real_env, logs / "atrium-node.log")
            try:
                wait_listening(real)
                nodes.append(f"http://127.0.0.1:{real.port}")
                measured.append(real.measured())
                nodes_started.append(real)
            except (RuntimeError, TimeoutError) as error:
                notes.append(f"real atrium-node could not start: {error}")
                real.stop()

        if args.target == "atrium":
            app = binary_dir / "atrium"
            if not app.exists():
                raise FileNotFoundError(f"{app} is not built; run cargo build -p atrium-app")
            write_settings(
                home,
                settings_json(project, binary_dir, nodes, workspace, f"soak-{stamp()}"),
            )
            app_env = dict(
                os.environ,
                HOME=str(home),
                XDG_CONFIG_HOME=str(home / ".config"),
                CODEX_HOME=str(codex_home),
                ATRIUM_NODES=",".join(nodes),
                ATRIUM_NO_TAILSCALE="1",
                ATRIUM_NO_SYNC="1",
                ATRIUM_MEMORY_DIR=str(workdir / "memory"),
                RUST_LOG=os.environ.get("RUST_LOG", "info,codex_client=info"),
            )
            cmd = app_command(app)
            app_log.parent.mkdir(parents=True, exist_ok=True)
            handle = app_log.open("w")
            handle.write(f"# {utc_now()} {' '.join(cmd)}\n")
            handle.flush()
            process = subprocess.Popen(
                cmd,
                env=app_env,
                stdout=handle,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            measured.insert(0, Measured("app", process))
            app_handle = handle
            if not args.no_display_check and is_macos():
                notes.append(
                    "macOS has no Xvfb; the app runs against the current WindowServer, which a "
                    "headless agent session may not have."
                )
            if is_linux() and not os.environ.get("DISPLAY") and not shutil.which("xvfb-run"):
                raise RuntimeError("Linux has no DISPLAY and xvfb-run is not installed")
        else:
            if not args.t3_bin:
                raise RuntimeError("--target t3 needs --t3-bin (the pinned T3 binary)")
            t3 = Path(args.t3_bin).expanduser().resolve()
            if not t3.exists():
                raise FileNotFoundError(f"{t3} does not exist")
            t3_home = workdir / "t3-home"
            t3_codex_home = workdir / "t3-codex-home"
            t3_home.mkdir(exist_ok=True)
            t3_codex_home.mkdir(exist_ok=True)
            t3_env = dict(
                os.environ,
                HOME=str(home),
                T3CODE_HOME=str(t3_home),
                CODEX_HOME=str(t3_codex_home),
                ATRIUM_MEMORY_DIR=str(workdir / "memory"),
            )
            wrapper = t3_wrapper(t3_env, binary_dir / "mock-codex")
            write_t3_provider(t3_home, wrapper, t3_codex_home)
            port = free_port()
            cmd = [str(t3), "serve", "--port", str(port), "--host", "127.0.0.1", "--no-browser", str(project)]
            handle = app_log.open("w")
            handle.write(f"# {utc_now()} {' '.join(cmd)}\n")
            handle.flush()
            process = subprocess.Popen(
                cmd, env=t3_env, stdout=handle, stderr=subprocess.STDOUT, start_new_session=True
            )
            app_handle = handle
            measured.insert(0, Measured("t3", process))
            # T3 is ready when its HTTP endpoint answers.
            deadline = time.monotonic() + args.startup_timeout
            ready = False
            while time.monotonic() < deadline and process.poll() is None:
                try:
                    urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=1)
                    ready = True
                    break
                except urllib.error.HTTPError:
                    # Any HTTP response means the server is listening.
                    ready = True
                    break
                except OSError:
                    time.sleep(0.5)
            if not ready:
                raise RuntimeError("T3 server did not answer within the startup timeout")
            driver_thread = threading.Thread(
                target=t3_turn_loop,
                args=(args, t3, project, t3_env, stop_driver, t3_state),
                daemon=True,
            )
            driver_thread.start()

        # --- startup wait
        main = measured[0]
        deadline = time.monotonic() + args.startup_timeout
        while time.monotonic() < deadline:
            if main.process.poll() is not None:
                break
            if sample_roles([main])[0].alive:
                break
            time.sleep(0.5)
        if main.process.poll() is not None:
            tail = app_log.read_text(errors="replace")[-2000:] if app_log.exists() else ""
            raise RuntimeError(
                f"{main.role} exited during startup with code {main.process.returncode}:\n{tail}"
            )
        startup = time.monotonic()
        notes.append(f"main process came up after {startup - launch_monotonic:.1f}s")

        # --- sampling loop
        next_sample = time.monotonic()
        next_notice = time.monotonic() + args.notice_every
        next_restart = time.monotonic() + args.node_restart_every
        notice_index = 0
        start_monotonic = time.monotonic()
        while True:
            now = time.monotonic()
            elapsed = now - start_monotonic
            if elapsed >= duration:
                break
            if main.process.poll() is not None:
                status = "failed"
                failure_reasons.append(f"{main.role} exited early with code {main.process.returncode}")
                break

            if now >= next_sample:
                roots = {target.role: target.root_pid() or "" for target in measured}
                for row in sample_roles(measured):
                    rows.append(
                        {
                            "t_utc": utc_now(),
                            "elapsed_s": round(elapsed, 3),
                            "role": row.role,
                            "pid": ",".join(map(str, row.pids)),
                            "root_pid": roots.get(row.role, ""),
                            "rss_kib": row.rss_kib,
                            "fds": row.fds,
                            "threads": row.threads,
                            "cpu_seconds": round(row.cpu_seconds, 3),
                            "alive": int(row.alive),
                        }
                    )
                next_sample += args.sample_every

            if args.notice_every > 0 and now >= next_notice:
                notice_index += 1
                append_notice(project, notice_index, ["codex-lane", "claude-lane", "deepseek-lane"])
                events["notices"] = notice_index
                next_notice += args.notice_every

            if (
                args.node_restart_every > 0
                and mock_node.process is not None
                and now >= next_restart
                and not args.no_device_lane
            ):
                mock_node.restart()
                wait_listening(mock_node)
                for index, target in enumerate(measured):
                    if target.role == mock_node.role:
                        measured[index] = mock_node.measured()
                events["node_restarts"] += 1
                append_notice(project, notice_index + 1, ["node-lane"])
                next_restart += args.node_restart_every

            time.sleep(min(1.0, max(0.0, next_sample - time.monotonic())))

    except Exception as error:  # noqa: BLE001 - the report must record the blocker
        status = "blocked"
        failure_reasons.append(f"{type(error).__name__}: {error}")
    finally:
        stop_driver.set()
        if driver_thread is not None:
            driver_thread.join(timeout=5)
        for target in measured:
            if target.process is not None:
                kill_tree(target.process, timeout=10)
        for node in reversed(nodes_started):
            node.stop()

    ended = time.time()

    # --- verdict after the warm-up window
    warmup_end = min(args.warmup_seconds, max(0.0, ended - started))
    observations = []
    for role in [target.role for target in measured]:
        stats = role_stats(rows, role, warmup_end)
        if stats["baseline"] is None or stats["end"] is None:
            continue
        base, end = stats["baseline"], stats["end"]
        rss_growth = (end["rss_kib"] - base["rss_kib"]) * 1024
        fd_growth = end["fds"] - base["fds"]
        thread_growth = end["threads"] - base["threads"]
        observations.append(
            {
                "role": role,
                "rss_growth_bytes": rss_growth,
                "fd_growth": fd_growth,
                "thread_growth": thread_growth,
                **stats,
            }
        )
        if status == "passed":
            if rss_growth > args.rss_growth_mib * BYTES_PER_MIB:
                status = "failed"
                failure_reasons.append(
                    f"{role}: RSS grew {rss_growth / BYTES_PER_MIB:.1f} MiB "
                    f"(threshold {args.rss_growth_mib:.1f} MiB) after warm-up"
                )
            if fd_growth > args.fd_growth:
                status = "failed"
                failure_reasons.append(
                    f"{role}: open files grew by {fd_growth} (threshold {args.fd_growth})"
                )
            if thread_growth > args.thread_growth:
                status = "failed"
                failure_reasons.append(
                    f"{role}: threads grew by {thread_growth} (threshold {args.thread_growth})"
                )

    homes = [codex_home, mock_codex_home]
    if args.target == "t3":
        homes = [workdir / "t3-codex-home"]
    threads_seen, turns_seen = count_turns(homes)
    if status == "passed" and turns_seen == 0 and (ended - started) > max(30, args.warmup_seconds):
        status = "failed"
        failure_reasons.append("no turns were observed in the mock agents' session files")

    # --- evidence
    evidence_dir = args.evidence_dir.expanduser().resolve()
    evidence_dir.mkdir(parents=True, exist_ok=True)
    tag = f"soak-{stamp()}-{args.target}"
    csv_path = evidence_dir / f"{tag}.csv"
    report_path = evidence_dir / f"{tag}.md"
    with csv_path.open("w", newline="") as handle:
        writer = csv.DictWriter(
            handle,
            fieldnames=[
                "t_utc",
                "elapsed_s",
                "role",
                "pid",
                "root_pid",
                "rss_kib",
                "fds",
                "threads",
                "cpu_seconds",
                "alive",
            ],
        )
        writer.writeheader()
        writer.writerows(rows)

    binaries = {
        "atrium": binary_dir / "atrium",
        "mock-codex": binary_dir / "mock-codex",
        "mock-acp": binary_dir / "mock-acp",
        "mock-node": binary_dir / "mock-node",
        "atrium-node": binary_dir / "atrium-node",
    }
    for name, path in binaries.items():
        if path.exists():
            binary_info[name] = {
                "path": str(path),
                "sha256": sha256(path),
                "bytes": path.stat().st_size,
            }
    if args.target == "t3" and args.t3_bin:
        t3_path = Path(args.t3_bin).expanduser().resolve()
        if t3_path.exists():
            binary_info["t3"] = {
                "path": str(t3_path),
                "sha256": sha256(t3_path),
                "bytes": t3_path.stat().st_size,
            }

    lines = [
        f"# Soak {args.target} — {status}",
        "",
        f"- Started: {dt.datetime.fromtimestamp(started, dt.timezone.utc).isoformat()}",
        f"- Ended: {dt.datetime.fromtimestamp(ended, dt.timezone.utc).isoformat()}",
        f"- Duration: {ended - started:.1f} s (requested {duration:.0f} s)",
        f"- Warm-up: {warmup_end:.0f} s",
        f"- Platform: {platform_text}",
        f"- Commit: `{git_commit()}`"
        + (" (working tree has uncommitted changes)" if git_dirty() else ""),
        f"- Command: `{command_text}`",
        f"- Project: `{project}`",
        f"- CSV: `{csv_path.name}`",
        f"- Threads/turns observed in mock session files: {threads_seen}/{turns_seen}",
        f"- Notices injected: {events['notices']}; mock-node restarts: {events['node_restarts']}",
    ]
    if args.target == "t3":
        lines += [
            f"- T3 thread: `{t3_state.get('thread_id', 'none')}`; turns sent: "
            f"{t3_state.get('turns_sent', 0)}; turn errors: {t3_state.get('turn_errors', 0)}",
        ]
        if t3_state.get("thread_error"):
            lines.append(f"- T3 thread error: `{t3_state['thread_error'][:300]}`")
        if t3_state.get("last_turn_error"):
            lines.append(f"- Last T3 turn error: `{t3_state['last_turn_error']}`")
    lines += ["", "## Verdict", ""]
    if failure_reasons:
        lines += [f"- {reason}" for reason in failure_reasons]
    else:
        lines.append("- Growth stayed inside the configured thresholds.")
    lines += [
        "",
        f"Thresholds after warm-up: RSS +{args.rss_growth_mib:.1f} MiB, "
        f"open files +{args.fd_growth}, threads +{args.thread_growth}.",
        "",
        "## Footprint",
        "",
        "| process | samples | RSS start MiB | RSS end MiB | RSS peak MiB | growth MiB | fds start/end/peak | threads start/end/peak | CPU avg % | CPU p95 % |",
        "| --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |",
    ]
    for item in observations:
        base, end = item["baseline"], item["end"]
        lines.append(
            f"| {item['role']} | {item['samples']} | {base['rss_kib'] / 1024:.1f} | "
            f"{end['rss_kib'] / 1024:.1f} | {item['peak_rss_kib'] / 1024:.1f} | "
            f"{item['rss_growth_bytes'] / BYTES_PER_MIB:+.1f} | "
            f"{base['fds']}/{end['fds']}/{item['peak_fds']} | "
            f"{base['threads']}/{end['threads']}/{item['peak_threads']} | "
            f"{item['cpu_percent_avg']:.1f} | {item['cpu_percent_p95']:.1f} |"
        )
    if status == "blocked":
        notes.append(f"blocked runs keep their working directory: {workdir}")
        if app_log.exists():
            tail = app_log.read_text(errors="replace").strip().splitlines()[-12:]
            notes.append("last app log lines: " + " | ".join(tail) if tail else "app log is empty")
    lines += ["", "## Notes", ""]
    if notes:
        lines += [f"- {note}" for note in notes]
    else:
        lines.append("- None.")
    if args.target == "atrium":
        lines += [
            "",
            "Linux runs use `xvfb-run` when DISPLAY is unset. macOS needs the window server;",
            "a headless session without one records the launch error instead of a result.",
        ]
    else:
        lines += [
            "",
            "T3 runs point its `mockcodex` provider at `tools/soak/t3/mock_codex_t3.py`,",
            "which fills the protocol gaps named in `docs/t3-spike.md` and auto-approves",
            "commands so unattended turns exercise tool calls. T3 itself is not bundled.",
        ]
    lines += [
        "",
        "## Binaries",
        "",
        "| binary | bytes | sha256 |",
        "| --- | ---: | --- |",
    ]
    for name, info in binary_info.items():
        lines.append(f"| {name} | {info['bytes']} | `{info['sha256']}` |")
    lines += ["", f"Logs: `{logs}`", ""]
    report_path.write_text("\n".join(lines) + "\n")

    print(json.dumps({"status": status, "csv": str(csv_path), "report": str(report_path)}, indent=2))
    if failure_reasons:
        for reason in failure_reasons:
            print(f"soak: {reason}", file=sys.stderr)
    if not args.keep and status != "blocked":
        shutil.rmtree(workdir, ignore_errors=True)
    return 0 if status == "passed" else (1 if status == "failed" else 2)


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--target", choices=["atrium", "t3"], default="atrium")
    parser.add_argument("--hours", type=float, default=DEFAULT_HOURS, help="duration (default 2)")
    parser.add_argument("--seconds", type=float, help="explicit duration; overrides --hours")
    parser.add_argument("--smoke", action="store_true", help="90 s run with fast sampling")
    parser.add_argument("--sample-every", type=float, default=60.0, help="seconds between samples")
    parser.add_argument("--warmup-seconds", type=float, default=None)
    parser.add_argument("--notice-every", type=float, default=120.0, help="0 disables notice injection")
    parser.add_argument("--node-restart-every", type=float, default=600.0, help="0 disables the link drop")
    parser.add_argument("--startup-timeout", type=float, default=120.0)
    parser.add_argument("--turn-every", type=float, default=20.0, help="T3 turn interval")
    parser.add_argument("--rss-growth-mib", type=float, default=64.0)
    parser.add_argument("--fd-growth", type=int, default=32)
    parser.add_argument("--thread-growth", type=int, default=16)
    parser.add_argument("--binaries", type=Path, default=ROOT / "target" / "debug")
    parser.add_argument("--project", type=Path, help="use an existing project instead of a fixture")
    parser.add_argument("--workdir", type=Path, help="keep the fixture and logs here")
    parser.add_argument("--keep", action="store_true", help="keep the temporary work directory")
    parser.add_argument("--evidence-dir", type=Path, default=ROOT / "docs" / "evidence")
    parser.add_argument("--t3-bin", help="pinned T3 binary or npm launcher (--target t3)")
    parser.add_argument("--no-real-node", action="store_true", help="do not start atrium-node")
    parser.add_argument("--no-device-lane", action="store_true", help="do not start a task on the node")
    parser.add_argument("--no-display-check", action="store_true")
    parser.add_argument("--dry-run", action="store_true", help="create the fixture and print commands")
    return parser.parse_args(argv)


if __name__ == "__main__":
    try:
        sys.exit(run(parse_args(sys.argv[1:])))
    except KeyboardInterrupt:
        sys.exit(130)
