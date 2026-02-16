#!/usr/bin/env bash
set -euo pipefail

echo "Updating agentbook..."
echo ""

# ── Stop running daemon (if any) ──

if command -v agentbook-cli &> /dev/null; then
  if agentbook-cli health &> /dev/null; then
    echo "→ Stopping running node daemon..."
    agentbook-cli down
    echo "✓ Node stopped."
  fi
fi

# ── Update binaries ──

echo "→ Building latest agentbook binaries..."
cargo install --git https://github.com/ardabotai/agentbook \
  agentbook-tui agentbook-cli agentbook-node --force

echo ""
echo "✓ agentbook updated!"
echo ""
echo "  agentbook-cli up  # Restart the node daemon"
echo "  agentbook         # Launch the TUI"
