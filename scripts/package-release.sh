#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

TARGET="${1:-$(rustc -vV | awk '/^host:/ {print $2}')}"
OUT_DIR="${2:-dist}"

BINARIES=(
  tmax-local
  tmax
  tmax-web
  tmaxent
  tmax-sandbox-runner
)

cargo build --workspace --release --bins --target "$TARGET"

BIN_DIR="target/$TARGET/release"
PKG_NAME="tmax-$TARGET"
PKG_DIR="$OUT_DIR/$PKG_NAME"
ARCHIVE="$OUT_DIR/$PKG_NAME.tar.gz"

rm -rf "$PKG_DIR"
mkdir -p "$PKG_DIR/bin"

for bin in "${BINARIES[@]}"; do
  src="$BIN_DIR/$bin"
  if [[ ! -x "$src" ]]; then
    echo "missing built binary: $src" >&2
    exit 1
  fi
  cp "$src" "$PKG_DIR/bin/$bin"
done

cp README.md "$PKG_DIR/README.md"

if [[ -d "ops/systemd" ]]; then
  mkdir -p "$PKG_DIR/ops/systemd"
  cp ops/systemd/* "$PKG_DIR/ops/systemd/"
fi

rm -f "$ARCHIVE"
tar -C "$OUT_DIR" -czf "$ARCHIVE" "$PKG_NAME"

echo "created $ARCHIVE"
