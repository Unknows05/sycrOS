"""Everything that differs between Linux, macOS and Windows.

Listing disks, safety checks, unmounting, re-reading the partition table,
formatting the exFAT data partition and finding where it is mounted.
"""
from __future__ import annotations

import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import List, Optional

IS_WIN = sys.platform == "win32"
IS_MAC = sys.platform == "darwin"
IS_LINUX = sys.platform.startswith("linux")


class HostError(Exception):
    pass


@dataclass
class DiskInfo:
    id: str  # what the user types: /dev/sdb, /dev/disk4, 2 (Windows disk number)
    path: str  # what we open for raw access
    size: int
    model: str
    removable: bool
    system: bool  # holds the running OS -> never touch


def run(cmd, check=True, capture=True, input=None) -> str:
    r = subprocess.run(cmd, capture_output=capture, text=True, input=input)
    if check and r.returncode != 0:
        raise HostError(f"command failed: {' '.join(cmd)}\n{(r.stderr or r.stdout or '').strip()}")
    return r.stdout if capture else ""


def ps(script: str, check=True) -> str:
    """Run a PowerShell snippet (Windows)."""
    return run(["powershell", "-NoProfile", "-NonInteractive", "-Command", script], check=check)


def is_admin() -> bool:
    if IS_WIN:
        try:
            import ctypes
            return bool(ctypes.windll.shell32.IsUserAnAdmin())
        except Exception:
            return False
    return os.geteuid() == 0


# ---------------------------------------------------------------------------
# listing
# ---------------------------------------------------------------------------
def list_disks() -> List[DiskInfo]:
    if IS_LINUX:
        return _linux_disks()
    if IS_MAC:
        return _mac_disks()
    if IS_WIN:
        return _win_disks()
    raise HostError(f"unsupported system: {sys.platform}")


def _linux_disks():
    out = json.loads(run(["lsblk", "-J", "-b", "-o", "NAME,PATH,SIZE,MODEL,RM,HOTPLUG,TYPE,MOUNTPOINTS,TRAN"]))
    disks = []
    for d in out.get("blockdevices", []):
        if d.get("type") != "disk":
            continue
        mounts = []

        def walk(n):
            mounts.extend(m for m in (n.get("mountpoints") or []) if m)
            for c in n.get("children") or []:
                walk(c)

        walk(d)
        system = any(m in ("/", "/boot", "/boot/efi", "/usr", "/home") or m.startswith("[SWAP")
                     for m in mounts)
        removable = bool(d.get("rm")) or bool(d.get("hotplug")) or d.get("tran") == "usb"
        disks.append(DiskInfo(d["path"], d["path"], int(d["size"]), (d.get("model") or "").strip(),
                              removable, system))
    return disks


def _mac_disks():
    pl = plistlib.loads(run(["diskutil", "list", "-plist", "physical"]).encode())
    disks = []
    for name in pl.get("WholeDisks", []):
        info = plistlib.loads(run(["diskutil", "info", "-plist", name]).encode())
        internal = info.get("Internal", True)
        system = internal and not info.get("RemovableMediaOrExternalDevice", False)
        disks.append(DiskInfo(f"/dev/{name}", f"/dev/r{name}", int(info.get("TotalSize", 0)),
                              info.get("MediaName", ""), not internal, system))
    return disks


def _win_disks():
    raw = ps("Get-Disk | Select-Object Number,Size,FriendlyName,BusType,IsBoot,IsSystem | ConvertTo-Json")
    items = json.loads(raw or "[]")
    if isinstance(items, dict):
        items = [items]
    disks = []
    for d in items:
        n = str(d["Number"])
        disks.append(DiskInfo(n, rf"\\.\PhysicalDrive{n}", int(d["Size"]), d.get("FriendlyName") or "",
                              d.get("BusType") in ("USB", "SD", "MMC"),
                              bool(d.get("IsBoot") or d.get("IsSystem"))))
    return disks


def find_disk(user_id: str) -> DiskInfo:
    norm = user_id.strip()
    if IS_WIN:
        norm = re.sub(r"(?i)^(\\\\\.\\)?physicaldrive", "", norm)
    if IS_MAC:
        norm = norm.replace("/dev/rdisk", "/dev/disk")
        if not norm.startswith("/dev/"):
            norm = "/dev/" + norm
    for d in list_disks():
        if d.id == norm:
            return d
    raise HostError(f"disk '{user_id}' not found. Run: sycros disks")


# ---------------------------------------------------------------------------
# partition names
# ---------------------------------------------------------------------------
def part_path(disk: DiskInfo, n: int) -> str:
    if IS_LINUX:
        base = disk.id
        return f"{base}p{n}" if base[-1].isdigit() else f"{base}{n}"
    if IS_MAC:
        return f"{disk.id}s{n}"
    return f"{disk.id}:{n}"  # Windows: disk number + partition number


# ---------------------------------------------------------------------------
# before / after raw writes
# ---------------------------------------------------------------------------
def release_disk(disk: DiskInfo, wipe: bool = False) -> None:
    """Unmount everything on the disk so we can write raw bytes to it."""
    if IS_LINUX:
        out = json.loads(run(["lsblk", "-J", "-o", "PATH,MOUNTPOINTS", disk.id]))

        def walk(n):
            if any(n.get("mountpoints") or []):
                run(["umount", n["path"]], check=False)
            for c in n.get("children") or []:
                walk(c)

        for d in out.get("blockdevices", []):
            walk(d)
    elif IS_MAC:
        run(["diskutil", "unmountDisk", "force", disk.id])
    elif IS_WIN and wipe:
        ps(f"Clear-Disk -Number {disk.id} -RemoveData -RemoveOEM -Confirm:$false -ErrorAction SilentlyContinue; "
           f"Set-Disk -Number {disk.id} -IsOffline $false -ErrorAction SilentlyContinue; "
           f"Set-Disk -Number {disk.id} -IsReadOnly $false", check=False)


def reread_partitions(disk: DiskInfo) -> None:
    """Tell the OS the partition table changed."""
    if IS_LINUX:
        for cmd in (["blockdev", "--rereadpt", disk.id], ["partprobe", disk.id], ["partx", "-u", disk.id]):
            if shutil.which(cmd[0]) and subprocess.run(cmd, capture_output=True).returncode == 0:
                break
        subprocess.run(["udevadm", "settle"], capture_output=True)
    elif IS_MAC:
        time.sleep(2)  # macOS re-reads by itself once the raw device is closed
    elif IS_WIN:
        ps(f"Update-HostStorageCache; Update-Disk -Number {disk.id}", check=False)
        time.sleep(2)


def _wait_for(path: str, seconds: int = 15) -> None:
    for _ in range(seconds * 2):
        if os.path.exists(path):
            return
        time.sleep(0.5)
    raise HostError(f"partition {path} did not show up")


# ---------------------------------------------------------------------------
# exFAT
# ---------------------------------------------------------------------------
def exfat_cluster_sectors(disk_size: int) -> int:
    # same rule as Ventoy: 128 KB clusters above 32 GB, 32 KB below
    return 256 if disk_size > 32 * 1024 ** 3 else 64


def _linux_mkfs(engine, target: str, label: str, cluster_sectors: int) -> None:
    sysmk = shutil.which("mkfs.exfat")
    if sysmk:
        r = subprocess.run([sysmk, "-n", label, "-c", str(cluster_sectors * 512), target], capture_output=True)
        if r.returncode == 0:
            return
    mk = engine.mkexfatfs() if engine else None
    if not mk:
        raise HostError("no exFAT formatter found. Install exfatprogs (mkfs.exfat).")
    run([str(mk), "-n", label, "-s", str(cluster_sectors), target])


def format_exfat(engine, disk: DiskInfo, n: int, label: str) -> None:
    """Format partition n of a real disk as exFAT."""
    cs = exfat_cluster_sectors(disk.size)
    if IS_LINUX:
        p = part_path(disk, n)
        _wait_for(p)
        _linux_mkfs(engine, p, label, cs)
    elif IS_MAC:
        run(["diskutil", "eraseVolume", "ExFAT", label, part_path(disk, n)])
    elif IS_WIN:
        ps(f"$p = Get-Partition -DiskNumber {disk.id} -PartitionNumber {n}; "
           f"$p | Format-Volume -FileSystem exFAT -NewFileSystemLabel '{label}' "
           f"-AllocationUnitSize {cs * 512} -Force -Confirm:$false | Out-Null; "
           f"if (-not ($p | Get-Partition).DriveLetter) {{ $p | Add-PartitionAccessPath -AssignDriveLetter }}")


def format_exfat_image(engine, image: str, offset: int, length: int, label: str, disk_size: int) -> None:
    """Format an exFAT partition that lives inside an image file."""
    if not IS_LINUX:
        raise HostError("building .img files is supported on Linux. On macOS/Windows, write to a USB disk directly.")
    fd, tmp = tempfile.mkstemp(prefix="sycros-exfat-", dir=os.path.dirname(os.path.abspath(image)))
    os.close(fd)
    try:
        with open(tmp, "wb") as f:
            f.truncate(length)
        _linux_mkfs(engine, tmp, label, exfat_cluster_sectors(disk_size))
        # copy only the non-empty parts back (keeps the image sparse)
        with open(tmp, "rb") as src, open(image, "r+b") as dst:
            pos = 0
            step = 1 << 20
            while pos < length:
                blk = src.read(step)
                if not blk:
                    break
                if blk.count(0) != len(blk):
                    dst.seek(offset + pos)
                    dst.write(blk)
                pos += len(blk)
    finally:
        os.remove(tmp)


# ---------------------------------------------------------------------------
# mounting the data partition (to copy ISO files)
# ---------------------------------------------------------------------------
def mount_data(disk: DiskInfo, n: int = 1) -> str:
    """Return a folder where partition n is mounted, mounting it if needed."""
    if IS_LINUX:
        p = part_path(disk, n)
        _wait_for(p)
        mp = run(["lsblk", "-no", "MOUNTPOINT", p]).strip().splitlines()
        if mp and mp[0]:
            return mp[0]
        if shutil.which("udisksctl"):
            out = run(["udisksctl", "mount", "--no-user-interaction", "-b", p], check=False)
            m = re.search(r" at (.+?)\.?$", out.strip())
            if m:
                return m.group(1)
        mnt = tempfile.mkdtemp(prefix="sycros-mnt-")
        run(["mount", p, mnt])
        return mnt
    if IS_MAC:
        p = part_path(disk, n)
        run(["diskutil", "mount", p], check=False)
        info = plistlib.loads(run(["diskutil", "info", "-plist", p]).encode())
        mp = info.get("MountPoint")
        if not mp:
            raise HostError(f"could not mount {p}")
        return mp
    if IS_WIN:
        letter = ps(f"$p = Get-Partition -DiskNumber {disk.id} -PartitionNumber {n}; "
                    f"if (-not $p.DriveLetter) {{ $p | Add-PartitionAccessPath -AssignDriveLetter; "
                    f"$p = Get-Partition -DiskNumber {disk.id} -PartitionNumber {n} }}; $p.DriveLetter").strip()
        if not letter:
            raise HostError("could not give the data partition a drive letter")
        return f"{letter}:\\"
    raise HostError("unsupported system")


def unmount_slot(disk: DiskInfo, n: int) -> None:
    """macOS auto-mounts HFS+/APFS slots; unmount one before overwriting it."""
    if IS_MAC:
        run(["diskutil", "unmount", "force", part_path(disk, n)], check=False)
    elif IS_LINUX:
        p = part_path(disk, n)
        if os.path.exists(p):
            run(["umount", p], check=False)


# ---------------------------------------------------------------------------
# image files on Linux: attach as loop device without root (udisks)
# ---------------------------------------------------------------------------
def attach_image(image: str) -> Optional[str]:
    if IS_LINUX and shutil.which("udisksctl"):
        out = run(["udisksctl", "loop-setup", "--no-user-interaction", "-f", image], check=False)
        m = re.search(r"(/dev/loop\d+)", out)
        return m.group(1) if m else None
    if IS_MAC:
        out = run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", image])
        m = re.search(r"(/dev/disk\d+)\s", out)
        return m.group(1) if m else None
    return None


def detach_image(dev: str) -> None:
    if IS_LINUX:
        # The desktop may auto-mount partitions a moment after attach, so wait,
        # then unmount until nothing is left. loop-setup sets "autoclear": the
        # loop device goes away by itself once no partition is mounted
        # (loop-delete itself would need a password).
        subprocess.run(["sync"], capture_output=True)
        subprocess.run(["udevadm", "settle", "--timeout=5"], capture_output=True)
        clean = 0
        for _ in range(15):
            out = json.loads(run(["lsblk", "-J", "-o", "PATH,MOUNTPOINTS", dev], check=False) or "{}")
            if not out.get("blockdevices"):
                break  # loop device already gone
            nodes = []  # lsblk lists loop partitions flat, real disks nested: take both

            def walk(n):
                nodes.append(n)
                for c in n.get("children") or []:
                    walk(c)

            for n in out["blockdevices"]:
                walk(n)
            mounted = [n["path"] for n in nodes if n["path"] != dev and any(n.get("mountpoints") or [])]
            for path in mounted:
                run(["udisksctl", "unmount", "--no-user-interaction", "-b", path], check=False)
            clean = 0 if mounted else clean + 1
            if clean >= 3:
                break
            time.sleep(1)
        run(["udisksctl", "loop-delete", "--no-user-interaction", "-b", dev], check=False)
    elif IS_MAC:
        run(["hdiutil", "detach", dev], check=False)
