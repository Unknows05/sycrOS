"""Find the bootable macOS volume inside a .dmg / .iso / .img file.

Handles:
  * plain HFS+ volume (header 'H+'/'HX' at byte 1024) - e.g. mkmaciso ISOs
  * APFS container ('NXSB' at byte 32)
  * GPT disk image (mkmaciso .dmg.img, hdiutil UDRW, createinstallmedia dumps)
  * Apple Partition Map (old hybrid CDs) and MBR type 0xAF
  * compressed UDIF .dmg (unpacked first, see udif.py)
"""
from __future__ import annotations

import os
import struct
import tempfile
from dataclasses import dataclass
from typing import Optional

from . import gpt as gptmod
from . import udif


@dataclass
class MacVolume:
    source: str  # file that holds the raw bytes (maybe a temp unpacked copy)
    offset: int  # where the volume starts in `source`
    length: int  # volume size in bytes
    fs: str  # "hfs" or "apfs"
    label: str
    temp: bool = False  # True if `source` is our temp file (delete after use)

    @property
    def kind(self) -> str:
        """installer / recovery / macos (best guess from the name)."""
        low = self.label.lower()
        if "install" in low:
            return "installer"
        if "base system" in low or "recovery" in low or "basesystem" in low:
            return "recovery"
        return "macos"

    @property
    def gpt_type(self):
        return gptmod.APPLE_APFS if self.fs == "apfs" else gptmod.APPLE_HFS

    def cleanup(self):
        if self.temp and os.path.exists(self.source):
            os.remove(self.source)


def _read(f, off, n):
    f.seek(off)
    return f.read(n)


def hfs_info(f, off) -> Optional[tuple]:
    """(length, label) if an HFS+ volume starts at `off`."""
    h = _read(f, off + 1024, 512)
    if len(h) < 512 or h[:2] not in (b"H+", b"HX"):
        return None
    block_size, total_blocks = struct.unpack_from(">II", h, 40)
    if block_size < 512 or block_size & (block_size - 1) or total_blocks == 0:
        return None
    return block_size * total_blocks, _hfs_label(f, off, h, block_size)


def _hfs_label(f, off, hdr, block_size) -> str:
    """Volume name = name of the root folder record (parent ID 1) in the catalog."""
    try:
        # catalog file fork: logicalSize(8) clump(4) totalBlocks(4) extents 8x(start,count)
        cat = hdr[272:272 + 80]
        start_block, _cnt = struct.unpack_from(">II", cat, 16)
        cat_off = off + start_block * block_size
        node0 = _read(f, cat_off, 512)
        first_leaf, = struct.unpack_from(">I", node0, 14 + 10)
        node_size, = struct.unpack_from(">H", node0, 14 + 18)
        # first leaf is assumed inside the first extent (always true in practice)
        leaf = _read(f, cat_off + first_leaf * node_size, node_size)
        rec_off, = struct.unpack_from(">H", leaf, node_size - 2)
        key_len, parent = struct.unpack_from(">HI", leaf, rec_off)
        if parent != 1:
            return ""
        name_len, = struct.unpack_from(">H", leaf, rec_off + 6)
        return leaf[rec_off + 8:rec_off + 8 + 2 * name_len].decode("utf-16-be", "replace")
    except (struct.error, ValueError):
        return ""


def apfs_info(f, off) -> Optional[int]:
    b = _read(f, off, 64)
    if len(b) < 48 or b[32:36] != b"NXSB":
        return None
    block_size, = struct.unpack_from("<I", b, 36)
    block_count, = struct.unpack_from("<Q", b, 40)
    return block_size * block_count


def _sniff(f, off, name_hint):
    """Return (length, fs, label) for a volume at `off`, or None."""
    h = hfs_info(f, off)
    if h:
        return h[0], "hfs", h[1] or name_hint
    a = apfs_info(f, off)
    if a:
        return a, "apfs", name_hint
    return None


def _candidates(f, size):
    """Yield (offset, max_length, name) for every partition-like area."""
    yield 0, size, ""
    head = _read(f, 0, 64 * 1024)
    # GPT (512- or 4096-byte sectors)
    for sec in (512, 4096):
        g = gptmod.GPT.parse_bytes(head, sec)
        if g:
            for _, p in g.partitions():
                if p.type_guid in (gptmod.EFI_SYSTEM,):
                    continue
                yield p.first * sec, p.sectors * sec, p.name
    # Apple Partition Map
    if head[:2] == b"ER":
        blk, = struct.unpack_from(">H", head, 2)
        for unit in {512, blk or 512}:
            for i in range(1, 64):
                e = _read(f, i * unit, 512)
                if e[:2] != b"PM":
                    break
                start, count = struct.unpack_from(">II", e, 8)
                ptype = e[48:80].split(b"\0")[0]
                if ptype.startswith(b"Apple_HFS"):
                    for u in {512, blk or 512}:
                        yield start * u, count * u, e[16:48].split(b"\0")[0].decode(errors="replace")
    # MBR, type 0xAF (HFS+)
    if head[510:512] == b"\x55\xaa":
        for i in range(4):
            e = head[446 + 16 * i:462 + 16 * i]
            if e[4] == 0xAF:
                start, count = struct.unpack_from("<II", e, 8)
                yield start * 512, count * 512, ""


def find_volume(path: str, workdir: Optional[str] = None) -> MacVolume:
    """Locate the macOS volume in `path`. Raises ValueError if there is none."""
    src, temp = path, False
    if udif.is_udif(path):
        fd, raw = tempfile.mkstemp(prefix="sycros-", suffix=".raw", dir=workdir)
        os.close(fd)
        try:
            udif.convert(path, raw)
        except Exception:
            os.remove(raw)
            raise
        src, temp = raw, True

    hint = _name_from_file(path)
    size = os.path.getsize(src)
    best = None
    with open(src, "rb") as f:
        for off, max_len, pname in _candidates(f, size):
            found = _sniff(f, off, pname or hint)
            if found:
                length, fs, label = found
                length = min(length, max_len, size - off)
                # prefer the biggest volume (an installer beats a tiny helper volume)
                if best is None or length > best[1]:
                    best = (off, length, fs, label)
    if best is None:
        if temp:
            os.remove(src)
        raise ValueError(
            f"{os.path.basename(path)}: no macOS volume found inside "
            "(not a macOS installer/recovery image?)"
        )
    off, length, fs, label = best
    return MacVolume(src, off, length, fs, label or hint, temp)


def looks_like_mac(path: str) -> bool:
    """Cheap check used to auto-sort .iso files: Mac image or Windows/Linux ISO?"""
    low = path.lower()
    if low.endswith((".dmg", ".dmg.img")):
        return True
    try:
        if udif.is_udif(path):
            return True
        with open(path, "rb") as f:
            size = os.path.getsize(path)
            for off, _l, _n in _candidates(f, size):
                if _sniff(f, off, ""):
                    return True
    except OSError:
        pass
    return False


def _name_from_file(path: str) -> str:
    base = os.path.basename(path)
    for ext in (".dmg.img", ".dmg", ".iso", ".img", ".cdr"):
        if base.lower().endswith(ext):
            base = base[: -len(ext)]
            break
    return base.replace("_", " ").strip() or "macOS"
