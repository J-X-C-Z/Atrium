#!/usr/bin/env python3
"""Native node-only archive, integrity verification, and isolated foreground smoke.
Python 3.11+ standard library; Linux/macOS only. Never installs a service.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import secrets
import selectors
import stat
import struct
import subprocess
import tarfile
import tempfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BINARY = "bin/atrium-node"
MEMBERS = {BINARY, "manifest.json"}
MAX_BINARY = 256 * 1024 * 1024


def sha(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def run(args, **kwargs):
    return subprocess.check_output(args, timeout=60, **kwargs).decode().strip()


def binary_identity(data):
    if data[:4] == b"\x7fELF" and len(data) >= 20 and data[4] == 2:
        endian = {1: "<", 2: ">"}.get(data[5])
        if not endian:
            raise ValueError("invalid ELF byte order")
        machine = struct.unpack(endian + "H", data[18:20])[0]
        arch = {62: "x86_64", 183: "aarch64"}.get(machine)
        kind, system = "ELF64", "Linux"
    elif data[:4] == b"\xcf\xfa\xed\xfe" and len(data) >= 8:
        cpu = struct.unpack("<I", data[4:8])[0]
        arch = {0x01000007: "x86_64", 0x0100000C: "aarch64"}.get(cpu)
        kind, system = "Mach-O64", "Darwin"
    else:
        raise ValueError("only native 64-bit ELF/Mach-O node packages are supported")
    if not arch:
        raise ValueError("unsupported executable architecture")
    return {"format": kind, "os": system, "arch": arch}


def host_identity():
    arch = {"arm64": "aarch64", "AMD64": "x86_64"}.get(platform.machine(), platform.machine())
    return {"os": platform.system(), "arch": arch}


def target_identity(target):
    arch = target.split("-", 1)[0]
    system = "Darwin" if target.endswith("-apple-darwin") else "Linux" if "-linux-" in target else None
    if arch not in {"x86_64", "aarch64"} or system is None:
        raise ValueError("unsupported target triple")
    return {"os": system, "arch": arch}


def make_archive(binary, metadata, output):
    """Deterministic archive from fixed bytes/metadata; never overwrite paths."""
    identity = binary_identity(binary)
    if len(binary) > MAX_BINARY:
        raise ValueError("binary exceeds package limit")
    manifest = dict(metadata, schema=1, package="atrium-node", binary=dict(identity, path=BINARY, size=len(binary), sha256=sha(binary)))
    encoded = canonical(manifest)
    if len(encoded) > 65536:
        raise ValueError("manifest too large")
    output = Path(output)
    # Exclusive creation also refuses symlinks, and never removes collisions.
    with output.open("xb") as destination:
        try:
            with gzip.GzipFile(filename="", mode="wb", fileobj=destination, mtime=0, compresslevel=9) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                    for name, data, mode in [(BINARY, binary, 0o755), ("manifest.json", encoded, 0o644)]:
                        item = tarfile.TarInfo(name)
                        item.size, item.mode, item.mtime = len(data), mode, 0
                        item.uid = item.gid = 0
                        item.uname = item.gname = ""
                        archive.addfile(item, io.BytesIO(data))
            destination.flush()
            os.fsync(destination.fileno())
        except BaseException:
            # Keep a failed exclusive output for inspection. A stat/unlink
            # sequence cannot safely prove ownership against a same-UID swap.
            # Never risk deleting a replacement; callers choose a new path.
            raise
    return manifest


def read_regular_bounded(path, limit):
    """Open once without following the final symlink; bound even concurrent growth."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError as error:
        raise ValueError("cannot open regular non-symlink input") from error
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise ValueError("input must be a bounded regular file")
        data = source.read(limit + 1)
        if len(data) > limit:
            raise ValueError("input grew beyond limit")
        return data


def read_archive_members(packed):
    # Do not let TarFile consume extension headers before we see them. Its PAX
    # and GNU processors can allocate an attacker-selected body before yielding
    # a member. Only our exact two canonical USTAR headers are accepted here.
    files = {}
    consumed = 0
    with gzip.GzipFile(fileobj=io.BytesIO(packed), mode="rb") as source:
        for name, mode, limit in [(BINARY, 0o755, MAX_BINARY), ("manifest.json", 0o644, 65536)]:
            header = source.read(512)
            if len(header) != 512 or header[156:157] != b"0":
                raise ValueError("expected regular USTAR header; extensions forbidden")
            try:
                item = tarfile.TarInfo.frombuf(header, "utf-8", "strict")
            except (tarfile.HeaderError, UnicodeError, ValueError) as error:
                raise ValueError("invalid USTAR header") from error
            if item.size < 0 or item.size > limit:
                raise ValueError("archive member exceeds limit")
            expected = tarfile.TarInfo(name)
            expected.size, expected.mode, expected.mtime = item.size, mode, 0
            expected.uid = expected.gid = 0
            expected.uname = expected.gname = ""
            if header != expected.tobuf(format=tarfile.USTAR_FORMAT):
                raise ValueError("noncanonical or unexpected archive header")
            data = source.read(item.size)
            padding = (-item.size) % 512
            if len(data) != item.size or source.read(padding) != bytes(padding):
                raise ValueError("truncated member or invalid padding")
            files[name] = data
            consumed += 512 + item.size + padding
        # The writer adds two zero end blocks and pads to the tar record size.
        # Check only this bounded tail, then one byte: no trailing bombs/members.
        tail = 1024 + (-(consumed + 1024)) % tarfile.RECORDSIZE
        if source.read(tail) != bytes(tail) or source.read(1):
            raise ValueError("invalid archive end or trailing data")
    return files


def verify(path, expected_sha256=None):
    packed = read_regular_bounded(path, MAX_BINARY + 1024 * 1024)
    digest = sha(packed)
    if expected_sha256 and digest != expected_sha256:
        raise ValueError("archive SHA-256 mismatch")
    files = read_archive_members(packed)
    if files.keys() != MEMBERS:
        raise ValueError("missing archive member")
    manifest = json.loads(files["manifest.json"])
    if not isinstance(manifest, dict) or canonical(manifest) != files["manifest.json"] or manifest.get("schema") != 1 or manifest.get("package") != "atrium-node":
        raise ValueError("invalid manifest schema/encoding")
    binary = files[BINARY]
    expected = dict(binary_identity(binary), path=BINARY, size=len(binary), sha256=sha(binary))
    if manifest.get("binary") != expected:
        raise ValueError("binary hash/size/architecture mismatch")
    if not isinstance(manifest.get("version"), str) or not manifest["version"] or not isinstance(manifest.get("target"), str):
        raise ValueError("missing version/target")
    if any(expected[key] != value for key, value in target_identity(manifest["target"]).items()):
        raise ValueError("target triple disagrees with binary header")
    return manifest, binary, {"archive_sha256": digest, "manifest_sha256": sha(files["manifest.json"])}


def build(output, target_dir):
    if platform.system() not in {"Linux", "Darwin"}:
        raise ValueError("native Linux/macOS builds only")
    if Path(output).exists() or Path(output).is_symlink():
        raise FileExistsError(output)
    if not Path(output).parent.is_dir():
        raise FileNotFoundError("output parent directory must already exist")
    target_dir = Path(target_dir).resolve()
    if target_dir == ROOT / "target":
        raise ValueError("use an independent target directory")
    rustc = run(["rustc", "-vV"])
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    subprocess.run(["cargo", "build", "--offline", "--locked", "--release", "-p", "atrium-node", "--bin", "atrium-node", "--target", host, "--target-dir", str(target_dir)], cwd=ROOT, check=True, timeout=900)
    executable = target_dir / host / "release/atrium-node"
    binary = read_regular_bounded(executable, MAX_BINARY)
    identity = binary_identity(binary)
    if any(identity[key] != value for key, value in host_identity().items()):
        raise ValueError("binary is not native to this machine")
    if platform.system() == "Linux":
        dynamic = {"tool": "readelf -d / --version-info", "output": run(["readelf", "-d", str(executable)]), "symbol_versions": run(["readelf", "--version-info", str(executable)])}
    else:
        dynamic = {"tool": "otool -L", "output": "\n".join(run(["otool", "-L", str(executable)]).splitlines()[1:])}
    metadata = {
        "version": tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"],
        "target": host, "profile": "release", "rustc": rustc,
        "source_commit": run(["git", "rev-parse", "HEAD"], cwd=ROOT),
        "source_dirty": bool(run(["git", "status", "--porcelain"], cwd=ROOT)),
        "cargo_lock_sha256": sha((ROOT / "Cargo.lock").read_bytes()),
        "dynamic_dependencies": dynamic,
        "boundary": "Unsigned native node only; no GUI, config, credentials, mocks, installers, signing or notarization.",
    }
    make_archive(binary, metadata, output)
    return verify(output)[2]


def startup_line(pipe, timeout=15):
    deadline = time.monotonic() + timeout
    data = bytearray()
    with selectors.DefaultSelector() as selector:
        selector.register(pipe, selectors.EVENT_READ)
        while b"\n" not in data:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise TimeoutError("node startup deadline")
            chunk = os.read(pipe.fileno(), 1024 - len(data))
            if not chunk:
                raise ValueError("node exited before listener announcement")
            data.extend(chunk)
            if len(data) >= 1024:
                raise ValueError("oversized startup announcement")
    return bytes(data).split(b"\n", 1)[0].decode().strip()


def stop_owned(child):
    """Only act on our Popen handle; never enumerate or signal arbitrary PIDs."""
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=12)
        except subprocess.TimeoutExpired:
            child.kill(); child.wait(timeout=3)
            raise TimeoutError("node graceful cleanup failed; smoke is not accepted")


def smoke(path, expected_sha256=None):
    manifest, binary, hashes = verify(path, expected_sha256)
    if any(manifest["binary"][key] != value for key, value in host_identity().items()):
        raise ValueError("smoke requires native OS/architecture")
    if platform.system() not in {"Linux", "Darwin"} or not Path("/bin/cat").is_file():
        raise ValueError("smoke requires Linux/macOS and /bin/cat")
    checks = []
    with tempfile.TemporaryDirectory(prefix="atrium-node-smoke-") as temp:
        root = Path(temp)
        work, home = root / "work", root / "home"
        work.mkdir(); home.mkdir()
        executable = root / "atrium-node"
        executable.write_bytes(binary); executable.chmod(0o700)
        token = secrets.token_hex(32)
        config = root / "node.toml"
        body = f'port=0\nloopback_only=true\ndiscover_magpie=false\nroots=[{json.dumps(str(work))}]\ntoken={json.dumps(token)}\n'
        for agent in ["echo", "codex", "claude", "deepseek", "antigravity"]:
            body += f'[agents.{agent}]\ncommand=["/bin/cat"]\nprotocol="raw"\n'
        config.write_text(body); config.chmod(0o600)
        env = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"), "PATH": "/usr/bin:/bin", "SHELL": "/bin/false", "LANG": "C"}
        # Logs are kept only in this owned temp root; never include token in output.
        with (root / "stderr").open("wb") as errors:
            child = subprocess.Popen([str(executable), "serve", "--config", str(config)], env=env, stdout=subprocess.PIPE, stderr=errors)
            try:
                line = startup_line(child.stdout)
                prefix = "listening on 127.0.0.1:"
                if not line.startswith(prefix) or not line[len(prefix):].isdigit():
                    raise ValueError("unexpected listener")
                port = int(line[len(prefix):])
                if not 1 <= port <= 65535:
                    raise ValueError("invalid listener port")
                base = f"http://127.0.0.1:{port}"
                # Ignore ambient proxy variables, including localhost proxies.
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                def req(method, route, data=None, auth=True, status=200):
                    headers = {"Authorization": "Bearer " + token} if auth else {}
                    if isinstance(data, dict):
                        headers["Content-Type"] = "application/json"; data = canonical(data)
                    request = urllib.request.Request(base + route, data=data, headers=headers, method=method)
                    try:
                        response = opener.open(request, timeout=5)
                    except urllib.error.HTTPError as error:
                        response = error
                    with response:
                        if response.status != status:
                            raise ValueError(f"unexpected HTTP status {response.status}, wanted {status}")
                        return response.read(65537)
                req("GET", "/v1/node", auth=False, status=401); checks.append("unauthorized_rejected")
                node = json.loads(req("GET", "/v1/node"))
                if node["version"] != manifest["version"] or node["protocol"] != 1:
                    raise ValueError("node version/protocol differs from manifest")
                checks.append("authenticated_version")
                file = work / "smoke.txt"
                query = "?path=" + urllib.parse.quote(str(file), safe="")
                req("PUT", "/v1/fs/write" + query, b"node-smoke\n", status=204)
                if req("GET", "/v1/fs/read" + query) != b"node-smoke\n":
                    raise ValueError("file roundtrip mismatch")
                req("GET", "/v1/fs/read?path=" + urllib.parse.quote(str(root / "outside"), safe=""), status=403)
                checks.extend(["file_roundtrip", "outside_root_rejected"])
                proc = json.loads(req("POST", "/v1/procs", {"agent": "echo", "cwd": str(work), "owner": "package-smoke"}, status=201))
                if proc.get("agent") != "echo" or proc.get("owner") != "package-smoke" or not isinstance(proc.get("pid"), int) or proc["pid"] <= 0:
                    raise ValueError("unexpected spawned process identity")
                ident = proc["id"]
                req("DELETE", "/v1/procs/" + urllib.parse.quote(ident, safe=""), status=204)
                deadline = time.monotonic() + 10
                while True:
                    procs = json.loads(req("GET", "/v1/procs?owner=package-smoke"))
                    entry = next((p for p in procs if p["id"] == ident), None)
                    if entry and entry.get("exit") is not None:
                        break
                    if time.monotonic() > deadline:
                        raise TimeoutError("owned cat exit deadline")
                    time.sleep(0.05)
                checks.append("owned_cat_exit")
            finally:
                # Signal only this Popen child. Its graceful shutdown owns cleanup
                # of spawned cat; never kill processes found by names or PID scans.
                try:
                    stop_owned(child)
                finally:
                    child.stdout.close()
            if child.returncode != 0:
                raise ValueError("node did not shut down cleanly")
            checks.append("node_clean_shutdown")
    return dict(hashes, version=manifest["version"], target=manifest["target"], checks=checks, scope="native foreground loopback only; no service/GUI/tailnet acceptance")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build_parser = commands.add_parser("build")
    build_parser.add_argument("--output", type=Path, required=True)
    build_parser.add_argument("--target-dir", type=Path, required=True)
    for name in ["verify", "smoke"]:
        sub = commands.add_parser(name)
        sub.add_argument("archive", type=Path)
        sub.add_argument("--expected-sha256")
    args = parser.parse_args()
    if args.command == "build":
        result = build(args.output, args.target_dir)
    elif args.command == "verify":
        manifest, _, hashes = verify(args.archive, args.expected_sha256)
        result = dict(hashes, manifest=manifest)
    else:
        result = smoke(args.archive, args.expected_sha256)
    print(json.dumps(result, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
