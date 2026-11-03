# sycrOS

One USB stick for **Windows, Linux and macOS** installers.

- Copy Windows / Linux ISO files onto the stick, then pick one from a boot menu (like Ventoy).
- Add macOS **.dmg** or **.iso** images (installer or Recovery). Each one gets its own
  section on the stick and shows up in the Mac's own boot menu (hold **Option**).
- The same stick works on PCs, Intel Macs and Apple silicon Macs. Each device shows the
  systems it can start.
- Runs on **Linux, macOS and Windows**. One small program (written in Rust), nothing else to install.

Ideas taken from [Ventoy](https://github.com/ventoy/Ventoy) (multi-boot USB) and
[macos-iso-builder](https://github.com/LongQT-sea/macos-iso-builder) (macOS images without a Mac).

---

## What boots where

| Device | How to open the boot menu | What you see |
|---|---|---|
| PC (BIOS or UEFI) | F12 / F11 / Esc at power-on | sycrOS menu: **Windows**, **Linux**, **Tools** folders |
| Intel Mac | hold **Option (⌥)** at power-on | **EFI Boot** (= sycrOS menu) + every macOS section |
| Intel Mac, inside the sycrOS menu | press **F6** | macOS sections (HFS+ ones) can also be started from here |
| Apple silicon Mac (M1–M4…) | hold the **power button** | macOS **full installer** sections only |

Notes:
- **Recovery** images (small, from mkmaciso "Recovery ISO") start on **Intel Macs only**.
  Apple silicon needs a **full installer** image.
- Windows/Linux ISOs are for Intel/AMD PCs. They do **not** start on Apple silicon Macs.
- Macs with a **T2 chip** (2018–2020): first open *Startup Security Utility* (in Recovery) and
  turn on **Allow booting from external media**. To use the Windows/Linux menu, also set
  Secure Boot to **No Security**.
- PCs cannot start macOS from the stick (Apple only allows that on Macs).

## How the stick is laid out

```
┌──────────┬──────────────────────────────┬────────┬──────────────┬──────────────┬───────┐
│ boot code│ 1  ISO files (exFAT)         │ 2 EFI  │ 3 macOS      │ 4 macOS      │ free  │
│ (BIOS)   │ Windows/ Linux/ Tools/       │ 32 MB  │ Sonoma inst. │ Recovery     │ macOS │
│          │ ventoy/ventoy.json           │ (boot) │ HFS+ / APFS  │ HFS+ / APFS  │ space │
└──────────┴──────────────────────────────┴────────┴──────────────┴──────────────┴───────┘
```

- Partitions 1 and 2 match Ventoy's layout exactly, so Ventoy's proven boot engine
  (downloaded from Ventoy's official release, SHA-256 checked) starts your ISO files.
  This includes BIOS, UEFI and Secure Boot support.
- The **macOS space** is reserved when you prepare the stick (`--mac-space`). Each macOS image
  you add becomes its own partition there. A full installer needs about 14–18 GB,
  Recovery about 1–3 GB.
- macOS sees the ISO partition as a normal exFAT drive, so you can copy ISOs from a Mac too.
  The `._` files macOS leaves behind are hidden from the boot menu.

## Install

Download from the GitHub **Releases** page (or *Actions* → *Build sycros* → *Artifacts*):

| System | File | How to start |
|---|---|---|
| Ubuntu / Debian | `sycros_<version>_amd64.deb` | `sudo apt install ./sycros_*.deb`, then `sudo sycros` (or "sycrOS" in the app list) |
| Any other 64-bit Linux | `sycros-linux-x86_64.tar.gz` | unpack, then `sudo ./sycros` |
| Windows 10/11 | `sycros-windows-x86_64.zip` | unzip, double-click **Start-sycrOS.cmd** (asks for admin) |
| Mac (M1–M4) | `sycros-macos-apple-silicon.zip` | unzip, double-click **Start-sycrOS.command** |
| Intel Mac | `sycros-macos-intel.zip` | same |

The Linux program is self-contained (works on old and new distributions). The Windows
program needs nothing extra installed.

**Mac, first time only:** the program is not signed by Apple, so macOS says it is from an
"unidentified developer". Right-click **Start-sycrOS.command** → **Open** → **Open**.
After that, a normal double-click works. (If macOS still refuses: System Settings →
Privacy & Security → scroll down → **Open Anyway**.)

**Or build it yourself** (needs [Rust](https://rustup.rs)):

```bash
cargo build --release                 # program: target/release/sycros
packaging/linux/build-deb.sh          # Debian/Ubuntu package in dist/
```

The first run downloads the Ventoy 1.1.18 boot files (~20 MB, with `curl`) and checks them.
Offline? Download `ventoy-1.1.18-linux.tar.gz` from Ventoy's release page and run
`sycros engine --tarball <file>`.

## The menu

Run `sycros` with nothing after it and you get a menu (arrow keys + Enter, Esc = back):

```
? What do you want to do? ›
❯ Prepare a USB stick (erases it)
  Add files to a stick (ISO, DMG, ...)
  Show what is on a stick
  Remove a macOS section
  Remove a file
  Check a file (writes nothing)
  Build an image file instead of a stick
  Show all disks
  Quit
```

**Choosing the macOS space:** it is fixed when you prepare the stick. Changing it later means
erasing the stick again. One full macOS installer needs about 18G, two need about 34G, and a
Recovery image about 2G. If you type 0, sycrOS asks you to confirm, because you then cannot
add macOS images to that stick.

When you run it with `sudo` on Linux, the stick is opened for *your* user, so you can also
drag ISO files onto it with the Files app.

It lists only USB/SD sticks to choose from. You can drag files from your file manager into
the terminal window to add them.

## Typed commands (same things, for scripts)

```bash
# 1. find your USB stick
sycros disks

# 2. prepare it (ERASES the stick). Keep 40 GB for macOS images:
sycros init /dev/sdb --mac-space 40G

# 3. add files. ISOs are sorted into Windows/Linux/Tools; macOS images become sections
sycros add /dev/sdb Win11_24H2.iso ubuntu-24.04.iso
sycros add /dev/sdb macOS_Sequoia_15.7.dmg.img  "Recovery_Tahoe.iso"

# 4. check
sycros status /dev/sdb --files

# remove a macOS section (number from status) or a file
sycros remove /dev/sdb --slot 3
sycros remove /dev/sdb --file Linux/ubuntu-24.04.iso
```

Disk names: Linux `/dev/sdb` · macOS `/dev/disk4` · Windows `2` (the disk number from `disks`).

sycrOS only writes to USB/SD disks. It refuses the disk your system runs from, and an internal
disk needs the extra flag `--internal`.

After `init` you can also drag ISO files into the `Windows`, `Linux` or `Tools` folders
yourself. sycrOS only needs to be used for macOS images.

### Build an image file instead of writing a stick

Like mkmaciso's `.dmg.img`, you can build a ready-made image and flash it later with
Rufus, balenaEtcher or `dd`:

```bash
sycros init stick.img --size 64G --mac-space 40G
sycros add  stick.img Win11.iso macOS_Sonoma.dmg.img
```

Works on Linux (and on macOS through `hdiutil`). No root needed on Linux desktops.
Not on Windows yet.

### Check a file first

```bash
sycros inspect macOS_Tahoe.iso
#   macOS image -> macOS slot. HFS volume 'Install macOS Tahoe', 16.8 GB, type: installer
```

## Which macOS files work

| File | From | Result |
|---|---|---|
| `macOS_*.iso` | mkmaciso / macos-iso-builder (ISO) | section (HFS+) |
| `macOS_*.dmg.img` | mkmaciso / macos-iso-builder (DMG) | section |
| `*.dmg` compressed (zlib, bzip2, LZMA, LZFSE, ADC) | Apple, hdiutil | unpacked, then section |
| `BaseSystem.dmg` | Apple Recovery | Recovery section (Intel Macs) |

How it works: sycrOS finds the HFS+ or APFS volume inside the file and copies it
byte for byte into its own partition. The volume keeps Apple's "blessed" boot marks,
so the Mac firmware treats it like a stick made with `createinstallmedia`.

## Safety

- `init` asks you to type `YES` and shows the disk model and size first.
- Disks that hold your running system are refused. Internal disks need `--internal`.
- Removing a macOS section clears its first megabyte so no system finds stale data.

## Limits (v0.1)

- Tested so far: image files on Linux, with checks against the official partition tools
  (`sgdisk -v`) and byte-for-byte checks against Ventoy's own install script.
  Not yet tested: real USB sticks, real Macs, and the Windows/macOS host paths.
- No "upgrade boot engine in place" yet. To move to a newer Ventoy, re-run `init` (this erases the stick).
- 4K-sector "native" disks are not supported (normal USB sticks use 512-byte sectors).

## Tests

```bash
cargo test
```

The tests build small fake macOS volumes (HFS+, APFS, GPT, Apple Partition Map,
compressed DMG with zlib + LZFSE), so no Mac and no root are needed. The full-stick test
builds a real image and checks it with `sgdisk` and against Ventoy's own files, byte for byte.

The first Python version is kept in `python-old/` for reference. It can be deleted.

## Credits / license

- Boot engine: [Ventoy](https://www.ventoy.net) by longpanda, GPLv3. sycrOS downloads the
  official release at run time and does not change its boot files.
- Idea for macOS images: [macos-iso-builder / mkmaciso](https://github.com/LongQT-sea/macos-iso-builder) by LongQT-sea.
