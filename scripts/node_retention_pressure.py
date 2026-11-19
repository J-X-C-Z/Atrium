#!/usr/bin/env python3
"""Bounded Linux retention/replay acceptance, not a speed competition.
Only owned loopback node + eight /bin/cat children, 128 MiB payload total.
Requires Python websockets >=14, and at least 768 MiB visible memory headroom.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import selectors
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
from websockets.sync.client import connect

CLIENTS, FRAMES, FRAME_BYTES = 8, 64, 256 * 1024
TOTAL = 64 * 1024 * 1024 + 128


def memory_gate():
    fields = dict(line.split(':', 1) for line in Path('/proc/meminfo').read_text().splitlines())
    available = int(fields['MemAvailable'].split()[0]) * 1024
    result = {'mem_available_bytes': available, 'cgroup_memory_max': None,
              'cgroup_memory_current': None, 'cgroup_visible': False}
    base = Path('/sys/fs/cgroup')
    if (base / 'memory.max').is_file() and (base / 'memory.current').is_file():
        limit = (base / 'memory.max').read_text().strip()
        current = int((base / 'memory.current').read_text())
        result.update(cgroup_visible=True, cgroup_memory_max=limit, cgroup_memory_current=current)
        if limit != 'max':
            available = min(available, int(limit) - current)
    result['visible_headroom_bytes'] = available
    if available < 768 * 1024 * 1024:
        raise RuntimeError(f'insufficient visible headroom: {result}')
    return result


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--binary', type=Path, required=True)
    ap.add_argument('--provenance', type=Path, required=True)
    ap.add_argument('--global-budget', action='store_true')
    ap.add_argument('--output', type=Path, required=True)
    ap.add_argument('--inject-monitor-failure', action='store_true', help='test cleanup with an intentional monitor failure')
    args = ap.parse_args()
    with args.output.open('x') as output:
        output.write(json.dumps({'status':'initializing'})+'\n')
    assert platform.system() == 'Linux'
    preflight = memory_gate()
    provenance = json.loads(args.provenance.read_text())
    binary = args.binary.resolve()
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    assert digest == provenance['binary_sha256']
    samples, cpu_last, owned = [], {}, []
    stop = threading.Event()
    aborted = threading.Event()
    abort_reason = []
    resource_lock = threading.Lock()
    socket_lock = threading.Lock()
    owned_sockets = []
    round_started = time.monotonic()
    node = None
    with tempfile.TemporaryDirectory(prefix='atrium-retention-pressure-') as directory:
        root = Path(directory)
        config = root / 'node.toml'
        config.write_text('port=0\nloopback_only=true\ndiscover_magpie=false\n'
                          'keep_output_lines=256\nkeep_output_bytes=134217728\ndetached_timeout_secs=30\n'
                          + (f'total_output_bytes={TOTAL}\n' if args.global_budget else '')
                          + f'roots=[{json.dumps(directory)}]\ntoken="pressure-fixture-only"\n'
                          + ''.join(f'[agents.{name}]\ncommand=["/bin/cat"]\nprotocol="raw"\n'
                                    for name in ['echo','codex','claude','deepseek','antigravity']))
        env = dict(os.environ, HOME=directory, XDG_CONFIG_HOME=directory, SHELL='/bin/sh', PATH='/usr/bin:/bin')
        try:
            node = subprocess.Popen([str(binary),'serve','--config',str(config)],env=env,
                                    stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
            owned.append(node.pid)
            with selectors.DefaultSelector() as selector:
                selector.register(node.stdout,selectors.EVENT_READ)
                assert selector.select(15), 'startup timeout'
            line = node.stdout.readline().strip()
            assert line.startswith('listening on 127.0.0.1:')
            url = 'http://' + line.removeprefix('listening on ')
            headers = {'Authorization':'Bearer pressure-fixture-only'}

            def request(method,path,body=None):
                req = urllib.request.Request(url+path, method=method,
                       data=None if body is None else json.dumps(body).encode(),
                       headers=dict(headers, **{'Content-Type':'application/json'}))
                with urllib.request.urlopen(req,timeout=20) as response:
                    data = response.read()
                    return json.loads(data) if data else None

            def collect_usage():
                rows = []
                for pid in tuple(owned):
                    try:
                        base = Path('/proc') / str(pid)
                        stat = (base/'stat').read_text().split(') ',1)[1].split()
                        status = dict(line.split(':',1) for line in (base/'status').read_text().splitlines())
                        cpu = (int(stat[11])+int(stat[12]))/os.sysconf('SC_CLK_TCK')
                        cpu_last[pid] = cpu
                        rows.append({'pid':pid,'rss_kib':int(status.get('VmRSS','0 kB').split()[0]),
                                     'cpu_seconds':cpu,'fds':len(list((base/'fd').iterdir())) if pid == node.pid else None})
                    except (FileNotFoundError, ProcessLookupError):
                        pass
                return {'t':time.monotonic(),'processes':rows,'aggregate_rss_kib':sum(r['rss_kib'] for r in rows),
                        'aggregate_cpu_seconds':sum(cpu_last.values())}

            def usage():
                with resource_lock:
                    return collect_usage()
            def check_abort():
                if aborted.is_set(): raise RuntimeError('pressure aborted: ' + '; '.join(abort_reason))
            baseline = usage()
            def monitor():
                while not stop.wait(.05):
                    try:
                        sample = usage()
                        samples.append(sample)
                        if args.inject_monitor_failure:
                            raise RuntimeError('injected monitor failure')
                        if time.monotonic() - round_started > 120:
                            raise RuntimeError('120 second whole-round deadline exceeded')
                        if sample['aggregate_rss_kib'] > 512 * 1024:
                            raise RuntimeError('owned node+cats RSS exceeded 512 MiB')
                        memory_gate()
                    except Exception as error:
                        abort_reason.append(str(error))
                        aborted.set()
                        with socket_lock:
                            for owned_socket in owned_sockets:
                                try: owned_socket.shutdown(socket.SHUT_RDWR)
                                except OSError: pass
                        # Interrupt owned sockets/children as well as future
                        # sends; never leave blocked writers generating load.
                        if node.poll() is None:
                            node.send_signal(signal.SIGTERM)
                        return
            watcher = threading.Thread(target=monitor,daemon=True)
            watcher.start()
            children = [request('POST','/v1/procs',{'agent':'echo','cwd':directory,'owner':'pressure'}) for _ in range(CLIENTS)]
            owned.extend(c['pid'] for c in children)
            def attach(child):
                ws = connect(url.replace('http:','ws:')+f'/v1/procs/{child["id"]}/attach?after=0',
                               additional_headers=headers,open_timeout=10,close_timeout=2,max_size=FRAME_BYTES*2)
                ws.socket.settimeout(10)
                with socket_lock:
                    owned_sockets.append(ws.socket)
                    if aborted.is_set():
                        try: ws.socket.shutdown(socket.SHUT_RDWR)
                        except OSError: pass
                check_abort()
                return ws
            barrier = threading.Barrier(CLIENTS)
            def fill(index,child):
                with attach(child) as ws:
                    barrier.wait(timeout=15)
                    for seq in range(1,FRAMES+1):
                        check_abort()
                        prefix=f'{index}:{seq}:'
                        payload=prefix+'x'*(FRAME_BYTES-len(prefix))
                        ws.send(json.dumps({'line':payload}))
                        echoed=json.loads(ws.recv(timeout=20))
                        assert echoed.get('line')==payload and echoed.get('seq')==seq
                return FRAMES
            load_start=time.monotonic()
            with concurrent.futures.ThreadPoolExecutor(max_workers=CLIENTS) as pool:
                echoes=list(pool.map(lambda pair:fill(*pair),enumerate(children)))
            check_abort()
            filled=usage()
            for c in children: request('DELETE','/v1/procs/'+c['id'])
            stopped=usage()
            barrier=threading.Barrier(CLIENTS)
            def replay(index,child):
                with attach(child) as ws:
                    barrier.wait(timeout=15)
                    expected,count,gap=1,0,None
                    while True:
                        check_abort()
                        frame=json.loads(ws.recv(timeout=20))
                        if 'gap' in frame:
                            assert count==0 and gap is None and frame['gap']['from']==1
                            gap=frame['gap'];expected=gap['to']+1
                            continue
                        assert frame['seq']==expected, (frame.get('seq'),expected)
                        expected+=1
                        if 'exit' in frame:
                            assert frame['seq']==FRAMES+1
                            return {'retained_frames':count,'gap':gap,'exit_seq':frame['seq']}
                        prefix=f'{index}:{frame["seq"]}:'
                        assert frame['line']==prefix+'x'*(FRAME_BYTES-len(prefix))
                        count+=1
            replay_start=time.monotonic()
            with concurrent.futures.ThreadPoolExecutor(max_workers=CLIENTS) as pool:
                replays=list(pool.map(lambda pair:replay(*pair),enumerate(children)))
            replay_seconds=time.monotonic()-replay_start
            check_abort()
            replayed=usage()
            retained=sum(r['retained_frames'] for r in replays)
            if args.global_budget:
                assert retained*(FRAME_BYTES+128)<=TOTAL
                assert any(r['gap'] for r in replays)
            else:
                assert retained==CLIENTS*FRAMES and all(r['gap'] is None for r in replays)
            deadline=time.monotonic()+40
            while request('GET','/v1/procs?owner=pressure'):
                assert time.monotonic()<deadline,'retention did not expire'
                time.sleep(.25)
            released=usage()
            assert all(not Path(f'/proc/{c["pid"]}').exists() for c in children)
            assert released['processes'][0]['fds']<=baseline['processes'][0]['fds']+2
            stop.set();watcher.join(timeout=2)
            check_abort()
            report={'status':'completed','safety_limits':{'owned_rss_max_kib':524288,'minimum_visible_headroom_bytes':805306368},'platform':platform.platform(),'preflight':preflight,'build':provenance,
                    'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    'binary_sha256':digest,'global_budget':TOTAL if args.global_budget else None,
                    'per_process_keep_bytes':134217728,'clients':CLIENTS,'frame_bytes':FRAME_BYTES,
                    'frames_per_process':FRAMES,'echoes_verified':sum(echoes),'total_payload_bytes':CLIENTS*FRAMES*FRAME_BYTES,
                    'replays':replays,'retained_payload_bytes':retained*FRAME_BYTES,'replay_seconds':replay_seconds,
                    'baseline':baseline,'filled':filled,'stopped':stopped,'replayed':replayed,'released':released,
                    'peak_sampled_node_plus_cat_rss_kib':max(s['aggregate_rss_kib'] for s in samples),
                    'node_plus_cat_cpu_seconds_workload':replayed['aggregate_cpu_seconds']-baseline['aggregate_cpu_seconds'],
                    'workload_seconds':replayed['t']-load_start,'resource_samples':samples,
                    'checks':{'echo_and_sequence':True,'concurrent_replay_gap_prefix_and_exit':True,
                              'rows_released':True,'owned_children_gone':True,'fds_near_baseline':True},
                    'limits':['debug binaries; config differs deliberately, not a speed competition',
                              '50ms sampled node+owned cats RSS excludes driver; transient peaks may be missed',
                              'cgroup memory controls may not be mounted; see preflight, no hard-limit claim',
                              'shared Linux cloud; no macOS/Windows or real-agent inference coverage',
                              'allocator RSS may remain above baseline after logical release']}
            args.output.write_text(json.dumps(report,indent=2)+'\n')
            print(json.dumps({k:v for k,v in report.items() if k not in ['resource_samples','build']},indent=2))
        except Exception as error:
            args.output.write_text(json.dumps({'status':'truncated_or_failed','error':str(error),
                'abort_reason':abort_reason,'preflight':preflight,'build':provenance,'owned_pids':owned,
                'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                'resource_samples':samples},indent=2)+'\n')
            raise
        finally:
            stop.set()
            if node and node.poll() is None:
                node.send_signal(signal.SIGTERM)
                try: node.wait(timeout=12)
                except subprocess.TimeoutExpired: node.kill();node.wait(timeout=5)

if __name__=='__main__': main()
