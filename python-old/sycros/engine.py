"""The boot engine: Ventoy's prebuilt boot files.

sycrOS writes the disk layout itself, but the code that actually boots ISO
files (a patched GRUB for BIOS + UEFI, with Secure Boot shim) comes from the
official Ventoy release. We download one pinned version, check its SHA-256,
and keep only the files we need in a cache folder.
"""
from __future__ import annotations

import hashlib
import lzma
import os
import shutil
import sys
import tarfile
import urllib.request
from pathlib import Path

VENTOY_VERSION = "1.1.18"
TARBALL = f"ventoy-{VENTOY_VERSION}-linux.tar.gz"
URL = f"https://github.com/ventoy/Ventoy/releases/download/v{VENTOY_VERSION}/{TARBALL}"
# from https://github.com/ventoy/Ventoy/releases/download/v1.1.18/sha256.txt
SHA256 = "d86ff9de63d94c8b8f1f6b14d23c55af4b88e8207bf2893890892a6a97b1ad54"

# files we keep (path inside the tarball, minus the top folder)
WANTED = {
    "boot/boot.img",
    "boot/core.img.xz",
    "ventoy/ventoy.disk.img.xz",
    "ventoy/ventoy_4k.disk.img.xz",
    "tool/x86_64/mkexfatfs.xz",
    "tool/aarch64/mkexfatfs.xz",
    "tool/i386/mkexfatfs.xz",
}

EFI_PART_SECTORS = 65536  # Ventoy's EFI partition is always 32 MiB
CORE_MAX_SECTORS = 2014  # sectors 34..2047 on a GPT disk


def cache_root() -> Path:
    env = os.environ.get("SYCROS_CACHE")
    if env:
        return Path(env)
    if sys.platform == "win32":
        base = Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData" / "Local"))
    elif sys.platform == "darwin":
        base = Path.home() / "Library" / "Caches"
    else:
        base = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return base / "sycros"


class Engine:
    def __init__(self, folder: Path):
        self.folder = folder

    @property
    def version(self) -> str:
        return VENTOY_VERSION

    def _file(self, rel: str) -> Path:
        p = self.folder / rel
        if not p.exists():
            raise FileNotFoundError(f"boot engine file missing: {p} (run: sycros engine --refresh)")
        return p

    def boot_img(self) -> bytes:
        data = self._file("boot/boot.img").read_bytes()
        assert len(data) == 512
        return data

    def core_img(self) -> bytes:
        data = lzma.decompress(self._file("boot/core.img.xz").read_bytes())
        # core.img is padded to 2047 sectors (MBR style). On GPT only 2014 fit;
        # Ventoy writes just those, so the cut-off tail must be empty padding.
        keep = CORE_MAX_SECTORS * 512
        if data[keep:].count(0) != len(data[keep:]):
            raise ValueError("core.img is too big for the GPT boot gap")
        return data[:keep]

    def efi_img_path(self, sector_4k: bool = False) -> Path:
        """Decompressed 32 MiB EFI partition image (cached)."""
        name = "ventoy_4k.disk.img" if sector_4k else "ventoy.disk.img"
        out = self.folder / "ventoy" / name
        if not out.exists():
            src = self._file(f"ventoy/{name}.xz")
            tmp = out.with_suffix(".part")
            with lzma.open(src) as fin, open(tmp, "wb") as fout:
                shutil.copyfileobj(fin, fout, 1 << 20)
            tmp.replace(out)
        return out

    def mkexfatfs(self) -> Path | None:
        """Ventoy's static exFAT formatter for Linux (used when mkfs.exfat is missing)."""
        if not sys.platform.startswith("linux"):
            return None
        arch = os.uname().machine
        sub = {"x86_64": "x86_64", "aarch64": "aarch64", "i686": "i386", "i386": "i386"}.get(arch)
        if not sub:
            return None
        out = self.folder / "tool" / sub / "mkexfatfs"
        if not out.exists():
            src = self.folder / "tool" / sub / "mkexfatfs.xz"
            if not src.exists():
                return None
            out.write_bytes(lzma.decompress(src.read_bytes()))
            out.chmod(0o755)
        return out


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def get_engine(refresh: bool = False, tarball: str | None = None, quiet: bool = False) -> Engine:
    """Return a ready Engine, downloading + verifying it the first time."""
    folder = cache_root() / f"ventoy-{VENTOY_VERSION}"
    marker = folder / ".ok"
    if marker.exists() and not refresh:
        return Engine(folder)

    folder.mkdir(parents=True, exist_ok=True)
    tar_path = Path(tarball) if tarball else cache_root() / TARBALL
    if not tar_path.exists() or refresh and not tarball:
        if not quiet:
            print(f"Downloading boot engine (Ventoy {VENTOY_VERSION}, ~20 MB) ...")
        tmp = tar_path.with_suffix(".part")
        with urllib.request.urlopen(URL, timeout=60) as r, open(tmp, "wb") as f:
            shutil.copyfileobj(r, f, 1 << 20)
        tmp.replace(tar_path)

    got = _sha256(tar_path)
    if got != SHA256:
        raise RuntimeError(
            f"boot engine checksum does not match!\n  expected {SHA256}\n  got      {got}\n"
            f"Delete {tar_path} and try again."
        )

    with tarfile.open(tar_path, "r:gz") as tf:
        for m in tf.getmembers():
            name = m.name[2:] if m.name.startswith("./") else m.name
            parts = name.split("/", 1)
            if len(parts) != 2 or parts[1] not in WANTED or not m.isfile():
                continue
            dest = folder / parts[1]
            dest.parent.mkdir(parents=True, exist_ok=True)
            src = tf.extractfile(m)
            with open(dest, "wb") as out:
                shutil.copyfileobj(src, out)
    missing = [w for w in WANTED if not (folder / w).exists() and "mkexfatfs" not in w]
    if missing:
        raise RuntimeError(f"boot engine is incomplete, missing: {missing}")
    marker.write_text(VENTOY_VERSION)
    if not quiet:
        print(f"Boot engine ready in {folder}")
    return Engine(folder)
