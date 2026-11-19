#!/usr/bin/env python3
"""Linux-only isolated bounded soak; Python 3 + websockets >= 14.
Not a saturation benchmark: 8 closed-loop clients, 100 iterations/s/client ceiling.
No real agents/accounts, service installation or non-loopback network traffic.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import tempfile
import threading
import time
import urllib.request

from websockets.sync.client import connect


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--binary', required=True, type=Path)
    ap.add_argument('--seconds', type=int, default=120)
    ap.add_argument('--clients', type=int, default=8)
    ap.add_argument('--output', type=Path, required=True)
    ap.add_argument('--label', default='unlabelled')
    ap.add_argument('--provenance', type=Path, help='explicit build manifest for archived sources')
    args = ap.parse_args()
    if platform.system() != 'Linux' or not 2 <= args.seconds <= 300 or not 1 <= args.clients <= 16:
        ap.error('requires Linux, 2..300 seconds, 1..16 clients')
    binary = args.binary.resolve()
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    provenance = json.loads(args.provenance.read_text()) if args.provenance else None
    if provenance:
        assert provenance['binary_sha256'] == binary_hash, 'binary differs from build manifest'
        commit = provenance['source_commit']
        source_diff_hash = provenance.get('node_tracked_diff_sha256')
    else:
        commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
        source_diff = subprocess.check_output(['git', 'diff', '--', 'crates/atrium-node'])
        source_diff_hash = hashlib.sha256(source_diff).hexdigest()
    token = 'isolated-load-test-only'
    samples, stopped = [], threading.Event()
    proc = None
    with tempfile.TemporaryDirectory(prefix='atrium-load-') as directory:
        root = Path(directory)
        config = root / 'node.toml'
        config.write_text('port=0\nloopback_only=true\ndiscover_magpie=false\n'
                          'keep_output_lines=128\nkeep_output_bytes=67109888\ndetached_timeout_secs=5\n'
                          f'token="{token}"\nroots=[{json.dumps(directory)}]\n' + ''.join(
            f'[agents.{name}]\ncommand=["/bin/cat"]\nprotocol="raw"\n'
            for name in ['echo', 'codex', 'claude', 'deepseek', 'antigravity']))
        env = dict(os.environ, HOME=directory, XDG_CONFIG_HOME=directory)
        try:
            proc = subprocess.Popen([str(binary), 'serve', '--config', str(config)],
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
            # Startup is bounded even if no listening line arrives.
            import selectors
            selector = selectors.DefaultSelector()
            selector.register(proc.stdout, selectors.EVENT_READ)
            assert selector.select(15), 'startup timed out'
            line = proc.stdout.readline().strip()
            selector.close()
            assert line.startswith('listening on '), line
            url = 'http://' + line.removeprefix('listening on ')
            headers = {'Authorization': 'Bearer ' + token}

            def request(method, path, body=None):
                data = None if body is None else json.dumps(body).encode()
                req = urllib.request.Request(url + path, data=data, method=method,
                                             headers=dict(headers, **{'Content-Type': 'application/json'}))
                with urllib.request.urlopen(req, timeout=15) as response:
                    raw = response.read()
                    return json.loads(raw) if raw else None

            def usage():
                base = Path('/proc') / str(proc.pid)
                stat = (base / 'stat').read_text().split(') ', 1)[1].split()
                status = dict(line.split(':', 1) for line in (base / 'status').read_text().splitlines())
                return {'t': time.monotonic(), 'rss_kib': int(status['VmRSS'].split()[0]),
                        'hwm_kib': int(status['VmHWM'].split()[0]),
                        'cpu_seconds': (int(stat[11]) + int(stat[12])) / os.sysconf('SC_CLK_TCK'),
                        'fds': len(list((base / 'fd').iterdir()))}

            def monitor():
                while not stopped.wait(.2):
                    samples.append(usage())

            baseline = usage()
            monitor_thread = threading.Thread(target=monitor, daemon=True)
            monitor_thread.start()
            children = [request('POST', '/v1/procs', {'agent': 'echo', 'cwd': directory,
                         'owner': 'isolated-soak', 'args': [], 'env': {}}) for _ in range(args.clients)]
            barrier = threading.Barrier(args.clients)

            def worker(index, child):
                def attach(after):
                    return connect(url.replace('http:', 'ws:') + f'/v1/procs/{child["id"]}/attach?after={after}',
                                   additional_headers=headers, open_timeout=5, close_timeout=2)
                ws = attach(0)
                last, count, reconnects, latencies = 0, 0, 0, []
                barrier.wait(timeout=10)
                start = time.monotonic()
                try:
                    while time.monotonic() - start < args.seconds:
                        tick = time.monotonic()
                        payload = f'{index}:{count}:' + 'x' * 1024
                        ws.send(json.dumps({'line': payload}))
                        frame = json.loads(ws.recv(timeout=5))
                        assert frame.get('line') == payload, frame
                        assert frame['seq'] == last + 1, frame
                        latencies.append((time.monotonic() - tick) * 1000)
                        last, count = frame['seq'], count + 1
                        if count % 250 == 0:
                            ws.close()
                            ws = attach(last - 1)
                            replay = json.loads(ws.recv(timeout=5))
                            assert replay == frame, (replay, frame)
                            reconnects += 1
                        # Explicit bounded offered rate, not maximum server throughput.
                        time.sleep(max(0, .01 - (time.monotonic() - tick)))
                finally:
                    ws.close()
                return {'count': count, 'reconnects': reconnects, 'latencies': latencies,
                        'seconds': time.monotonic() - start}

            start = time.monotonic()
            with concurrent.futures.ThreadPoolExecutor(max_workers=args.clients) as pool:
                futures = [pool.submit(worker, i, child) for i, child in enumerate(children)]
                results = [future.result() for future in futures]
            elapsed = time.monotonic() - start
            loaded = usage()
            for child in children:
                request('DELETE', '/v1/procs/' + child['id'])
            after_stop = usage()
            # Rows intentionally retain output after exit; maintenance expires them.
            expiry = time.monotonic() + 10
            rows = request('GET', '/v1/procs?owner=isolated-soak')
            while rows and time.monotonic() < expiry:
                time.sleep(.25)
                rows = request('GET', '/v1/procs?owner=isolated-soak')
            assert not rows, 'ended process retention did not expire'
            retained = usage()
            assert all(not Path(f'/proc/{c["pid"]}').exists() for c in children), 'owned child still present'
            assert retained['fds'] <= baseline['fds'] + 2, 'FDs failed to return near baseline'
            stopped.set()
            monitor_thread.join(timeout=2)
            values = sorted(v for result in results for v in result['latencies'])
            percentile = lambda p: values[min(len(values)-1, int((len(values)-1)*p))]
            peak_cpu = max((b['cpu_seconds']-a['cpu_seconds']) / (b['t']-a['t']) * 100
                           for a,b in zip(samples, samples[1:]))
            report = {'platform': platform.platform(), 'binary': str(binary),
                      'binary_sha256': binary_hash, 'label': args.label,
                      'source_commit': commit, 'node_tracked_diff_sha256': source_diff_hash,
                      'build_provenance': provenance,
                      'build_profile': 'debug; not release performance',
                      'duration_seconds': elapsed, 'clients': args.clients, 'payload_bytes_min': 1024,
                      'offered_rate_ceiling_per_second': args.clients * 100,
                      'echoes_verified': len(values), 'throughput_per_second': len(values)/elapsed,
                      'latency_ms': {'p50': percentile(.5), 'p95': percentile(.95), 'max': max(values)},
                      'reconnect_replays_verified': sum(r['reconnects'] for r in results),
                      'node_only_resources': {'baseline': baseline, 'after_load': loaded,
                          'after_stop': after_stop, 'after_retention': retained,
                          'peak_rss_kib': max(s['rss_kib'] for s in samples),
                          'kernel_peak_rss_kib': retained['hwm_kib'],
                          'peak_cpu_percent_one_core_200ms_sample': peak_cpu,
                          'average_cpu_percent_one_core_load':
                              (loaded['cpu_seconds']-baseline['cpu_seconds'])/elapsed*100},
                      'checks': {'ordered_echo': True, 'reconnect_replay': True,
                                 'retention_empty': True, 'owned_children_gone': True,
                                 'fd_return_near_baseline': True},
                      'limits': ['Linux cloud only; no macOS/Windows runtime claim',
                          'Bounded rate echo soak, not saturation or real LLM latency',
                          'RSS/CPU count node only; excludes client and mock processes',
                          'No slow-reader, 64MiB frame, or crash/restart coverage in this script',
                          'Allocator RSS need not return to baseline; FD/child/row release checked']}
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps(report, indent=2))
        finally:
            stopped.set()
            if proc is not None:
                if proc.poll() is None:
                    proc.send_signal(signal.SIGTERM)
                    try:
                        proc.wait(timeout=12)
                    except subprocess.TimeoutExpired:
                        # Only the test-owned node; normal node shutdown owns children.
                        proc.kill()
                        proc.wait(timeout=5)
                if proc.stderr:
                    error = proc.stderr.read()
                    if error:
                        print('node stderr:', error)


if __name__ == '__main__':
    main()
