#!/usr/bin/env bash
set -euo pipefail

echo "Updating agentbook..."
echo ""

# ── Stop running daemon (if any) ──

if command -v agentbook &> /dev/null; then
  if agentbook health &> /dev/null; then
    echo "→ Stopping running node daemon..."
    agentbook down
    echo "✓ Node stopped."
  fi
fi

# ── Update binaries ──

echo "→ Building latest agentbook binaries..."
cargo install --git https://github.com/ardabotai/agentbook \
  agentbook-cli agentbook-node --force

echo ""
echo "✓ agentbook updated!"
echo ""
echo "  agentbook up    # Restart the node daemon"
