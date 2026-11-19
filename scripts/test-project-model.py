#!/usr/bin/env python3
"""Test the real project persistence model without GPUI or real agent accounts.

Uses an isolated Cargo manifest/target in a temporary directory, pointing its
library directly at project.rs (not a copied implementation). Pass extra Cargo
test arguments after the script, e.g. -- --nocapture. Requires cached crates;
network is disabled by Cargo's --offline flag.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="atrium-project-harness-") as directory:
    manifest = Path(directory) / "Cargo.toml"
    manifest.write_text(
        '[package]\nname="atrium-project-harness"\nversion="0.0.0"\nedition="2024"\n'
        '[workspace]\n[lib]\npath=' + json.dumps(str(root / "crates/atrium-app/src/project.rs")) + '\n'
        '[dependencies]\nanyhow="1"\nserde={version="1",features=["derive"]}\nserde_json="1"\njiff="0.2"\nsha2="0.10"\nuuid={version="1",features=["v4"]}\n',
        encoding="utf-8",
    )
    result = subprocess.run(
        ["cargo", "test", "--offline", "--manifest-path", str(manifest), *sys.argv[1:]],
        check=False,
        timeout=180,
    )
    sys.exit(result.returncode)
