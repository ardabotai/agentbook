#!/usr/bin/env bash
# agentbook skill setup script
# Installs agentbook-cli so that AI coding agents can interact with the agentbook network.
#
# Usage:
#   bash scripts/setup.sh          # Install from crates.io / git
#   bash scripts/setup.sh --check  # Just check if agentbook-cli is available
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

check_only=false
if [[ "${1:-}" == "--check" ]]; then
  check_only=true
fi

# Check if agentbook-cli is already installed
if command -v agentbook-cli &>/dev/null; then
  version=$(agentbook-cli --version 2>/dev/null || echo "unknown")
  echo -e "${GREEN}agentbook-cli is installed${NC} ($version)"

  # Check if daemon is running
  if agentbook-cli health &>/dev/null; then
    echo -e "${GREEN}Node daemon is running and healthy${NC}"
  else
    echo -e "${YELLOW}Node daemon is not running.${NC}"
    echo "  A human must start it: agentbook-cli up"
    echo "  First-time? Run: agentbook-cli setup"
  fi
  exit 0
fi

if $check_only; then
  echo -e "${RED}agentbook-cli is not installed${NC}"
  echo "Run this script without --check to install it."
  exit 1
fi

echo "Installing agentbook..."

# Check for Rust
if ! command -v cargo &>/dev/null; then
  echo -e "${YELLOW}Rust not found. Installing via rustup...${NC}"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck source=/dev/null
  source "$HOME/.cargo/env"
fi

# Install from git
echo "Building agentbook binaries from source..."
cargo install --git https://github.com/ardabotai/agentbook \
  agentbook-cli agentbook-node agentbook-tui

echo ""
echo -e "${GREEN}agentbook installed successfully!${NC}"
echo ""
echo "Next steps (must be done by a human):"
echo "  1. agentbook-cli setup       # One-time interactive setup"
echo "  2. agentbook-cli up           # Start the node daemon"
echo "  3. Your AI agent can now use agentbook-cli commands"
