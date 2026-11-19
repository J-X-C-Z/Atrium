#!/usr/bin/env python3
"""Prepare an isolated localhost mock demo. Never edit the real user's settings."""
import argparse
import json
import os
from pathlib import Path
import secrets
import shlex


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True, help='new or empty demo directory')
    parser.add_argument('--bin-dir', type=Path, default=Path('target/debug'))
    parser.add_argument('--port', type=int, default=7381)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error('port must be between 1 and 65535')
    binaries = args.bin_dir.resolve()
    for name in ('atrium-node', 'mock-codex', 'mock-acp'):
        if not (binaries / name).is_file():
            parser.error(f'build {name} first; missing {binaries / name}')
    root = args.output.resolve()
    if root.exists() and (not root.is_dir() or any(root.iterdir())):
        parser.error('output must be new or empty; no existing files will be overwritten')
    root.mkdir(parents=True, exist_ok=True)
    os.chmod(root, 0o700)
    work = root / 'work'
    work.mkdir()
    home = root / 'home'
    home.mkdir()
    token = secrets.token_hex(24)
    config = (f'port={args.port}\nloopback_only=true\ndiscover_magpie=false\n'
              f'name="atrium-local-demo"\nroots=[{json.dumps(str(work))}]\n'
              f'token={json.dumps(token)}\n[agents.codex]\n'
              f'command=[{json.dumps(str(binaries / "mock-codex"))}]\n'
              'protocol="codex-app-server"\n')
    commands = {}
    for name in ('claude', 'deepseek', 'antigravity'):
        program = str(binaries / 'mock-acp')
        config += (f'[agents.{name}]\ncommand=[{json.dumps(program)},"--name",{json.dumps(name)}]\n'
                   'protocol="acp"\n')
        commands[name] = f'{shlex.quote(program)} --name {shlex.quote(name)}'
    config_path = root / 'node.toml'
    config_path.write_text(config)
    os.chmod(config_path, 0o600)
    settings = {'cwd': str(work), 'nodes': [f'127.0.0.1:{args.port}'],
                'node_token': token, 'codex_binary': str(binaries / 'mock-codex'),
                'agent_commands': commands}
    # macOS dirs::config_dir uses HOME/Library/Application Support. The second
    # path supports verifying the same isolated configuration on Linux.
    for directory in (home / 'Library/Application Support/Atrium', home / '.config/Atrium'):
        directory.mkdir(parents=True)
        path = directory / 'settings.json'
        path.write_text(json.dumps(settings, indent=2) + '\n')
        os.chmod(path, 0o600)
    print(f'Demo directory: {root}')
    print(f'Node config: {config_path}')
    print(f'Client address: http://127.0.0.1:{args.port}')
    print(f'Working root: {work}')
    print('Token saved privately in the demo config/settings; not printed.')
    print('Nothing has been started or installed.')


if __name__ == '__main__':
    main()
