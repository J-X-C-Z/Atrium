#!/usr/bin/env python3
"""Run the GPUI-free project model using the workspace lockfile."""
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
sys.exit(subprocess.call(["cargo", "test", "--offline", "--locked", "-p", "atrium-project", *sys.argv[1:]], cwd=root))
