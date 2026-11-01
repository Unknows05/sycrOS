"""sycrOS disk layout - build it, read it, add/remove macOS slots.

GPT disk, same first two partitions as Ventoy so Ventoy's boot engine works:

  sector 0        protective MBR + Ventoy boot.img (legacy BIOS boot)
  sectors 34-2047 Ventoy core.img (GRUB core for BIOS)
  partition 1     "Ventoy"   exFAT  - your ISO/WIM/IMG/VHD files (Windows/Linux/Tools)
  partition 2     "VTOYEFI"  32 MiB - Ventoy UEFI boot files (BIOS+UEFI, Secure Boot shim)
  partition 3..N  macOS slots, HFS+ or APFS, one per macOS image (Mac boot menu)
  free            room for more macOS slots ("Mac space", chosen at init)

Who sees what:
  PC (BIOS/UEFI)          -> Ventoy menu: Windows / Linux / Tools
  Intel Mac (hold Option) -> "EFI Boot" (= Ventoy menu) + every macOS slot
  Apple silicon Mac       -> macOS full-installer slots only (hold power button)
"""
from __future__ import annotations

import os
import secrets
import uuid
from typing import List, Optional, Tuple

from . import gpt as G
from . import hosts, library, macimg
from .engine import EFI_PART_SECTORS, Engine
from .rawdisk import SECTOR, Target, human

MIB = 1024 * 1024
ALIGN = 2048  # 1 MiB in sectors
DATA_NAME = "Ventoy"  # Ventoy's boot code expects these two names
EFI_NAME = "VTOYEFI"
MIN_DATA = 256 * MIB


class LayoutError(Exception):
    pass


# ---------------------------------------------------------------------------
# opening a target: USB disk or .img file
# ---------------------------------------------------------------------------
class Disk:
    """A USB disk (by id) or an image file, ready for raw access."""

    def __init__(self, target: str, create_size: Optional[int] = None, allow_system: bool = False,
                 writable: bool = True):
        self.attached: Optional[str] = None  # loop/hdiutil device for an image
        self.info: Optional[hosts.DiskInfo] = None
        looks_image = target.lower().endswith((".img", ".bin", ".raw")) or os.path.isfile(target)
        if looks_image:
            if create_size:
                with open(target, "wb") as f:
                    f.truncate(create_size)
            if not os.path.isfile(target):
                raise LayoutError(f"{target} does not exist (add --size to create a new image)")
            self.is_image = True
            self.name = target
            self.raw = Target(target, os.path.getsize(target), True, writable)
        else:
            info = hosts.find_disk(target)
            if info.system and not allow_system:
                raise LayoutError(f"{info.id} holds your running system - refusing to touch it")
            self.info = info
            self.is_image = False
            self.name = info.id
            if writable and hosts.IS_MAC:
                hosts.release_disk(info)  # macOS refuses raw writes while volumes are mounted
            self.raw = Target(info.path, info.size, False, writable)

    def close(self):
        self.raw.close()
        if self.attached:
            hosts.detach_image(self.attached)
            self.attached = None

    def __enter__(self):
        return self

    def __exit__(self, *e):
        self.close()

    def read_gpt(self) -> G.GPT:
        return G.GPT.read(self.raw, self.raw.sectors)

    # where the data partition is mounted (for copying ISO files)
    def data_folder(self) -> Optional[str]:
        if not self.is_image:
            return hosts.mount_data(self.info, 1)
        self.raw.flush()
        dev = hosts.attach_image(self.name)
        if not dev:
            return None
        self.attached = dev
        fake = hosts.DiskInfo(dev, dev, self.raw.size, "image", True, False)
        try:
            return hosts.mount_data(fake, 1)
        except hosts.HostError:
            return None


# ---------------------------------------------------------------------------
# reading a sycrOS / Ventoy layout
# ---------------------------------------------------------------------------
def check_layout(g: G.GPT) -> Tuple[G.Partition, G.Partition]:
    p1, p2 = g.slots[0], g.slots[1]
    if not p1 or not p2 or p2.name != EFI_NAME or p2.sectors != EFI_PART_SECTORS:
        raise LayoutError("this disk is not a sycrOS/Ventoy GPT disk (run: sycros init)")
    return p1, p2


def mac_slots(g: G.GPT) -> List[Tuple[int, G.Partition]]:
    return [(n, p) for n, p in g.partitions()
            if n >= 3 and p.type_guid in (G.APPLE_HFS, G.APPLE_APFS)]


def mac_free(g: G.GPT) -> int:
    _, p2 = check_layout(g)
    best = 0
    for a, b in g.free_ranges(p2.last + 1):
        first = -(-a // ALIGN) * ALIGN
        best = max(best, (b - first + 1) * SECTOR)
    return best


# ---------------------------------------------------------------------------
# init
# ---------------------------------------------------------------------------
def plan(total_sectors: int, mac_space: int) -> Tuple[int, int, int]:
    """(part1_first, part1_last, part2_first) - same maths as Ventoy2Disk with reserve."""
    mac_sectors = -(-mac_space // MIB) * ALIGN
    reserved = 33 + mac_sectors
    p1_count = total_sectors - reserved - EFI_PART_SECTORS - 2048
    p1_count -= p1_count % 8  # keep partition 2 on a 4 KB boundary
    if p1_count * SECTOR < MIN_DATA:
        raise LayoutError(
            f"disk too small: {human(total_sectors * SECTOR)} total, "
            f"{human(mac_space)} asked for macOS -> less than {human(MIN_DATA)} left for ISO files"
        )
    return 2048, 2048 + p1_count - 1, 2048 + p1_count


def build_mbr(engine: Engine, total_sectors: int) -> bytes:
    mbr = bytearray(engine.boot_img()[:440].ljust(512, b"\0"))
    mbr[92] = 0x22  # GRUB boot.img: core.img starts at sector 34 (0x22)
    mbr[0x180:0x190] = uuid.uuid4().bytes  # Ventoy disk UUID
    mbr[0x1B8:0x1BC] = secrets.token_bytes(4)  # disk signature
    mbr[0x1BC:0x1BE] = b"\0\0"
    mbr[446:462] = G.protective_mbr_entry(total_sectors)
    mbr[462:510] = b"\0" * 48
    mbr[510:512] = b"\x55\xaa"
    return bytes(mbr)


def init(disk: Disk, engine: Engine, mac_space: int, label: str = "sycrOS", log=print) -> G.GPT:
    total = disk.raw.sectors
    p1_first, p1_last, p2_first = plan(total, mac_space)
    p2_last = p2_first + EFI_PART_SECTORS - 1

    log(f"Layout for {disk.name} ({human(disk.raw.size)}):")
    log(f"  1  ISO files (exFAT)  {human((p1_last - p1_first + 1) * SECTOR)}")
    log(f"  2  boot files (EFI)   32.0 MB")
    log(f"  -  macOS space        {human((total - 34 - p2_last) * SECTOR)}")

    if not disk.is_image:
        hosts.release_disk(disk.info, wipe=True)

    raw = disk.raw
    # wipe old partition tables / filesystem signatures at both ends
    raw.zero(0, MIB)
    raw.zero((total - ALIGN) * SECTOR, ALIGN * SECTOR)
    raw.zero(p1_first * SECTOR, MIB)

    raw.write_at(0, build_mbr(engine, total))

    core = engine.core_img()
    raw.write_at(34 * SECTOR, core.ljust(-(-len(core) // SECTOR) * SECTOR, b"\0"))
    raw.write_at(34 * SECTOR + 500, b"\x23")  # first blocklist sector = 35

    g = G.GPT(total)
    g.add(G.Partition(G.MS_BASIC_DATA, uuid.uuid4(), p1_first, p1_last, 0, DATA_NAME), 1)
    g.add(G.Partition(G.MS_BASIC_DATA, uuid.uuid4(), p2_first, p2_last, G.ATTR_NO_DRIVE_LETTER, EFI_NAME), 2)
    g.write(raw)

    log("  writing boot files ...")
    raw.copy_from_file(str(engine.efi_img_path()), 0, EFI_PART_SECTORS * SECTOR, p2_first * SECTOR,
                       "boot files")
    raw.flush()

    log("  formatting ISO partition (exFAT) ...")
    if disk.is_image:
        if hosts.IS_MAC:
            _mac_format_image(disk, engine, label)
        else:
            hosts.format_exfat_image(engine, disk.name, p1_first * SECTOR,
                                     (p1_last - p1_first + 1) * SECTOR, label, raw.size)
    else:
        raw.close()  # let the OS see the new table
        hosts.reread_partitions(disk.info)
        hosts.format_exfat(engine, disk.info, 1, label)
        disk.raw = Target(disk.info.path, disk.info.size, False, writable=False)
    return g


def _mac_format_image(disk: Disk, engine: Engine, label: str):
    disk.raw.flush()
    dev = hosts.attach_image(disk.name)
    if not dev:
        raise LayoutError("could not attach the image with hdiutil")
    try:
        fake = hosts.DiskInfo(dev, "/dev/r" + dev[5:], disk.raw.size, "image", True, False)
        hosts.format_exfat(engine, fake, 1, label)
    finally:
        hosts.detach_image(dev)


# ---------------------------------------------------------------------------
# macOS slots
# ---------------------------------------------------------------------------
def add_mac(disk: Disk, image_path: str, name: Optional[str] = None, workdir: Optional[str] = None,
            log=print) -> int:
    g = disk.read_gpt()
    _, p2 = check_layout(g)

    log(f"Looking inside {os.path.basename(image_path)} ...")
    vol = macimg.find_volume(image_path, workdir)
    try:
        label = (name or vol.label or "macOS")[:36]
        need = -(-vol.length // MIB) * ALIGN  # sectors, whole MiB
        log(f"  found {vol.fs.upper()} volume '{vol.label}' ({vol.kind}), {human(vol.length)}")
        first = g.find_space(need, p2.last + 1, ALIGN)
        if first is None:
            raise LayoutError(
                f"not enough macOS space: need {human(need * SECTOR)}, biggest free gap is "
                f"{human(mac_free(g))}. Remove a slot or re-init with a bigger --mac-space."
            )
        part = G.Partition(vol.gpt_type, uuid.uuid4(), first, first + need - 1, 0, label)

        disk.raw.copy_from_file(vol.source, vol.offset, vol.length, first * SECTOR, "writing macOS")
        tail = need * SECTOR - vol.length
        if tail:
            disk.raw.zero(first * SECTOR + vol.length, tail)
        num = g.add(part)
        g.write(disk.raw)
        disk.raw.flush()
        if not disk.is_image:
            hosts.reread_partitions(disk.info)
        log(f"  macOS slot {num}: '{label}' ready")
        if vol.kind == "recovery":
            log("  note: Recovery images start on Intel Macs only (Apple silicon needs a full installer).")
        return num
    finally:
        vol.cleanup()


def remove_mac(disk: Disk, number: int, log=print) -> G.Partition:
    g = disk.read_gpt()
    check_layout(g)
    if number not in dict(mac_slots(g)):
        raise LayoutError(f"partition {number} is not a macOS slot")
    if not disk.is_image:
        hosts.unmount_slot(disk.info, number)
    p = g.remove(number)
    disk.raw.zero(p.first * SECTOR, min(MIB, p.size))  # so no OS finds the old volume
    g.write(disk.raw)
    disk.raw.flush()
    if not disk.is_image:
        hosts.reread_partitions(disk.info)
    log(f"Removed macOS slot {number} ('{p.name}'), freed {human(p.size)}")
    return p


def refresh_menus(disk: Disk, log=print) -> Optional[str]:
    """Update folders, ventoy.json and the macOS GRUB menu on the data partition."""
    g = disk.read_gpt()
    slots = []
    for n, p in mac_slots(g):
        fs = "apfs" if p.type_guid == G.APPLE_APFS else "hfs"
        slots.append((n, p.name, fs))
    root = disk.data_folder()
    if not root:
        log("  (data partition not mounted - menus will be written on the next 'add' from a USB disk)")
        return None
    library.ensure_layout(root)
    library.write_mac_menu(root, slots)
    return root


# ---------------------------------------------------------------------------
# status
# ---------------------------------------------------------------------------
def status(disk: Disk, log=print) -> None:
    g = disk.read_gpt()
    p1, p2 = check_layout(g)
    log(f"{disk.name}  ({human(disk.raw.size)})  boot engine: Ventoy layout, GPT")
    log(f"  1  ISO files      {human(p1.size):>10}   (Windows / Linux / Tools folders)")
    log(f"  2  boot files     {human(p2.size):>10}")
    for n, p in mac_slots(g):
        fs = "APFS" if p.type_guid == G.APPLE_APFS else "HFS+"
        log(f"  {n}  macOS ({fs})    {human(p.size):>10}   {p.name}")
    log(f"     free for macOS {human(mac_free(g)):>10}")
