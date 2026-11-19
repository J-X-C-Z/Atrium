#!/usr/bin/env python3
"""Bounded native release loopback load. Python 3.11 stdlib, Linux/macOS."""
import argparse
import base64
import concurrent.futures
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import secrets
import socket
import struct
import subprocess
import tempfile
import threading
import time
import urllib.request

spec = importlib.util.spec_from_file_location('package', Path(__file__).with_name('node-package.py'))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)
CLIENTS, FRAMES, PAYLOAD = 2, 64, 16384
BASELINE_ROUNDS = 32
BASELINE_PERIOD_NS = 50_000_000
BASELINE_MAX_NS = 120_000_000_000


def rtt_summary(samples):
    ordered = sorted(samples)
    if not ordered:
        raise ValueError('no verified echo RTT samples')
    return {'samples': len(ordered), **{
        f'p{p}_ms': ordered[math.ceil(len(ordered) * p / 100) - 1] / 1e6
        for p in (50, 95, 99)
    }}


def pace(slot_ns, limit_ns, clock=time.monotonic_ns, sleep=time.sleep):
    while (remaining := slot_ns - clock()) > 0:
        sleep(remaining / 1e9)
    actual_ns = slot_ns - remaining
    if actual_ns >= limit_ns:
        raise RuntimeError('paced baseline exceeded the 120s load budget')
    return actual_ns



def exact(sock, size, deadline=None):
    if deadline is None: deadline = time.monotonic() + 3
    data = bytearray()
    while len(data) < size:
        remaining = deadline - time.monotonic()
        if remaining <= 0: raise TimeoutError("socket operation deadline")
        sock.settimeout(remaining)
        part = sock.recv(size - len(data))
        if not part: raise EOFError('socket closed')
        data.extend(part)
    return bytes(data)


def http_body(response):
    deadline = time.monotonic() + 3
    data = bytearray()
    while True:
        if time.monotonic() >= deadline: raise TimeoutError('HTTP body deadline')
        part = response.read1(65537-len(data))
        if not part: return bytes(data)
        data.extend(part)
        if len(data)>65536: raise ValueError('large HTTP response')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('control HTTP redirects are forbidden')


class SocketGuard:
    """Own sockets only; abort and late registration are serialized."""
    def __init__(self):
        self.lock = threading.Lock()
        self.aborted = threading.Event()
        self.sockets = []

    def register(self, sock):
        with self.lock:
            rejected = self.aborted.is_set()
            if not rejected: self.sockets.append(sock)
        if rejected:
            try: sock.shutdown(socket.SHUT_RDWR)
            except OSError: pass
            sock.close()
            raise RuntimeError('socket registration after abort')

    def abort(self):
        with self.lock:
            self.aborted.set()
            sockets = list(self.sockets)
        for sock in sockets:
            try: sock.shutdown(socket.SHUT_RDWR)
            except OSError: pass


def validate_retention(gap, retained, count, small):
    if small:
        if gap != {'from': 1, 'to': count - 8} or retained != 8:
            raise ValueError('small retention must retain exactly eight data frames')
    elif gap is not None or retained != count:
        raise ValueError('default retention must preserve every data frame without gap')


class WS:
    """Only this fixture's bounded, unfragmented text protocol; no extensions."""
    def __init__(self, port, route, token, register=None):
        deadline = time.monotonic() + 3
        self.sock = socket.create_connection(('127.0.0.1', port), timeout=3)
        try:
            if register: register(self.sock)
            self.sock.settimeout(max(0.001, deadline-time.monotonic()))
            key = base64.b64encode(secrets.token_bytes(16)).decode()
            self.sock.sendall((f'GET {route} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nAuthorization: Bearer {token}\r\n\r\n').encode())
            header = bytearray()
            while not header.endswith(b'\r\n\r\n'):
                if len(header) >= 8192: raise ValueError('large handshake')
                header.extend(exact(self.sock, 1, deadline))
            lines = header.decode('ascii').split('\r\n')
            fields = {k.lower(): v.strip() for k, v in (line.split(':', 1) for line in lines[1:] if ':' in line)}
            accept = base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
            if (lines[0] != 'HTTP/1.1 101 Switching Protocols'
                or fields.get('sec-websocket-accept', '').strip() != accept
                or fields.get('upgrade', '').lower() != 'websocket'
                or 'upgrade' not in [v.strip().lower() for v in fields.get('connection','').split(',')]
                or 'sec-websocket-extensions' in fields):
                raise ValueError('invalid upgrade')
        except BaseException:
            self.sock.close(); raise

    def send(self, value):
        raw = json.dumps(value).encode()
        if len(raw) > 65535: raise ValueError('large fixture input')
        mask = secrets.token_bytes(4)
        length = bytes([len(raw) | 128]) if len(raw) < 126 else bytes([126 | 128])+struct.pack('!H', len(raw))
        self.sock.settimeout(3)
        self.sock.sendall(b'\x81'+length+mask+bytes(b ^ mask[i % 4] for i, b in enumerate(raw)))

    def recv(self):
        deadline = time.monotonic() + 3
        head = exact(self.sock, 2, deadline)
        if head[0] != 0x81 or head[1] & 128: raise ValueError('unexpected fragmented/control/masked frame')
        size = head[1] & 127
        if size == 126: size = struct.unpack('!H', exact(self.sock, 2, deadline))[0]
        elif size == 127: size = struct.unpack('!Q', exact(self.sock, 8, deadline))[0]
        if size > 65536: raise ValueError('large fixture output')
        return json.loads(exact(self.sock, size, deadline))

    def close(self):
        self.sock.close()


def cpu_seconds(value):
    days, _, clock = value.rpartition('-')
    parts = [float(p) for p in clock.split(':')]
    if len(parts) not in (2, 3): raise ValueError('unexpected ps CPU format')
    return (int(days) * 86400 if days else 0) + sum(v * 60**i for i, v in enumerate(reversed(parts)))


def sample(node, children):
    ids = [node.pid] + children
    output = subprocess.check_output(['ps', '-o', 'pid=,ppid=,rss=,time=', '-p', ','.join(map(str, ids))], timeout=3).decode()
    rows = []
    for line in output.splitlines():
        pid, ppid, rss, cpu = line.split()
        pid, ppid = int(pid), int(ppid)
        if pid == node.pid or pid in children and ppid == node.pid:
            rows.append({'pid': pid, 'role': 'node' if pid == node.pid else 'owned_cat', 'rss_kib': int(rss), 'cpu_seconds': cpu_seconds(cpu)})
    if not any(row['pid'] == node.pid for row in rows): raise RuntimeError('node missing from ps')
    return {'t': time.monotonic(), 'node_rss_kib': sum(r['rss_kib'] for r in rows if r['role']=='node'), 'children_rss_kib': sum(r['rss_kib'] for r in rows if r['role']=='owned_cat'), 'rows': rows}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--expected-sha256', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--rounds', type=int)
    parser.add_argument('--paced-baseline', action='store_true',
                        help='fixed 102.4s small-retention release measurement')
    parser.add_argument('--small-retention', action='store_true')
    parser.add_argument('--inject-monitor-failure', action='store_true')
    args = parser.parse_args()
    if args.paced_baseline and (args.rounds is not None or args.small_retention):
        parser.error('--paced-baseline fixes rounds and retention')
    if args.paced_baseline:
        args.rounds, args.small_retention = BASELINE_ROUNDS, True
    elif args.rounds is None:
        args.rounds = 2
    if not 1 <= args.rounds <= 60: raise ValueError('rounds must be 1..60')
    manifest, binary, hashes = package.verify(args.archive, args.expected_sha256)
    if manifest.get('profile') != 'release' or not manifest.get('source_commit'):
        raise ValueError('release profile and source provenance required')
    if args.paced_baseline and manifest.get('source_dirty') is not False:
        raise ValueError('paced baseline requires a clean source build')
    if any(manifest['binary'][k] != v for k, v in package.host_identity().items()): raise ValueError('native archive required')
    with args.output.open('x') as report:
        report.write('{"status":"initializing"}\n'); report.flush()
        result = {'status': 'failed', 'build': manifest, 'hashes': hashes, 'driver_sha256': package.sha(Path(__file__).read_bytes()), 'package_helper_sha256': package.sha(Path(__file__).with_name('node-package.py').read_bytes()), 'small_retention': args.small_retention, 'clients': CLIENTS, 'frames_per_round': FRAMES, 'payload_bytes': PAYLOAD, 'rounds': args.rounds}
        node = None; watcher = None; pids = []; samples = []; stop = threading.Event(); errors = []; guard = SocketGuard()
        started = time.monotonic(); deadline = started + 180
        try:
            with tempfile.TemporaryDirectory(prefix='atrium-portable-load-') as directory:
                root = Path(directory); token = secrets.token_hex(32)
                executable = root/'node'; executable.write_bytes(binary); executable.chmod(0o700)
                config = root/'node.toml'
                config.write_text('port=0\nloopback_only=true\ndiscover_magpie=false\n'+f'token="{token}"\nroots=[{json.dumps(directory)}]\n'+('keep_output_lines=8\n' if args.small_retention else '')+''.join(f'[agents.{name}]\ncommand=["/bin/cat"]\nprotocol="raw"\n' for name in ['echo','codex','claude','deepseek','antigravity']))
                config.chmod(0o600)
                with (root/'stderr').open('wb') as stderr:
                    node = subprocess.Popen([str(executable),'serve','--config',str(config)], stdout=subprocess.PIPE, stderr=stderr, env={'HOME':directory,'XDG_CONFIG_HOME':directory,'PATH':'/usr/bin:/bin','SHELL':'/bin/false','LANG':'C','LC_ALL':'C'})
                    def monitor():
                        try:
                            while not stop.is_set():
                                current=sample(node,pids); samples.append(current)
                                if args.inject_monitor_failure: raise RuntimeError('injected monitor failure')
                                if time.monotonic()>deadline or current['node_rss_kib']+current['children_rss_kib']>524288:
                                    raise RuntimeError('deadline or 512MiB sampled RSS guard')
                                stop.wait(.1)
                        except Exception as error:
                            errors.append(str(error))
                            guard.abort()
                            if node.poll() is None: node.terminate()
                    watcher=threading.Thread(target=monitor,daemon=True); watcher.start()
                    line = package.startup_line(node.stdout)
                    prefix = 'listening on 127.0.0.1:'
                    if not line.startswith(prefix): raise ValueError('not loopback')
                    port = int(line[len(prefix):]); opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
                    def request(method, path, body=None):
                        raw = json.dumps(body).encode() if body is not None else None
                        req = urllib.request.Request(f'http://127.0.0.1:{port}'+path, raw, {'Authorization':'Bearer '+token,'Content-Type':'application/json'}, method=method)
                        with opener.open(req, timeout=3) as response:
                            data=http_body(response)
                            return json.loads(data) if data else None
                    children = []
                    for _ in range(CLIENTS):
                        child = request('POST','/v1/procs',{'agent':'echo','cwd':directory,'owner':'portable-load'})
                        children.append(child); pids.append(child['pid'])
                    def attach(child, after):
                        return WS(port,f'/v1/procs/{child["id"]}/attach?after={after}',token,guard.register)
                    def payload(index,seq):
                        prefix=f'{index}:{seq}:'
                        return prefix+'x'*(PAYLOAD-len(prefix))
                    epoch_ns = time.monotonic_ns() + 500_000_000
                    span_ns = BASELINE_ROUNDS * FRAMES * BASELINE_PERIOD_NS
                    def load(index):
                        child=children[index]; seq=0; rtts=[]; previous_send=epoch_ns-BASELINE_PERIOD_NS
                        for _ in range(args.rounds):
                            ws=attach(child,seq)
                            try:
                                for _ in range(FRAMES):
                                    if errors or time.monotonic()>deadline: raise RuntimeError('load aborted')
                                    text=payload(index,seq+1)
                                    if args.paced_baseline:
                                        slot = epoch_ns + seq * BASELINE_PERIOD_NS
                                        pace(max(slot, previous_send + BASELINE_PERIOD_NS),
                                             epoch_ns + BASELINE_MAX_NS)
                                    seq+=1
                                    sent_ns = time.monotonic_ns()
                                    previous_send = sent_ns
                                    ws.send({'line':text}); frame=ws.recv()
                                    received_ns = time.monotonic_ns()
                                    if frame.get('seq')!=seq or frame.get('line')!=text: raise ValueError('echo/sequence mismatch')
                                    if args.paced_baseline: rtts.append(received_ns - sent_ns)
                            finally: ws.close()
                        return seq, rtts, previous_send
                    try:
                        with concurrent.futures.ThreadPoolExecutor(max_workers=CLIENTS) as pool: runs=list(pool.map(load,range(CLIENTS)))
                        counts = [run[0] for run in runs]
                        if args.paced_baseline:
                            end_ns = epoch_ns + span_ns
                            while (remaining := end_ns - time.monotonic_ns()) > 0:
                                if guard.aborted.is_set(): raise RuntimeError('baseline aborted')
                                stop.wait(min(remaining / 1e9, .05))
                            sample_count = BASELINE_ROUNDS * FRAMES
                            if any(count != sample_count or len(samples) != sample_count
                                   for count, samples, _ in runs):
                                raise ValueError('incomplete paced baseline')
                            observed_ns = time.monotonic_ns() - epoch_ns
                            if observed_ns > BASELINE_MAX_NS:
                                raise RuntimeError('paced baseline exceeded the 120s load budget')
                            result['paced_baseline'] = {
                                'scheduled_seconds': span_ns / 1e9,
                                'observed_seconds': observed_ns / 1e9,
                                'nominal_echoes_per_second': CLIENTS * 1e9 / BASELINE_PERIOD_NS,
                                'observed_echoes_per_second': sum(counts) * 1e9 / observed_ns,
                                'max_schedule_drift_ms': (max(run[2] for run in runs) -
                                                          (end_ns - BASELINE_PERIOD_NS)) / 1e6,
                                'rtt_by_client': [rtt_summary(run[1]) for run in runs],
                                'rtt': rtt_summary([rtt for run in runs for rtt in run[1]]),
                            }
                        # Snapshot cats before exit; ps cannot recover CPU after exit.
                        samples.append(sample(node,pids))
                        for child in children: request('DELETE','/v1/procs/'+child['id'])
                        def replay(index):
                            ws=attach(children[index],0); expected=1; gap=None; retained=0
                            try:
                                while True:
                                    frame=ws.recv()
                                    if 'gap' in frame:
                                        if retained or gap or frame['gap']['from']!=1: raise ValueError('invalid gap prefix')
                                        gap=frame['gap']; expected=gap['to']+1; continue
                                    if frame.get('seq')!=expected: raise ValueError('replay skipped sequence')
                                    expected+=1
                                    if 'exit' in frame:
                                        if frame['seq']!=counts[index]+1: raise ValueError('early exit')
                                        validate_retention(gap,retained,counts[index],args.small_retention)
                                        return {'gap':gap,'retained':retained,'exit_seq':frame['seq']}
                                    if frame.get('line')!=payload(index,frame['seq']): raise ValueError('corrupt replay')
                                    retained+=1
                            finally: ws.close()
                        with concurrent.futures.ThreadPoolExecutor(max_workers=CLIENTS) as pool: replays=list(pool.map(replay,range(CLIENTS)))
                        if errors: raise RuntimeError('; '.join(errors))
                        result.update(echoes_verified=sum(counts),replays=replays)
                    finally:
                        stop.set(); watcher.join(timeout=4)
                        package.stop_owned(node); node.stdout.close()
                        if node.returncode!=0: raise RuntimeError('unclean node shutdown')
                        if watcher.is_alive() or errors: raise RuntimeError('monitor did not finish successfully: '+str(errors))
                result['status']='completed'
        except BaseException as error:
            result['error']=str(error)
            raise
        finally:
            stop.set()
            if node and node.poll() is None: package.stop_owned(node)
            if node and node.stdout: node.stdout.close()
            if watcher: watcher.join(timeout=4)
            samples.sort(key=lambda point: point["t"])
            result.update(samples=samples,wall_seconds=time.monotonic()-started,monitor_errors=errors)
            if samples:
                observed = {}
                for point in samples:
                    for row in point['rows']:
                        observed.setdefault(row['pid'], []).append(row['cpu_seconds'])
                cpu_delta = sum(max(values)-min(values) for values in observed.values())
                result.update(peak_sampled_node_rss_kib=max(s['node_rss_kib'] for s in samples),
                              peak_sampled_children_rss_kib=max(s['children_rss_kib'] for s in samples),
                              sampled_cpu_delta_seconds=cpu_delta,
                              sampled_cpu_single_core_percent=100*cpu_delta/result['wall_seconds'],
                              max_sample_gap_seconds=max((b['t']-a['t'] for a,b in zip(samples,samples[1:])),default=0))
            result['limits']=['sampled RSS is not a hard cap; transient peaks may be missed','ps cumulative CPU is coarse and loses exited-process tail; no CPU speedup claim','wall includes driver/network; driver RSS excluded','same-binary acceptance only, not optimization comparison','small retention changes configuration; default mode preserves defaults','no GUI, service, real agent or tailnet acceptance']
            report.seek(0); report.truncate(); json.dump(result,report,indent=2); report.write('\n'); report.flush()

if __name__=='__main__': main()
