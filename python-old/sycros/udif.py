"""Unpack Apple .dmg (UDIF) disk images to a plain raw image, in pure Python.

A compressed .dmg is a list of chunks (zlib, bzip2, LZMA, ADC, raw, zero).
We stream each chunk into a sparse output file, so a 15 GB installer does
not need 15 GB of RAM. LZFSE chunks (used by some newer Apple images) need
the `lzfse` command or, on a Mac, `hdiutil`.
"""
from __future__ import annotations

import bz2
import lzma
import os
import plistlib
import shutil
import struct
import subprocess
import sys
import time
import zlib

from .rawdisk import progress, progress_end

KOLY = b"koly"

ZERO, RAW, IGNORE = 0x00000000, 0x00000001, 0x00000002
ADC, ZLIB, BZIP2, LZFSE, LZMA = 0x80000004, 0x80000005, 0x80000006, 0x80000007, 0x80000008
COMMENT, END = 0x7FFFFFFE, 0xFFFFFFFF


class UDIFError(Exception):
    pass


def is_udif(path: str) -> bool:
    size = os.path.getsize(path)
    if size < 512:
        return False
    with open(path, "rb") as f:
        f.seek(size - 512)
        return f.read(4) == KOLY


def _koly(f, size):
    f.seek(size - 512)
    k = f.read(512)
    if k[:4] != KOLY:
        raise UDIFError("not a UDIF .dmg (no 'koly' trailer)")
    data_off, data_len = struct.unpack_from(">QQ", k, 24)
    xml_off, xml_len = struct.unpack_from(">QQ", k, 216)
    (sector_count,) = struct.unpack_from(">Q", k, 492)
    return data_off, xml_off, xml_len, sector_count


def _blocks(f, xml_off, xml_len):
    f.seek(xml_off)
    pl = plistlib.loads(f.read(xml_len))
    try:
        entries = pl["resource-fork"]["blkx"]
    except KeyError as e:
        raise UDIFError("dmg has no block table") from e
    for e in entries:
        mish = e["Data"]
        if mish[:4] != b"mish":
            raise UDIFError("bad block table entry")
        first_sector, _count, data_off = struct.unpack_from(">QQQ", mish, 8)
        (nchunks,) = struct.unpack_from(">I", mish, 200)
        for i in range(nchunks):
            kind, _c, sec, cnt, coff, clen = struct.unpack_from(">IIQQQQ", mish, 204 + 40 * i)
            yield kind, first_sector + sec, cnt, data_off + coff, clen


def adc_decompress(src: bytes) -> bytes:
    """Apple Data Compression (old DMGs)."""
    out = bytearray()
    i = 0
    n = len(src)
    while i < n:
        b = src[i]
        if b & 0x80:  # literal run
            ln = (b & 0x7F) + 1
            out += src[i + 1:i + 1 + ln]
            i += 1 + ln
            continue
        if b & 0x40:  # 3-byte copy
            ln = (b & 0x3F) + 4
            dist = (src[i + 1] << 8 | src[i + 2]) + 1
            i += 3
        else:  # 2-byte copy
            ln = ((b & 0x3C) >> 2) + 3
            dist = ((b & 0x03) << 8 | src[i + 1]) + 1
            i += 2
        start = len(out) - dist
        for k in range(ln):
            out.append(out[start + k])
    return bytes(out)


def _lzfse(data: bytes) -> bytes:
    tool = shutil.which("lzfse")
    if not tool:
        raise UDIFError(
            "this .dmg uses LZFSE compression. Install the 'lzfse' tool "
            "(Linux: apt install lzfse / Windows: build from github.com/lzfse/lzfse), "
            "or run sycrOS on a Mac, or convert it first: hdiutil convert in.dmg -format UDRW -o out.dmg"
        )
    r = subprocess.run([tool, "-decode"], input=data, capture_output=True, check=True)
    return r.stdout


def _decode(kind: int, data: bytes) -> bytes:
    if kind == ZLIB:
        return zlib.decompress(data)
    if kind == BZIP2:
        return bz2.decompress(data)
    if kind == LZMA:
        return lzma.decompress(data)
    if kind == ADC:
        return adc_decompress(data)
    if kind == LZFSE:
        return _lzfse(data)
    raise UDIFError(f"unknown dmg chunk type 0x{kind:08x}")


def convert(src: str, dst: str) -> int:
    """Unpack `src` .dmg to raw `dst`. Returns the raw size in bytes."""
    if sys.platform == "darwin" and shutil.which("hdiutil"):
        # Apple's own tool knows every format (including LZFSE); use it on a Mac.
        tmp = dst + ".cdr"
        subprocess.run(["hdiutil", "convert", "-quiet", src, "-format", "UDTO", "-o", tmp], check=True)
        os.replace(tmp, dst)
        return os.path.getsize(dst)

    size = os.path.getsize(src)
    with open(src, "rb") as f:
        data_fork, xml_off, xml_len, sectors = _koly(f, size)
        chunks = list(_blocks(f, xml_off, xml_len))
        total = sectors * 512 or max((s + c) * 512 for _, s, c, _, _ in chunks)
        t0 = time.time()
        with open(dst, "wb") as out:
            out.truncate(total)  # sparse: zero areas cost nothing
            done = 0
            for kind, sec, cnt, off, ln in chunks:
                if kind in (ZERO, IGNORE, COMMENT, END) or cnt == 0:
                    continue
                f.seek(data_fork + off)
                blob = f.read(ln)
                raw = blob if kind == RAW else _decode(kind, blob)
                out.seek(sec * 512)
                out.write(raw[:cnt * 512])
                done += cnt * 512
                progress("unpacking dmg", min(done, total), total, t0)
        progress("unpacking dmg", total, total, t0)
        progress_end()
    return total
