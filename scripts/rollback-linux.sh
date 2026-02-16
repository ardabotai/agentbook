#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<USAGE
Usage: $(basename "$0") [options]

Options:
  --target NAME             Release directory name under releases/ to roll back to
  --install-root PATH       Install root (default: /opt/tmax)
  --service-name NAME       systemd service name (default: tmax-local)
  --socket PATH             Socket for post-rollback health check (default: /run/tmax/tmax.sock)
  --systemctl-bin PATH      systemctl binary (default: systemctl)
  --dry-run                 Print commands without executing them
  -h, --help                Show this help
USAGE
}

TARGET_NAME=""
INSTALL_ROOT="/opt/tmax"
SERVICE_NAME="tmax-local"
SOCKET_PATH="/run/tmax/tmax.sock"
SYSTEMCTL_BIN="systemctl"
DRY_RUN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      TARGET_NAME="${2:-}"
      shift 2
      ;;
    --install-root)
      INSTALL_ROOT="${2:-}"
      shift 2
      ;;
    --service-name)
      SERVICE_NAME="${2:-}"
      shift 2
      ;;
    --socket)
      SOCKET_PATH="${2:-}"
      shift 2
      ;;
    --systemctl-bin)
      SYSTEMCTL_BIN="${2:-}"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ "$DRY_RUN" -eq 0 ]]; then
  if ! command -v "$SYSTEMCTL_BIN" >/dev/null 2>&1; then
    echo "systemctl not found: $SYSTEMCTL_BIN" >&2
    exit 1
  fi
fi

run() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf '+'
    for arg in "$@"; do
      printf ' %q' "$arg"
    done
    printf '\n'
  else
    "$@"
  fi
}

RELEASES_DIR="$INSTALL_ROOT/releases"
CURRENT_LINK="$INSTALL_ROOT/current"
if [[ ! -d "$RELEASES_DIR" ]]; then
  echo "missing releases directory: $RELEASES_DIR" >&2
  exit 1
fi

CURRENT_TARGET=""
if [[ -L "$CURRENT_LINK" ]]; then
  CURRENT_TARGET="$(readlink -f "$CURRENT_LINK")"
fi

releases=()
while IFS= read -r line; do
  releases+=("${line%/}")
done < <(
  ls -1dt "$RELEASES_DIR"/*/ 2>/dev/null || true
)
if [[ "${#releases[@]}" -lt 2 && -z "$TARGET_NAME" ]]; then
  echo "need at least two releases for automatic rollback" >&2
  exit 1
fi

TARGET_PATH=""
if [[ -n "$TARGET_NAME" ]]; then
  TARGET_PATH="$RELEASES_DIR/$TARGET_NAME"
  if [[ ! -d "$TARGET_PATH" ]]; then
    echo "target release does not exist: $TARGET_PATH" >&2
    exit 1
  fi
else
  for candidate in "${releases[@]}"; do
    resolved="$(readlink -f "$candidate")"
    if [[ "$resolved" != "$CURRENT_TARGET" ]]; then
      TARGET_PATH="$candidate"
      break
    fi
  done
fi

if [[ -z "$TARGET_PATH" ]]; then
  echo "could not determine rollback target" >&2
  exit 1
fi

run ln -sfn "$TARGET_PATH" "$CURRENT_LINK"
run "$SYSTEMCTL_BIN" restart "$SERVICE_NAME"
run "$CURRENT_LINK/bin/tmax" --socket "$SOCKET_PATH" health --json

echo "rollback complete: target=$(basename "$TARGET_PATH") service=$SERVICE_NAME"
