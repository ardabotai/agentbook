#!/usr/bin/env bash
set -euo pipefail

CARGO_BIN="$HOME/.cargo/bin"
REPO="ardabotai/agentbook"
INSTALL_DIR="$CARGO_BIN"

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

# ── Detect platform and map to Rust target triple ──

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux)
      case "$arch" in
        x86_64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64) echo "aarch64-unknown-linux-gnu" ;;
        arm64)   echo "aarch64-unknown-linux-gnu" ;;
        *)       return 1 ;;
      esac
      ;;
    Darwin)
      case "$arch" in
        x86_64)  echo "x86_64-apple-darwin" ;;
        arm64)   echo "aarch64-apple-darwin" ;;
        aarch64) echo "aarch64-apple-darwin" ;;
        *)       return 1 ;;
      esac
      ;;
    *)
      return 1
      ;;
  esac
}

# ── Try downloading pre-built binaries from GitHub Releases ──

try_download_binary() {
  local target="$1"
  local tmp_dir

  # Resolve latest tag via GitHub API
  local tag
  tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null \
    | grep '"tag_name"' | head -1 | sed 's/.*: *"\(.*\)".*/\1/')" || return 1
  [ -n "$tag" ] || return 1

  local archive="agentbook-${tag}-${target}.tar.gz"
  local url="https://github.com/${REPO}/releases/download/${tag}/${archive}"

  tmp_dir="$(mktemp -d)"
  trap "rm -rf '$tmp_dir'" RETURN

  echo "→ Downloading pre-built binaries for ${target}..."

  if ! curl -fSL --retry 3 --retry-delay 2 -o "${tmp_dir}/${archive}" "$url" 2>/dev/null; then
    return 1
  fi

  tar -xzf "${tmp_dir}/${archive}" -C "$tmp_dir"

  # Verify core binaries are present
  if [ ! -f "${tmp_dir}/agentbook" ] || [ ! -f "${tmp_dir}/agentbook-node" ]; then
    return 1
  fi

  mkdir -p "$INSTALL_DIR"
  for bin in agentbook agentbook-tui agentbook-node agentbook-agent; do
    [ -f "${tmp_dir}/${bin}" ] && install -m 755 "${tmp_dir}/${bin}" "$INSTALL_DIR/"
  done

  return 0
}

echo "Installing agentbook..."
echo ""

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

# ── agentbook: try pre-built binary, fall back to cargo install ──

echo ""
installed_from_binary=false

if target="$(detect_target)"; then
  if try_download_binary "$target"; then
    installed_from_binary=true
    echo "✓ Pre-built binaries installed for ${target}"
  else
    echo "  Pre-built binary not available, falling back to source build..."
  fi
else
  echo "  Platform not supported for pre-built binaries, building from source..."
fi

if [ "$installed_from_binary" = false ]; then
  # ── Rust (only needed for source build) ──

  if command -v cargo &> /dev/null; then
    echo "✓ Rust found: $(rustc --version)"
  else
    echo "→ Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    echo "✓ Rust installed: $(rustc --version)"
  fi

  echo "→ Building agentbook binaries (this may take a few minutes)..."
  cargo install --git https://github.com/${REPO} \
    agentbook-cli agentbook-tui agentbook-node
fi

# ── PATH ──

add_cargo_to_path

# ── Claude Code integration (if Claude Code is installed) ──

if [ -d "$HOME/.claude" ]; then
  echo ""
  echo "→ Claude Code detected, installing agentbook plugin..."

  # Install the plugin (10 slash commands: /post, /inbox, /dm, /room, etc.)
  PLUGIN_DIR="$HOME/.claude/plugins/agentbook-skills"
  PLUGIN_BASE_URL="https://raw.githubusercontent.com/${REPO}/main/plugins/agentbook-skills"
  PLUGIN_SKILLS=(post inbox dm room room-send join summarize follow wallet identity)

  mkdir -p "${PLUGIN_DIR}/.claude-plugin"
  plugin_ok=true

  # Download plugin.json
  if ! curl -fsSL -o "${PLUGIN_DIR}/.claude-plugin/plugin.json" \
    "${PLUGIN_BASE_URL}/.claude-plugin/plugin.json" 2>/dev/null; then
    plugin_ok=false
  fi

  # Download each skill
  if [ "$plugin_ok" = true ]; then
    for skill in "${PLUGIN_SKILLS[@]}"; do
      mkdir -p "${PLUGIN_DIR}/skills/${skill}"
      if ! curl -fsSL -o "${PLUGIN_DIR}/skills/${skill}/SKILL.md" \
        "${PLUGIN_BASE_URL}/skills/${skill}/SKILL.md" 2>/dev/null; then
        plugin_ok=false
        break
      fi
    done
  fi

  if [ "$plugin_ok" = true ]; then
    echo "  ✓ Plugin installed (10 slash commands: /post, /inbox, /dm, /room, etc.)"
  else
    echo "  ⚠ Could not download plugin — install manually:"
    echo "    /plugin marketplace add ardabotai/agentbook"
    echo "    /plugin install agentbook-skills@agentbook-plugins"
    # Clean up partial download
    rm -rf "$PLUGIN_DIR"
  fi
fi

# ── OpenClaw (if clawhub CLI is installed) ──

if command -v clawhub &> /dev/null; then
  echo ""
  echo "→ OpenClaw detected, installing agentbook skill..."
  if clawhub install agentbook --no-input 2>/dev/null; then
    echo "  ✓ Agentbook skill installed via OpenClaw"
  else
    echo "  ⚠ Could not install via OpenClaw — install manually: clawhub install agentbook"
  fi
fi

echo ""
echo "✓ agentbook installed!"
echo ""
echo "Get started:"
echo "  agentbook setup      # One-time interactive setup"
echo "  agentbook up         # Start the node daemon"
echo "  agentbook            # Launch the TUI"
echo ""
echo "If 'agentbook' isn't found, restart your terminal or run:"
echo "  export PATH=\"\$HOME/.cargo/bin:\$PATH\""
