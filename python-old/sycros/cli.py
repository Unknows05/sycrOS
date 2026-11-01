"""sycros command line."""
from __future__ import annotations

import argparse
import os
import re
import sys

from . import __version__, engine as eng, hosts, layout, library, macimg
from .rawdisk import human

USAGE_EXAMPLES = """
examples:
  sycros disks                                  show USB disks
  sycros init /dev/sdb --mac-space 32G          prepare a USB stick (ERASES IT)
  sycros add /dev/sdb Win11.iso ubuntu.iso      copy ISOs (sorted into Windows/Linux/Tools)
  sycros add /dev/sdb macOS_Sonoma.dmg.img      write a macOS slot (Mac boot menu)
  sycros status /dev/sdb
  sycros remove /dev/sdb --slot 3               delete a macOS slot
  sycros inspect Tahoe.iso                      what sycrOS sees inside a file
  sycros init stick.img --size 16G --mac-space 8G   build an image to flash later

disk names:  Linux /dev/sdb   macOS /dev/disk4   Windows 2 (disk number)
"""


def parse_size(text: str) -> int:
    m = re.fullmatch(r"\s*(\d+(?:\.\d+)?)\s*([KMGT]?)(I?B)?\s*", text.upper())
    if not m:
        raise argparse.ArgumentTypeError(f"bad size: {text} (use e.g. 32G or 500M)")
    mult = {"": 1024 ** 3, "K": 1024, "M": 1024 ** 2, "G": 1024 ** 3, "T": 1024 ** 4}[m.group(2)]
    return int(float(m.group(1)) * mult)


def need_admin(disk_arg: str) -> None:
    if os.path.isfile(disk_arg) or disk_arg.lower().endswith((".img", ".bin", ".raw")):
        return
    if not hosts.is_admin():
        how = "run the terminal as Administrator" if hosts.IS_WIN else "use sudo"
        sys.exit(f"Writing to a disk needs admin rights - {how}.")


def confirm(text: str, yes: bool) -> None:
    if yes:
        return
    print(text)
    if input("Type YES to continue: ").strip() != "YES":
        sys.exit("Cancelled.")


# ---------------------------------------------------------------------------
def cmd_disks(a):
    disks = hosts.list_disks()
    print(f"{'DISK':<16}{'SIZE':>10}  {'TYPE':<10} MODEL")
    for d in disks:
        kind = "SYSTEM" if d.system else ("usb/sd" if d.removable else "internal")
        print(f"{d.id:<16}{human(d.size):>10}  {kind:<10} {d.model}")
    print("\nOnly use a usb/sd disk. sycrOS refuses SYSTEM disks.")


def cmd_engine(a):
    e = eng.get_engine(refresh=a.refresh, tarball=a.tarball)
    print(f"Ventoy {e.version} boot files: {e.folder}")


def cmd_init(a):
    if not 1 <= len(a.label) <= 11:
        sys.exit("--label must be 1 to 11 characters (exFAT limit)")
    need_admin(a.disk)
    e = eng.get_engine()
    is_new_image = a.size is not None
    if is_new_image and os.path.exists(a.disk) and not a.yes:
        confirm(f"{a.disk} exists and will be replaced.", a.yes)
    with layout.Disk(a.disk, create_size=a.size) as d:
        if not d.is_image:
            confirm(f"\nALL DATA on {d.name} ({human(d.raw.size)}, {d.info.model}) will be ERASED.", a.yes)
        layout.init(d, e, a.mac_space, a.label)
        root = layout.refresh_menus(d)
        print(f"\nDone. {d.name} is a sycrOS disk.")
        if root:
            print(f"ISO files go in: {root}  (folders Windows / Linux / Tools)")
        print("Next: sycros add <disk> <file.iso|file.dmg> ...")


def cmd_add(a):
    need_admin(a.disk)
    with layout.Disk(a.disk) as d:
        g = d.read_gpt()
        layout.check_layout(g)
        isos = []
        changed_mac = False
        for f in a.files:
            if not os.path.isfile(f):
                sys.exit(f"{f}: file not found")
            kind = a.os or ("mac" if macimg.looks_like_mac(f) else library.classify(f))
            if kind == "mac":
                layout.add_mac(d, f, a.name, a.workdir)
                changed_mac = True
            else:
                isos.append((f, kind))
        if isos or changed_mac:
            root = layout.refresh_menus(d)
            if isos and not root:
                sys.exit("Could not mount the ISO partition. Flash/plug the disk and run 'add' again, "
                         "or copy the files into the Windows/Linux/Tools folders yourself.")
            for f, kind in isos:
                dest = library.copy_file(f, root, kind)
                print(f"  {os.path.basename(f)} -> {library.FOLDERS[kind]}/  ({human(os.path.getsize(dest))})")
        print("Done.")


def cmd_remove(a):
    need_admin(a.disk)
    with layout.Disk(a.disk) as d:
        if a.slot:
            layout.remove_mac(d, a.slot)
            layout.refresh_menus(d)
        if a.file:
            root = d.data_folder()
            if not root:
                sys.exit("Could not mount the ISO partition.")
            p = os.path.join(root, a.file)
            if not os.path.isfile(p):
                sys.exit(f"{a.file} not found on the disk (see: sycros status)")
            os.remove(p)
            print(f"Removed {a.file}")


def cmd_status(a):
    with layout.Disk(a.disk, writable=False) as d:
        layout.status(d)
        if a.files:
            root = d.data_folder()
            if root:
                print("\nfiles:")
                for folder, fn, size in library.list_files(root):
                    print(f"  {folder:<8} {fn}  ({human(size)})")


def cmd_inspect(a):
    for f in a.files:
        print(f"{f}:")
        if macimg.looks_like_mac(f):
            v = macimg.find_volume(f, a.workdir)
            try:
                print(f"  macOS image -> macOS slot. {v.fs.upper()} volume '{v.label}', {human(v.length)}, "
                      f"type: {v.kind}")
                print(f"  boots on: Intel Macs" + ("" if v.kind == "recovery" else
                                                   " + Apple silicon (if it is a full installer)"))
            finally:
                v.cleanup()
        else:
            k = library.classify(f)
            lbl = library.iso_label(f)
            print(f"  {k} image -> {library.FOLDERS[k]}/ folder" + (f"  (label: {lbl})" if lbl else ""))


# ---------------------------------------------------------------------------
def main(argv=None):
    p = argparse.ArgumentParser(
        prog="sycros",
        description="sycrOS - one USB for Windows, Linux and macOS installers.",
        epilog=USAGE_EXAMPLES, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--version", action="version", version=f"sycrOS {__version__} (Ventoy {eng.VENTOY_VERSION})")
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("disks", help="list disks")
    s.set_defaults(fn=cmd_disks)

    s = sub.add_parser("engine", help="download / check the Ventoy boot files")
    s.add_argument("--refresh", action="store_true", help="download again")
    s.add_argument("--tarball", help="use a local ventoy-*-linux.tar.gz (offline)")
    s.set_defaults(fn=cmd_engine)

    s = sub.add_parser("init", help="prepare a disk or image (ERASES it)")
    s.add_argument("disk")
    s.add_argument("--mac-space", type=parse_size, default=0,
                   help="space kept for macOS slots, e.g. 32G (default 0 = no macOS)")
    s.add_argument("--size", type=parse_size, help="create a new image file of this size")
    s.add_argument("--label", default="sycrOS", help="name of the ISO partition (default sycrOS)")
    s.add_argument("--yes", action="store_true", help="do not ask for confirmation")
    s.set_defaults(fn=cmd_init)

    s = sub.add_parser("add", help="add ISO/WIM/IMG/VHD files or macOS DMG/ISO images")
    s.add_argument("disk")
    s.add_argument("files", nargs="+")
    s.add_argument("--os", choices=["windows", "linux", "tools", "mac"], help="force the OS folder / mac slot")
    s.add_argument("--name", help="name for the macOS slot (default: volume name)")
    s.add_argument("--workdir", help="folder for temporary files when unpacking .dmg")
    s.set_defaults(fn=cmd_add)

    s = sub.add_parser("remove", help="remove a macOS slot or a file")
    s.add_argument("disk")
    s.add_argument("--slot", type=int, help="macOS slot partition number (see status)")
    s.add_argument("--file", help="file path on the disk, e.g. Linux/ubuntu.iso")
    s.set_defaults(fn=cmd_remove)

    s = sub.add_parser("status", help="show what is on a sycrOS disk")
    s.add_argument("disk")
    s.add_argument("--files", action="store_true", help="also list ISO files (mounts the disk)")
    s.set_defaults(fn=cmd_status)

    s = sub.add_parser("inspect", help="show how sycrOS would use a file")
    s.add_argument("files", nargs="+")
    s.add_argument("--workdir")
    s.set_defaults(fn=cmd_inspect)

    a = p.parse_args(argv)
    try:
        a.fn(a)
    except (layout.LayoutError, hosts.HostError, ValueError, RuntimeError, OSError) as e:
        sys.exit(f"error: {e}")
    except KeyboardInterrupt:
        sys.exit("\nStopped.")


if __name__ == "__main__":
    main()
