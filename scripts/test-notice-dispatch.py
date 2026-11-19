#!/usr/bin/env python3
"""Exercise the actual pure Workspace dispatch helpers and Project model.

Does not compile GPUI/Workspace itself or replace the macOS app CI check.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="atrium-dispatch-harness-") as directory:
    directory = Path(directory)
    (directory / "lib.rs").write_text(
        "#![allow(dead_code)]\n"
        + "use atrium_project as project;\n#[path="
        + json.dumps(str(root / "crates/atrium-app/src/notice_dispatch.rs"))
        + "]\nmod notice_dispatch;\n",
        encoding="utf-8",
    )
    manifest = directory / "Cargo.toml"
    manifest.write_text(
        '[package]\nname="atrium-dispatch-harness"\nversion="0.0.0"\nedition="2024"\n'
        '[workspace]\n[lib]\npath="lib.rs"\n[dependencies]\nanyhow="1"\n'
        'uuid={version="1",features=["v4"]}\natrium-project={path='
        + json.dumps(str(root / "crates/atrium-project")) + '}\ncodex-client={path='
        + json.dumps(str(root / "crates/codex-client")) + '}\n',
        encoding="utf-8",
    )
    result = subprocess.run(
        ["cargo", "test", "--offline", "--manifest-path", str(manifest), *sys.argv[1:]],
        check=False,
        timeout=180,
    )
    sys.exit(result.returncode)
