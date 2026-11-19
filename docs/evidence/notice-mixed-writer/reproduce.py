#!/usr/bin/env python3
"""Own temporary synthetic project only; requires cached Rust dependencies."""
from pathlib import Path
import subprocess, tempfile, json, os
root = Path(__file__).resolve().parents[3]
evidence = Path(__file__).resolve().parent
with tempfile.TemporaryDirectory(prefix='atrium-mixed-writer-') as d:
    d = Path(d)
    (d/'src').mkdir()
    old = subprocess.check_output(['git', 'show', '37c86bfe:crates/atrium-app/src/project.rs'], cwd=root)
    (d/'src/old_project.rs').write_bytes(old)
    config_old = subprocess.check_output(['git','show','b14819a:crates/atrium-node/src/config.rs'], cwd=root).decode()
    config_new = (root/'crates/atrium-node/src/config.rs').read_text()
    for name, source in [('old_config',config_old),('new_config',config_new)]:
        definitions = source[:source.index('pub fn path()')].replace('use anyhow::{Context, Result, bail};','')
        (d/f'src/{name}.rs').write_text(definitions + 'fn hostname() -> String {\"fixture\".into()}\n')
    (d/'src/main.rs').write_text((evidence/'main.rs.in').read_text().replace('__CURRENT_PROJECT__', str(root/'crates/atrium-app/src/project.rs')))
    (d/'Cargo.toml').write_text('[package]\nname="notice-mixed-writer"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\nanyhow="1"\nserde={version="1",features=["derive"]}\nserde_json="1"\njiff="0.2"\nsha2="0.10"\nuuid={version="1",features=["v4"]}\ntoml="0.8"\n')
    subprocess.run(['cargo','run','--offline','--release','--manifest-path',str(d/'Cargo.toml')], check=True, timeout=180)
