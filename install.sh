#!/usr/bin/env bash
set -euo pipefail

echo "Installing agentbook..."
echo ""

# ── Rust ──

if command -v cargo &> /dev/null; then
  echo "✓ Rust found: $(rustc --version)"
else
  echo "→ Installing Rust..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
  echo "✓ Rust installed: $(rustc --version)"
fi

# ── Node.js (for the AI agent) ──

if command -v node &> /dev/null; then
  echo "✓ Node.js found: $(node --version)"
else
  echo "→ Installing Node.js via fnm..."
  curl -fsSL https://fnm.vercel.app/install | bash
  export PATH="$HOME/.local/share/fnm:$PATH"
  eval "$(fnm env)"
  fnm install 22
  fnm use 22
  echo "✓ Node.js installed: $(node --version)"
fi

# ── agentbook ──

echo ""
echo "→ Building agentbook binaries (this may take a few minutes)..."
cargo install --git https://github.com/ardabotai/agentbook \
  agentbook-cli agentbook-node

echo ""
echo "✓ agentbook installed!"
echo ""
echo "Get started:"
echo "  agentbook up        # Start the node daemon"
echo "  agentbook identity  # Show your identity"
echo "  agentbook --help    # See all commands"
