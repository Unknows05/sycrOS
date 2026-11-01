"""Raw byte access to a whole disk or an image file.

Real disks (/dev/sdX, /dev/rdiskN, \\\\.\\PhysicalDriveN) only accept reads and
writes in whole sectors at sector boundaries. Target hides that: any offset or
length works, it reads/patches/writes the surrounding sectors when needed.
"""
from __future__ import annotations

import os
import sys
import time

SECTOR = 512
CHUNK = 4 * 1024 * 1024


class Target:
    def __init__(self, path: str, size: int, is_image: bool, writable: bool = True):
        self.path = path
        self.size = size
        self.is_image = is_image
        flags = os.O_RDWR if writable else os.O_RDONLY
        flags |= getattr(os, "O_BINARY", 0)
        self.fd = os.open(path, flags)

    @property
    def sectors(self) -> int:
        return self.size // SECTOR

    # -- basic aligned primitives -------------------------------------------
    def _pread(self, off: int, n: int) -> bytes:
        os.lseek(self.fd, off, os.SEEK_SET)
        out = bytearray()
        while len(out) < n:
            chunk = os.read(self.fd, n - len(out))
            if not chunk:
                break
            out += chunk
        return bytes(out).ljust(n, b"\0")

    def _pwrite(self, off: int, data: bytes) -> None:
        os.lseek(self.fd, off, os.SEEK_SET)
        view = memoryview(data)
        while view:
            n = os.write(self.fd, view)
            view = view[n:]

    # -- public: any offset / length -----------------------------------------
    def read_at(self, off: int, n: int) -> bytes:
        if self.is_image:
            return self._pread(off, n)
        a = off - off % SECTOR
        b = -(-(off + n) // SECTOR) * SECTOR
        blob = self._pread(a, b - a)
        return blob[off - a:off - a + n]

    def write_at(self, off: int, data: bytes) -> None:
        if off + len(data) > self.size:
            raise IOError("write goes past the end of the disk")
        if self.is_image or (off % SECTOR == 0 and len(data) % SECTOR == 0):
            self._pwrite(off, data)
            return
        a = off - off % SECTOR
        b = -(-(off + len(data)) // SECTOR) * SECTOR
        blob = bytearray(self._pread(a, b - a))
        blob[off - a:off - a + len(data)] = data
        self._pwrite(a, bytes(blob))

    def zero(self, off: int, n: int) -> None:
        z = b"\0" * min(n, CHUNK)
        while n > 0:
            k = min(n, len(z))
            self.write_at(off, z[:k])
            off += k
            n -= k

    def copy_from_file(self, src_path: str, src_off: int, length: int, dst_off: int,
                       label: str = "writing") -> None:
        """Copy `length` bytes of a file into the disk, with a progress line."""
        done = 0
        t0 = time.time()
        with open(src_path, "rb") as f:
            f.seek(src_off)
            while done < length:
                want = min(CHUNK, length - done)
                buf = f.read(want)
                if len(buf) < want:
                    buf = buf.ljust(want, b"\0")  # source ended early: pad with zeros
                self.write_at(dst_off + done, buf)
                done += want
                progress(label, done, length, t0)
        progress_end()

    def flush(self) -> None:
        try:
            os.fsync(self.fd)
        except OSError:
            pass  # some raw devices refuse fsync; writes are unbuffered there anyway

    def close(self) -> None:
        if self.fd is not None:
            self.flush()
            os.close(self.fd)
            self.fd = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


def human(n: float) -> str:
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if abs(n) < 1024 or unit == "TB":
            return f"{n:.1f} {unit}" if unit != "B" else f"{int(n)} B"
        n /= 1024
    return str(n)


_last = [0.0]


def progress(label: str, done: int, total: int, t0: float) -> None:
    now = time.time()
    if now - _last[0] < 0.5 and done < total:
        return
    _last[0] = now
    pct = 100.0 * done / total if total else 100.0
    speed = done / max(now - t0, 0.001)
    sys.stderr.write(f"\r  {label}: {pct:5.1f}%  {human(done)} / {human(total)}  ({human(speed)}/s)   ")
    sys.stderr.flush()


def progress_end() -> None:
    sys.stderr.write("\n")
    sys.stderr.flush()
