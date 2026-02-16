#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<USAGE
Usage: $(basename "$0") --artifact PATH [options]

Required:
  --artifact PATH           Release tarball (for example: dist/tmax-x86_64-unknown-linux-gnu.tar.gz)

Options:
  --release-name NAME       Name used under releases/ (default: artifact basename without .tar.gz)
  --install-root PATH       Install root (default: /opt/tmax)
  --etc-dir PATH            Config directory (default: /etc/tmax)
  --service-name NAME       systemd service name (default: tmax-local)
  --socket PATH             Socket for post-deploy health check (default: /run/tmax/tmax.sock)
  --systemctl-bin PATH      systemctl binary (default: systemctl)
  --dry-run                 Print commands without executing them
  -h, --help                Show this help
USAGE
}

ARTIFACT=""
RELEASE_NAME=""
INSTALL_ROOT="/opt/tmax"
ETC_DIR="/etc/tmax"
SERVICE_NAME="tmax-local"
SOCKET_PATH="/run/tmax/tmax.sock"
SYSTEMCTL_BIN="systemctl"
DRY_RUN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifact)
      ARTIFACT="${2:-}"
      shift 2
      ;;
    --release-name)
      RELEASE_NAME="${2:-}"
      shift 2
      ;;
    --install-root)
      INSTALL_ROOT="${2:-}"
      shift 2
      ;;
    --etc-dir)
      ETC_DIR="${2:-}"
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

if [[ -z "$ARTIFACT" ]]; then
  echo "--artifact is required" >&2
  usage >&2
  exit 1
fi
if [[ ! -f "$ARTIFACT" ]]; then
  echo "artifact not found: $ARTIFACT" >&2
  exit 1
fi

if [[ -z "$RELEASE_NAME" ]]; then
  RELEASE_NAME="$(basename "$ARTIFACT")"
  RELEASE_NAME="${RELEASE_NAME%.tar.gz}"
fi

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
TARGET_DIR="$RELEASES_DIR/$RELEASE_NAME"
SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

run tar -xzf "$ARTIFACT" -C "$TMP_DIR"

if [[ "$DRY_RUN" -eq 1 ]]; then
  EXTRACTED_DIR="$TMP_DIR/$RELEASE_NAME"
else
  extracted_dirs=()
  while IFS= read -r line; do
    extracted_dirs+=("$line")
  done < <(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d)
  if [[ "${#extracted_dirs[@]}" -ne 1 ]]; then
    echo "expected one top-level directory in artifact, found ${#extracted_dirs[@]}" >&2
    exit 1
  fi
  EXTRACTED_DIR="${extracted_dirs[0]}"
fi

run install -d -m 0755 "$RELEASES_DIR"
run install -d -m 0755 "$ETC_DIR"
if [[ "$DRY_RUN" -eq 0 ]]; then
  rm -rf "$TARGET_DIR"
else
  printf '+ rm -rf %q\n' "$TARGET_DIR"
fi
run mv "$EXTRACTED_DIR" "$TARGET_DIR"
run ln -sfn "$TARGET_DIR" "$CURRENT_LINK"

UNIT_SRC="$TARGET_DIR/ops/systemd/tmax-local.service"
TOML_SRC="$TARGET_DIR/ops/systemd/tmax-local.toml"
ENV_SRC="$TARGET_DIR/ops/systemd/tmax-local.env"

if [[ "$DRY_RUN" -eq 0 ]]; then
  [[ -f "$UNIT_SRC" ]] || { echo "missing $UNIT_SRC" >&2; exit 1; }
  [[ -f "$TOML_SRC" ]] || { echo "missing $TOML_SRC" >&2; exit 1; }
  [[ -f "$ENV_SRC" ]] || { echo "missing $ENV_SRC" >&2; exit 1; }
fi

run install -m 0644 "$UNIT_SRC" "$SERVICE_FILE"

if [[ -f "$ETC_DIR/tmax-local.toml" ]]; then
  echo "preserving existing $ETC_DIR/tmax-local.toml"
else
  run install -m 0644 "$TOML_SRC" "$ETC_DIR/tmax-local.toml"
fi

if [[ -f "$ETC_DIR/tmax-local.env" ]]; then
  echo "preserving existing $ETC_DIR/tmax-local.env"
else
  run install -m 0644 "$ENV_SRC" "$ETC_DIR/tmax-local.env"
fi

run "$SYSTEMCTL_BIN" daemon-reload
run "$SYSTEMCTL_BIN" enable --now "$SERVICE_NAME"
run "$CURRENT_LINK/bin/tmax" --socket "$SOCKET_PATH" health --json

echo "deployment complete: release=$RELEASE_NAME current=$CURRENT_LINK service=$SERVICE_NAME"
