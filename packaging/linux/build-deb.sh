#!/usr/bin/env bash
# Build a .deb package:  packaging/linux/build-deb.sh   ->  dist/sycros_<version>_<arch>.deb
set -euo pipefail
cd "$(dirname "$0")/../.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
# Self-contained (static) program: runs on any 64-bit Linux, old or new.
TARGET=${TARGET:-x86_64-unknown-linux-musl}
ARCH=amd64
rustup target add "$TARGET" >/dev/null 2>&1 || true
cargo build --release --target "$TARGET"
ROOT=$(mktemp -d)
chmod 755 "$ROOT"
trap 'rm -rf "$ROOT"' EXIT
install -Dm755 "target/$TARGET/release/sycros" "$ROOT/usr/bin/sycros"
install -Dm644 README.md "$ROOT/usr/share/doc/sycros/README.md"
install -Dm644 packaging/linux/sycros.desktop "$ROOT/usr/share/applications/sycros.desktop"
mkdir -p "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<CTRL
Package: sycros
Version: $VERSION
Architecture: $ARCH
Maintainer: sycrOS maintainers <sycros@users.noreply.github.com>
Depends: curl, util-linux
Recommends: exfatprogs, udisks2
Section: utils
Priority: optional
Description: one USB stick for Windows, Linux and macOS installers
 sycrOS prepares a USB stick that boots Windows and Linux ISO files
 (using Ventoy's boot engine) and holds macOS installer/Recovery images
 as separate sections for the Mac boot menu. Run "sudo sycros" for the menu.
CTRL
mkdir -p dist
OUT="dist/sycros-linux-${ARCH}.deb"   # fixed name: releases/latest/download/ links keep working
fakeroot dpkg-deb --build --root-owner-group "$ROOT" "$OUT" >/dev/null
echo "$OUT"
