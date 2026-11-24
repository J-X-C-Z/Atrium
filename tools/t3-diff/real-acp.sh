#!/usr/bin/env bash
# Real-machine ACP/Antigravity check. Prints no credentials and stores none.
#
#   tools/t3-diff/real-acp.sh claude
#   tools/t3-diff/real-acp.sh deepseek
#   tools/t3-diff/real-acp.sh antigravity
#   ATRIUM_ACP="my-agent --flag" tools/t3-diff/real-acp.sh
#
# The agent must already be signed in. For Antigravity, run `agy acp --login`
# once yourself; this script never handles the sign-in code or token.
set -eu

AGENT="${1:-claude}"
case "$AGENT" in
  claude)      : "${ATRIUM_ACP:=claude-agent-acp}" ;;
  deepseek)    : "${ATRIUM_ACP:=dsh --profile acp}" ;;
  antigravity) : "${ATRIUM_ACP:=agy acp}" ;;
  *)           : "${ATRIUM_ACP:?set ATRIUM_ACP to the agent command}" ;;
esac
export ATRIUM_ACP
export NO_PROXY='*'
: "${CARGO_TARGET_DIR:=target}"
export CARGO_TARGET_DIR

if [ "$AGENT" = "antigravity" ] && command -v agy >/dev/null 2>&1; then
  # Version only; no account details.
  printf 'agy: %s\n' "$(agy --version 2>/dev/null | head -n 1)"
fi

echo "Handshaking $AGENT (no prompt, no credentials printed)..."
cargo test -p acp-client -- --ignored real_agent_handshake_and_session --nocapture

echo "Building the engine adapter driver..."
cargo build -p atrium-engine --bin atrium-engine-driver
cargo build -p acp-client --bin mock-acp

cat <<'EOF'

For a real turn through the engine, point the ACP scenarios at a real agent by
setting ATRIUM_ACP and building a scenario whose mock_name matches the agent's
handshake. The mock scenarios (08-12) exercise the same adapter and engine paths
without an account:

  python3 tools/t3-diff/run.py --no-t3 08-acp-thread-lifecycle
  python3 tools/t3-diff/run.py --no-t3 09-acp-antigravity-settings

No token, code or account id is read or written by this script.
EOF
