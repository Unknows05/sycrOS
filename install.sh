#!/bin/sh
# sycrOS installer for Linux and macOS.
#   curl -fsSL https://raw.githubusercontent.com/Unknows05/sycrOS/main/install.sh | sh
# Downloads the newest release for this computer and installs the `sycros` command.
# Then start it with:  sudo sycros
set -eu

REPO="Unknows05/sycrOS"
BASE="https://github.com/$REPO/releases/latest/download"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

say() { printf '%s\n' "$*"; }
fail() { say "sycrOS install: $*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || fail "curl is needed. Install it first (e.g. sudo apt install curl)."
SUDO=""
[ "$(id -u)" -ne 0 ] && SUDO="sudo"

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Linux)
    [ "$ARCH" = "x86_64" ] || fail "only 64-bit Intel/AMD Linux is supported for now (this is $ARCH)."
    if command -v apt >/dev/null 2>&1 && command -v dpkg >/dev/null 2>&1; then
      say "Downloading sycrOS (.deb) ..."
      curl -fL --progress-bar -o "$TMP/sycros-linux-amd64.deb" "$BASE/sycros-linux-amd64.deb"
      chmod 644 "$TMP/sycros-linux-amd64.deb"
      cp "$TMP/sycros-linux-amd64.deb" /tmp/sycros-linux-amd64.deb   # apt's helper user can read /tmp
      $SUDO apt install -y /tmp/sycros-linux-amd64.deb
      rm -f /tmp/sycros-linux-amd64.deb
    else
      say "Downloading sycrOS ..."
      curl -fL --progress-bar -o "$TMP/s.tar.gz" "$BASE/sycros-linux-amd64.tar.gz"
      tar -xzf "$TMP/s.tar.gz" -C "$TMP"
      $SUDO install -m 755 "$TMP/sycros/sycros" /usr/local/bin/sycros
    fi
    ;;
  Darwin)
    case "$ARCH" in
      arm64) FILE="sycros-macos-apple-silicon.zip" ;;
      x86_64) FILE="sycros-macos-intel.zip" ;;
      *) fail "unknown Mac type: $ARCH" ;;
    esac
    say "Downloading sycrOS for Mac ($ARCH) ..."
    curl -fL --progress-bar -o "$TMP/s.zip" "$BASE/$FILE"
    unzip -q "$TMP/s.zip" -d "$TMP"
    xattr -d com.apple.quarantine "$TMP/sycrOS/sycros" 2>/dev/null || true
    $SUDO mkdir -p /usr/local/bin
    $SUDO install -m 755 "$TMP/sycrOS/sycros" /usr/local/bin/sycros
    ;;
  *)
    fail "this script is for Linux and macOS. On Windows use install.ps1 (see README)."
    ;;
esac

say ""
say "Installed: $(sycros --version)"
say "Start it with:  sudo sycros"
