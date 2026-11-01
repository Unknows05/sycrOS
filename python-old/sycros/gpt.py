"""Read and write GPT partition tables (plus the protective MBR entry).

Pure Python, no tools needed. Works on any object with read_at/write_at
(see rawdisk.Target), so the same code edits a real USB stick or an .img file.
"""
from __future__ import annotations

import struct
import uuid
import zlib
from dataclasses import dataclass, field
from typing import List, Optional

SECTOR = 512
ENTRY_COUNT = 128
ENTRY_SIZE = 128
TABLE_SECTORS = ENTRY_COUNT * ENTRY_SIZE // SECTOR  # 32

# Partition type GUIDs
MS_BASIC_DATA = uuid.UUID("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7")
EFI_SYSTEM = uuid.UUID("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
APPLE_HFS = uuid.UUID("48465300-0000-11AA-AA11-00306543ECAC")
APPLE_APFS = uuid.UUID("7C3457EF-0000-11AA-AA11-00306543ECAC")

TYPE_NAMES = {
    MS_BASIC_DATA: "basic data",
    EFI_SYSTEM: "EFI system",
    APPLE_HFS: "Apple HFS+",
    APPLE_APFS: "Apple APFS",
}

# Ventoy marks its small EFI partition with bit 63 ("no drive letter" on Windows)
ATTR_NO_DRIVE_LETTER = 1 << 63


class GPTError(Exception):
    pass


def crc32(data: bytes) -> int:
    return zlib.crc32(data) & 0xFFFFFFFF


@dataclass
class Partition:
    type_guid: uuid.UUID
    part_guid: uuid.UUID
    first: int  # first LBA (inclusive)
    last: int  # last LBA (inclusive)
    attrs: int = 0
    name: str = ""

    @property
    def sectors(self) -> int:
        return self.last - self.first + 1

    @property
    def size(self) -> int:
        return self.sectors * SECTOR

    @property
    def type_name(self) -> str:
        return TYPE_NAMES.get(self.type_guid, str(self.type_guid))

    def pack(self) -> bytes:
        name = self.name.encode("utf-16-le")[:72].ljust(72, b"\0")
        return (
            self.type_guid.bytes_le
            + self.part_guid.bytes_le
            + struct.pack("<QQQ", self.first, self.last, self.attrs)
            + name
        )

    @classmethod
    def unpack(cls, raw: bytes) -> Optional["Partition"]:
        tguid = uuid.UUID(bytes_le=raw[0:16])
        if tguid.int == 0:
            return None
        first, last, attrs = struct.unpack_from("<QQQ", raw, 32)
        name = raw[56:128].decode("utf-16-le", "replace").split("\0", 1)[0]
        return cls(tguid, uuid.UUID(bytes_le=raw[16:32]), first, last, attrs, name)


@dataclass
class GPT:
    total_sectors: int
    disk_guid: uuid.UUID = field(default_factory=uuid.uuid4)
    # slot list, index 0 = partition 1. None = empty slot.
    slots: List[Optional[Partition]] = field(default_factory=lambda: [None] * ENTRY_COUNT)

    # ---- geometry -------------------------------------------------------
    @property
    def first_usable(self) -> int:
        return 2 + TABLE_SECTORS  # 34

    @property
    def last_usable(self) -> int:
        return self.total_sectors - 1 - TABLE_SECTORS - 1  # total - 34

    @property
    def backup_header_lba(self) -> int:
        return self.total_sectors - 1

    def partitions(self):
        """Yield (number, Partition) for used slots. Numbers start at 1."""
        for i, p in enumerate(self.slots):
            if p is not None:
                yield i + 1, p

    def free_ranges(self, start: Optional[int] = None, end: Optional[int] = None):
        """Free (first, last) LBA ranges inside [start, end]."""
        lo = self.first_usable if start is None else max(start, self.first_usable)
        hi = self.last_usable if end is None else min(end, self.last_usable)
        used = sorted((p.first, p.last) for _, p in self.partitions())
        cur = lo
        out = []
        for a, b in used:
            if b < cur:
                continue
            if a > hi:
                break
            if a > cur:
                out.append((cur, min(a - 1, hi)))
            cur = max(cur, b + 1)
        if cur <= hi:
            out.append((cur, hi))
        return out

    def find_space(self, sectors: int, start: int, align: int = 2048) -> Optional[int]:
        """First aligned LBA at or after `start` with room for `sectors`."""
        for a, b in self.free_ranges(start):
            first = -(-a // align) * align
            if first + sectors - 1 <= b:
                return first
        return None

    def add(self, part: Partition, number: Optional[int] = None) -> int:
        for n, p in self.partitions():
            if not (part.last < p.first or part.first > p.last):
                raise GPTError(f"new partition overlaps partition {n}")
        if part.first < self.first_usable or part.last > self.last_usable:
            raise GPTError("partition outside usable area")
        if number is None:
            number = next(i + 1 for i, s in enumerate(self.slots) if s is None)
        if self.slots[number - 1] is not None:
            raise GPTError(f"partition slot {number} is already used")
        self.slots[number - 1] = part
        return number

    def remove(self, number: int) -> Partition:
        p = self.slots[number - 1]
        if p is None:
            raise GPTError(f"partition {number} does not exist")
        self.slots[number - 1] = None
        return p

    # ---- encode ---------------------------------------------------------
    def table_bytes(self) -> bytes:
        return b"".join(
            (p.pack() if p else b"\0" * ENTRY_SIZE) for p in self.slots
        )

    def header_bytes(self, primary: bool, table_crc: int) -> bytes:
        my_lba = 1 if primary else self.backup_header_lba
        alt_lba = self.backup_header_lba if primary else 1
        table_lba = 2 if primary else self.backup_header_lba - TABLE_SECTORS
        fields = [
            b"EFI PART",
            0x00010000,
            92,
            0,  # header crc placeholder
            0,
            my_lba,
            alt_lba,
            self.first_usable,
            self.last_usable,
            self.disk_guid.bytes_le,
            table_lba,
            ENTRY_COUNT,
            ENTRY_SIZE,
            table_crc,
        ]
        fmt = "<8sIIIIQQQQ16sQIII"
        hdr = struct.pack(fmt, *fields)
        hdr = hdr[:16] + struct.pack("<I", crc32(hdr)) + hdr[20:]
        return hdr.ljust(SECTOR, b"\0")

    def write(self, disk) -> None:
        """Write primary + backup GPT. Leaves sector 0 (MBR) alone."""
        table = self.table_bytes()
        tcrc = crc32(table)
        disk.write_at(1 * SECTOR, self.header_bytes(True, tcrc) + table)
        disk.write_at(
            (self.backup_header_lba - TABLE_SECTORS) * SECTOR,
            table + self.header_bytes(False, tcrc),
        )

    # ---- decode ---------------------------------------------------------
    @classmethod
    def read(cls, disk, total_sectors: int) -> "GPT":
        hdr = disk.read_at(SECTOR, SECTOR)
        try:
            return cls._parse(disk, hdr, total_sectors)
        except GPTError:
            # try backup header at the end of the disk
            hdr = disk.read_at((total_sectors - 1) * SECTOR, SECTOR)
            return cls._parse(disk, hdr, total_sectors)

    @classmethod
    def _parse(cls, disk, hdr: bytes, total_sectors: int, sector: int = SECTOR) -> "GPT":
        if hdr[:8] != b"EFI PART":
            raise GPTError("no GPT partition table found")
        (hsize,) = struct.unpack_from("<I", hdr, 12)
        (hcrc,) = struct.unpack_from("<I", hdr, 16)
        check = bytearray(hdr[:hsize])
        check[16:20] = b"\0\0\0\0"
        if crc32(bytes(check)) != hcrc:
            raise GPTError("GPT header checksum is wrong")
        disk_guid = uuid.UUID(bytes_le=hdr[56:72])
        table_lba, count, esize, tcrc = struct.unpack_from("<QIII", hdr, 72)
        if esize != ENTRY_SIZE or count > 1024:
            raise GPTError("unsupported GPT entry layout")
        table = disk.read_at(table_lba * sector, count * esize)
        if crc32(table) != tcrc:
            raise GPTError("GPT partition list checksum is wrong")
        g = cls(total_sectors, disk_guid)
        slots = [Partition.unpack(table[i * esize:(i + 1) * esize]) for i in range(count)]
        g.slots = (slots + [None] * ENTRY_COUNT)[:max(ENTRY_COUNT, count)]
        return g

    @classmethod
    def parse_bytes(cls, blob: bytes, sector: int = SECTOR) -> Optional["GPT"]:
        """Parse a GPT from the start of an in-memory image (used to look inside DMGs)."""

        class _Mem:
            def read_at(self, off, n):
                return blob[off:off + n]

        try:
            return cls._parse(_Mem(), blob[sector:2 * sector], len(blob) // sector, sector)
        except (GPTError, struct.error):
            return None


def protective_mbr_entry(total_sectors: int) -> bytes:
    """The single 0xEE partition entry a GPT disk carries in its MBR."""
    count = min(total_sectors - 1, 0xFFFFFFFF)
    return struct.pack(
        "<B3sB3sII", 0x00, bytes([0, 2, 0]), 0xEE, bytes([254, 255, 255]), 1, count
    )
