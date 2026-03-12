#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-light}"

cd "$ROOT_DIR"

report_top_dirs() {
  echo "Top directories in repo:"
  find "$ROOT_DIR" -mindepth 1 -maxdepth 1 -print0 \
    | xargs -0 du -sh 2>/dev/null \
    | sort -h \
    | tail -n 20
}

cleanup_light() {
  find "$ROOT_DIR" -type f -name "*.tsbuildinfo" -delete
  find "$ROOT_DIR" -type d \( -name "dist" -o -name ".turbo" -o -name "coverage" \) -prune -exec rm -rf {} +
}

cleanup_deep() {
  cleanup_light
  rm -rf "$ROOT_DIR/target"
  find "$ROOT_DIR" -type d -name "node_modules" -prune -exec rm -rf {} +

  if command -v pnpm >/dev/null 2>&1; then
    pnpm store prune || true
  fi
}

usage() {
  cat <<'USAGE'
Usage:
  scripts/disk-hygiene.sh report
  scripts/disk-hygiene.sh light
  scripts/disk-hygiene.sh deep
USAGE
}

before_size="$(du -sh "$ROOT_DIR" | awk '{print $1}')"

case "$MODE" in
  report)
    echo "Repo size: $before_size"
    report_top_dirs
    exit 0
    ;;
  light)
    cleanup_light
    ;;
  deep)
    cleanup_deep
    ;;
  *)
    usage
    exit 1
    ;;
esac

after_size="$(du -sh "$ROOT_DIR" | awk '{print $1}')"
echo "Repo size: $before_size -> $after_size"
report_top_dirs
