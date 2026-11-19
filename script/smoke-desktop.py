"""Exercise the extracted desktop binary as an MCP child over redirected stdio."""

import json
import subprocess
import sys
import tempfile
from pathlib import Path


def smoke(executable):
    protocol = "2025-06-18"
    requests = [
        {"id": 1, "method": "initialize", "params": {
            "protocolVersion": protocol, "capabilities": {},
            "clientInfo": {"name": "package-smoke", "version": "1"},
        }},
        {"method": "notifications/initialized"},
        {"id": 2, "method": "tools/list"},
    ]
    wire = "".join(json.dumps({"jsonrpc": "2.0", **req}) + "\n" for req in requests)
    with tempfile.TemporaryDirectory(prefix="atrium desktop ") as project:
        process = subprocess.run(
            [str(executable), "mcp", "--project", project, "--stream", "package-smoke"],
            cwd=project, input=wire, stdout=subprocess.PIPE, encoding="utf-8",
            timeout=30, check=True,
        )
    init, listing = (json.loads(line) for line in process.stdout.splitlines())
    if [(r["jsonrpc"], r["id"]) for r in (init, listing)] != [("2.0", 1), ("2.0", 2)]:
        raise ValueError("Unexpected JSON-RPC envelope or reply order")
    if init["result"]["protocolVersion"] != protocol or init["result"]["serverInfo"]["name"] != "atrium":
        raise ValueError("Unexpected MCP initialization result")
    if "whoami" not in {tool["name"] for tool in listing["result"]["tools"]}:
        raise ValueError("Missing Atrium whoami tool")
    print("Packaged MCP passed initialize and tools/list")


if __name__ == "__main__":
    smoke(Path(sys.argv[1]).resolve(strict=True))
