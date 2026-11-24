#!/usr/bin/env bash
# One real `claude` turn, for the owner's acceptance check (item 23).
#
# Requires a signed-in `claude` CLI. Set CLAUDE_BIN to use another path and
# CLAUDE_MODEL to pin a model. The transcript is sanitized: event kinds,
# assistant text, terminal status and token counts, never the environment,
# credentials, headers or raw frames.
set -euo pipefail

cd "$(dirname "$0")/.."

binary="${CLAUDE_BIN:-claude}"
if ! command -v "$binary" >/dev/null 2>&1 && [[ ! -x "$binary" ]]; then
  echo "claude binary not found: $binary (set CLAUDE_BIN)" >&2
  exit 1
fi

export OUT="${OUT:-claude-check-transcript.jsonl}"
echo "Starting one real Claude turn; a sanitized transcript goes to $OUT."
cargo run -q -p claude-client --example real_claude_check
