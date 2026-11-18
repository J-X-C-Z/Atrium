#!/usr/bin/env python3
"""Isolated Unix native-node microbenchmark; Python stdlib, no real agents."""
import argparse
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import platform
import selectors
import subprocess
import tempfile
import time
import urllib.parse


def summary(samples, elapsed):
    ordered = sorted(samples)
    return {"samples": len(samples), "p50_ms": ordered[math.ceil(len(ordered)*.50)-1],
            "p95_ms": ordered[math.ceil(len(ordered)*.95)-1],
            "sequential_ops_s": len(samples)/elapsed}


def rss(pid):
    # Only the child PID owned by this benchmark, never a system process scan.
    try:
        return int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)], text=True).strip())
    except (OSError, ValueError, subprocess.CalledProcessError):
        return None


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--binary", type=Path, default=Path("target/release/atrium-node"))
    ap.add_argument("--samples", type=int, default=1000)
    ap.add_argument("--process-samples", type=int, default=100)
    ap.add_argument("--output", type=Path)
    args = ap.parse_args()
    if os.name != "posix" or not Path("/bin/cat").is_file():
        ap.error("This harness requires Unix and /bin/cat; Windows is not measured")
    if min(args.samples, args.process_samples) < 1:
        ap.error("sample counts must be positive")
    binary = args.binary.resolve(strict=True)
    result = {"measured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "platform": platform.platform(), "machine": platform.machine(),
              "logical_cpus": os.cpu_count(), "python": platform.python_version(),
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "binary_bytes": binary.stat().st_size, "payload_bytes": 1024,
              "transport": "HTTP/1.1 persistent loopback, bearer token, concurrency 1",
              "warmup_requests": 50}
    if platform.system() == "Darwin":
        for key in ["machdep.cpu.brand_string", "hw.memsize"]:
            result[key] = subprocess.check_output(["sysctl", "-n", key], text=True).strip()
    with tempfile.TemporaryDirectory(prefix="atrium-bench-") as temp:
        root = Path(temp).resolve()
        config = root / "node.toml"
        config.write_text('port=0\nloopback_only=true\ndiscover_magpie=false\n'
                          'name="benchmark"\ntoken="benchmark-only-token"\n'
                          f'roots=[{json.dumps(str(root))}]\n' + ''.join(
                              f'[agents.{name}]\ncommand=["/bin/cat"]\nprotocol="raw"\n'
                              for name in ["echo", "codex", "claude", "deepseek", "antigravity"]))
        payload = b"x" * 1024
        fixture = root / "payload.bin"
        fixture.write_bytes(payload)
        # Avoid reading the owner's login-shell startup files or invoking real agents.
        env = dict(os.environ, SHELL="/bin/sh", HOME=str(root), PATH="/usr/bin:/bin")
        started = time.perf_counter()
        with (root / "stderr.log").open("wb") as err:
            proc = subprocess.Popen([str(binary), "serve", "--config", str(config)],
                                    stdout=subprocess.PIPE, stderr=err, env=env)
            conn = None
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(proc.stdout, selectors.EVENT_READ)
                    if not selector.select(15):
                        raise RuntimeError("node startup timed out")
                line = proc.stdout.readline().decode().strip()
                if not line.startswith("listening on 127.0.0.1:"):
                    raise RuntimeError(f"unexpected startup: {line}")
                result["startup_to_listen_ms"] = (time.perf_counter()-started)*1000
                port = int(line.rsplit(":", 1)[1])
                conn = http.client.HTTPConnection("127.0.0.1", port, timeout=15)

                def request(method, path, body=None, expected=200):
                    headers = {"Authorization": "Bearer benchmark-only-token"}
                    if method == "POST":
                        headers["Content-Type"] = "application/json"
                    conn.request(method, path, body, headers)
                    response = conn.getresponse()
                    data = response.read()
                    if response.status != expected:
                        raise RuntimeError(f"{method} {path}: {response.status}: {data[:500]!r}")
                    return data

                def bench(method, path, body=None, expected=200, verify=None):
                    samples = []
                    for _ in range(50):
                        request(method, path, body, expected)
                    start = time.perf_counter()
                    for _ in range(args.samples):
                        t = time.perf_counter_ns()
                        data = request(method, path, body, expected)
                        samples.append((time.perf_counter_ns()-t)/1e6)
                        if verify is not None and data != verify:
                            raise RuntimeError("response content mismatch")
                    return summary(samples, time.perf_counter()-start)

                request("GET", "/v1/node")
                time.sleep(1)
                result["idle_rss_kib"] = [rss(proc.pid) for _ in range(5)]
                query = "?" + urllib.parse.urlencode({"path": str(fixture)})
                result["node"] = bench("GET", "/v1/node")
                result["read_1k"] = bench("GET", "/v1/fs/read"+query, verify=payload)
                result["write_1k_atomic_sync"] = bench("PUT", "/v1/fs/write"+query, payload, 204)
                assert request("GET", "/v1/fs/read"+query) == payload
                spawn_samples, stop_samples = [], []
                spawn_body = json.dumps({"agent": "echo", "cwd": str(root), "owner": "benchmark"}).encode()
                for i in range(args.process_samples + 5):
                    t = time.perf_counter_ns()
                    p = json.loads(request("POST", "/v1/procs", spawn_body, 201))
                    spawn_ms = (time.perf_counter_ns()-t)/1e6
                    t = time.perf_counter_ns()
                    request("DELETE", "/v1/procs/"+p["id"], expected=204)
                    stop_ms = (time.perf_counter_ns()-t)/1e6
                    if i >= 5:
                        spawn_samples.append(spawn_ms)
                        stop_samples.append(stop_ms)
                # Reciprocal mean phase time, not independent sustained process throughput.
                result["spawn_cat"] = summary(spawn_samples, sum(spawn_samples)/1000)
                result["stop_cat_until_reaped"] = summary(stop_samples, sum(stop_samples)/1000)
                for key in ["spawn_cat", "stop_cat_until_reaped"]:
                    phase = result[key]
                    phase["reciprocal_mean_phase_s"] = phase.pop("sequential_ops_s")
                result["post_workload_rss_kib"] = rss(proc.pid)
            finally:
                if conn:
                    conn.close()
                if proc.poll() is None:
                    proc.terminate()  # Graceful node shutdown reaps only its own agents.
                    try:
                        proc.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        # Do not kill an arbitrary process tree or leave unnoticed children.
                        raise RuntimeError(f"owned node PID {proc.pid} failed graceful shutdown")
                if proc.returncode != 0:
                    raise RuntimeError(f"node exit {proc.returncode}: {(root/'stderr.log').read_text()[:1000]}")
    text = json.dumps(result, indent=2)
    print(text)
    if args.output:
        args.output.write_text(text + "\n")


if __name__ == "__main__":
    main()
