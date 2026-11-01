#!/usr/bin/env bash
# Linux / macOS launcher. Disk commands need root, so this re-runs itself with sudo.
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
PY="$(command -v python3 || true)"
[ -z "$PY" ] && { echo "Python 3 is needed (Linux: apt install python3 / macOS: xcode-select --install)"; exit 1; }
case "$1" in
  init|add|remove)
    case "$2" in *.img|*.bin|*.raw) ;; *) [ "$(id -u)" -ne 0 ] && exec sudo "$PY" "$HERE/sycros.py" "$@";; esac ;;
esac
exec "$PY" "$HERE/sycros.py" "$@"
