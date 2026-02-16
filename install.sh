#!/usr/bin/env bash
set -euo pipefail

CARGO_BIN="$HOME/.cargo/bin"

# ── Ensure ~/.cargo/bin is on PATH (for this session and future shells) ──

ensure_in_path() {
  local line="export PATH=\"\$HOME/.cargo/bin:\$PATH\""
  local rc_file="$1"

  if [ -f "$rc_file" ] && grep -q '.cargo/bin' "$rc_file" 2>/dev/null; then
    return
  fi

  echo "" >> "$rc_file"
  echo "# Added by agentbook installer" >> "$rc_file"
  echo "$line" >> "$rc_file"
  echo "  → Added ~/.cargo/bin to $rc_file"
}

add_cargo_to_path() {
  # Add to current session
  export PATH="$CARGO_BIN:$PATH"

  # Add to shell rc files for future sessions
  local shell_name
  shell_name="$(basename "${SHELL:-/bin/bash}")"

  case "$shell_name" in
    zsh)  ensure_in_path "$HOME/.zshrc" ;;
    bash)
      # Prefer .bashrc, fall back to .bash_profile on macOS
      if [ -f "$HOME/.bashrc" ]; then
        ensure_in_path "$HOME/.bashrc"
      else
        ensure_in_path "$HOME/.bash_profile"
      fi
      ;;
    *)
      ensure_in_path "$HOME/.profile"
      ;;
  esac
}

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

# ── PATH ──

add_cargo_to_path

echo ""
echo "✓ agentbook installed!"
echo ""
echo "Get started:"
echo "  agentbook setup     # One-time interactive setup"
echo "  agentbook up        # Start the node daemon"
echo "  agentbook --help    # See all commands"
echo ""
echo "If 'agentbook' isn't found, restart your terminal or run:"
echo "  export PATH=\"\$HOME/.cargo/bin:\$PATH\""
