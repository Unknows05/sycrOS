"""The data partition: OS folders, Ventoy menu settings and the Mac menu.

Layout on the data (exFAT) partition:

    /Windows/         Windows installers, WinPE, .wim/.vhd(x)
    /Linux/           Linux ISOs
    /Tools/           everything else (rescue, memtest, ...)
    /ventoy/ventoy.json       menu settings (tree view = one menu per OS folder)
    /ventoy/ventoy_grub.cfg   extra menu (F6): macOS slots for Intel Macs
"""
from __future__ import annotations

import json
import os
import re
import time

from .rawdisk import progress, progress_end

FOLDERS = {"windows": "Windows", "linux": "Linux", "tools": "Tools"}

BOOTABLE_EXT = (".iso", ".wim", ".img", ".vhd", ".vhdx", ".efi", ".vtoy")

_WIN = re.compile(r"(^|[^a-z])(win(dows)?|win1[01]|win[78]|server20\d\d|winpe|hiren|cc[cs][ao]|"
                  r"[a-z0-9]*x64fre|x86fre|esd-iso|tiny1[01]|atlas|revios)", re.I)
_LINUX = re.compile(
    r"ubuntu|debian|fedora|arch|mint|kali|manjaro|suse|centos|rocky|alma|rhel|pop[-_]?os|elementary|"
    r"zorin|endeavour|cachy|nixos|gentoo|void|alpine|mx[-_]|tails|proxmox|linux|garuda|bazzite|slackware|"
    r"steamos|parrot|deepin|solus|nobara|kubuntu|xubuntu|lubuntu|bluefin|vanilla|clonezilla|gparted|"
    r"systemrescue|freebsd|truenas|opnsense|pfsense|chromeos|fydeos", re.I)

MAC_BEGIN = "# >>> sycrOS macOS entries (generated, do not edit between these lines)"
MAC_END = "# <<< sycrOS macOS entries"

DEFAULT_CONTROL = [
    {"VTOY_DEFAULT_MENU_MODE": "1"},  # tree view: Windows / Linux / Tools menus
    {"VTOY_FILT_DOT_UNDERSCORE_FILE": "1"},  # hide macOS "._" junk files
]


def iso_label(path: str) -> str:
    """Volume label of an ISO 9660 image ('' if none)."""
    try:
        with open(path, "rb") as f:
            f.seek(16 * 2048)
            pvd = f.read(2048)
        if pvd[1:6] == b"CD001":
            return pvd[40:72].decode("ascii", "replace").strip()
    except OSError:
        pass
    return ""


def classify(path: str) -> str:
    """windows / linux / tools for a bootable file."""
    low = path.lower()
    if low.endswith((".wim", ".vhd", ".vhdx")):
        return "windows"
    if low.endswith(".efi"):
        return "tools"
    text = f"{os.path.basename(path)} {iso_label(path)}"
    if _WIN.search(text):
        return "windows"
    if _LINUX.search(text):
        return "linux"
    return "tools"


def ensure_layout(root: str) -> None:
    for name in FOLDERS.values():
        os.makedirs(os.path.join(root, name), exist_ok=True)
    os.makedirs(os.path.join(root, "ventoy"), exist_ok=True)
    cfg = os.path.join(root, "ventoy", "ventoy.json")
    data = {}
    if os.path.exists(cfg):
        try:
            with open(cfg, encoding="utf-8") as f:
                data = json.load(f)
        except (OSError, ValueError):
            print(f"  note: {cfg} is not valid JSON, leaving it alone")
            return
    control = data.setdefault("control", [])
    have = {k for item in control for k in item}
    changed = not os.path.exists(cfg)
    for item in DEFAULT_CONTROL:
        if not set(item) & have:
            control.append(item)
            changed = True
    if changed:
        with open(cfg, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=4)
            f.write("\n")


def copy_file(src: str, root: str, os_kind: str) -> str:
    folder = os.path.join(root, FOLDERS[os_kind])
    os.makedirs(folder, exist_ok=True)
    dest = os.path.join(folder, os.path.basename(src))
    size = os.path.getsize(src)
    t0 = time.time()
    done = 0
    tmp = dest + ".sycros-part"
    with open(src, "rb") as fi, open(tmp, "wb") as fo:
        while True:
            buf = fi.read(8 << 20)
            if not buf:
                break
            fo.write(buf)
            done += len(buf)
            progress(f"copying to {FOLDERS[os_kind]}/", done, size, t0)
        fo.flush()
        os.fsync(fo.fileno())
    progress_end()
    os.replace(tmp, dest)
    return dest


def list_files(root: str):
    out = []
    for kind, name in FOLDERS.items():
        folder = os.path.join(root, name)
        if os.path.isdir(folder):
            for fn in sorted(os.listdir(folder)):
                if fn.lower().endswith(BOOTABLE_EXT) and not fn.startswith("._"):
                    out.append((name, fn, os.path.getsize(os.path.join(folder, fn))))
    for fn in sorted(os.listdir(root)):
        p = os.path.join(root, fn)
        if os.path.isfile(p) and fn.lower().endswith(BOOTABLE_EXT) and not fn.startswith("._"):
            out.append(("/", fn, os.path.getsize(p)))
    return out


def write_mac_menu(root: str, slots) -> None:
    """Rewrite the macOS part of ventoy_grub.cfg.

    `slots` = list of (partition_number, label, fs). Only HFS+ volumes can be
    started from GRUB (it cannot read APFS); APFS slots are listed as a hint.
    """
    path = os.path.join(root, "ventoy", "ventoy_grub.cfg")
    old = ""
    if os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            old = f.read()
    keep = re.sub(re.escape(MAC_BEGIN) + r".*?" + re.escape(MAC_END) + r"\n?", "", old, flags=re.S)

    lines = [MAC_BEGIN]
    for num, label, fs in slots:
        safe = label.replace('"', "'")
        if fs == "hfs":
            lines += [
                f'menuentry "macOS: {safe}  (Intel Mac only)" --class=macOS {{',
                "    insmod part_gpt",
                "    insmod hfsplus",
                "    insmod chain",
                f"    set root=(${{vtoydev}},gpt{num})",
                "    chainloader /System/Library/CoreServices/boot.efi",
                "}",
            ]
        else:
            lines += [
                f'menuentry "macOS: {safe}  (APFS - use the Mac boot menu: hold Option)" --class=macOS {{',
                '    echo "This macOS volume is APFS. Restart and hold the Option key to start it."',
                "    sleep 5",
                "}",
            ]
    lines.append(MAC_END)
    block = "\n".join(lines) + "\n"

    if not slots and not keep.strip():
        if os.path.exists(path):
            os.remove(path)
        return
    if "VTOY_RET" not in keep:
        keep += ("menuentry '<-- Return to previous menu [Esc]' --class=vtoyret VTOY_RET {\n"
                 "    echo 'Return ...'\n}\n")
    with open(path, "w", encoding="utf-8") as f:
        f.write((block if slots else "") + keep)
