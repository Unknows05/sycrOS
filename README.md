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
- Partition 2 is named `sycrOS-boot`. On Linux and Mac you may still see a drive labelled
  **VTOYEFI**: that is its label, which Ventoy's boot program requires, or it refuses to start.
  Do not change or format it, and do not put files on it.
- macOS sees the ISO partition as a normal exFAT drive, so you can copy ISOs from a Mac too.
  The `._` files macOS leaves behind are hidden from the boot menu.

## Install

### Easiest: one line

**Linux or Mac** (open a terminal):
```bash
curl -fsSL https://raw.githubusercontent.com/Unknows05/sycrOS/main/install.sh | sh
sudo sycros
```

**Windows 10/11** (open PowerShell):
```powershell
irm https://raw.githubusercontent.com/Unknows05/sycrOS/main/install.ps1 | iex
```
Then start **sycrOS** from the Start menu (it asks for admin rights).

### Or download a file yourself

All files: **https://github.com/Unknows05/sycrOS/releases/latest**

| System | Download | How to start |
|---|---|---|
| Ubuntu / Debian / Mint | [sycros-linux-amd64.deb](https://github.com/Unknows05/sycrOS/releases/latest/download/sycros-linux-amd64.deb) | see below |
| Any other 64-bit Linux | [sycros-linux-amd64.tar.gz](https://github.com/Unknows05/sycrOS/releases/latest/download/sycros-linux-amd64.tar.gz) | unpack, then `sudo ./sycros/sycros` |
| Windows 10/11 | [sycros-windows-x64.zip](https://github.com/Unknows05/sycrOS/releases/latest/download/sycros-windows-x64.zip) | unzip, double-click **Start-sycrOS.cmd** (asks for admin) |
| Mac with Apple chip (M1–M4) | [sycros-macos-apple-silicon.zip](https://github.com/Unknows05/sycrOS/releases/latest/download/sycros-macos-apple-silicon.zip) | unzip, double-click **Start-sycrOS.command** |
| Mac with Intel chip | [sycros-macos-intel.zip](https://github.com/Unknows05/sycrOS/releases/latest/download/sycros-macos-intel.zip) | same |

Ubuntu / Debian / Mint from the terminal:
```bash
cd /tmp
wget https://github.com/Unknows05/sycrOS/releases/latest/download/sycros-linux-amd64.deb
sudo apt install ./sycros-linux-amd64.deb
sudo sycros
```

The Linux program is self-contained (works on old and new distributions). The Windows
program needs nothing extra installed.

**Windows step by step:**
1. Open **PowerShell** and run the install line above.
2. Open **sycrOS** from the Start menu and click **Yes** on the admin prompt.
3. **Prepare a USB stick (erases it)** → choose the stick → press Enter for 34G → type what it asks, such as `ERASE 2`.
4. **Add files to a stick** → choose the stick → drag your files into the window → Enter.
5. **Safely remove a stick**, then unplug it.

Windows cannot read Mac sections, so they do not appear in File Explorer, and it may offer to
format them. **Always click Cancel**: formatting destroys that section. Only the `sycrOS` part
appears as a normal drive. Building image files is not available on Windows.

**Mac, first time only:** the program is not signed by Apple, so macOS says it is from an
"unidentified developer". Right-click **Start-sycrOS.command** → **Open** → **Open**.
After that, a normal double-click works. (If macOS still refuses: System Settings →
Privacy & Security → scroll down → **Open Anyway**.) The one-line installer avoids this.

**Update:** when the app opens, it asks GitHub once whether a newer version exists, in the
background. If there is one, the app says so and adds **Update sycrOS** to the menu. Choose it to
download the new version, check its fingerprint and replace the program. Or run `sycros update`.
sycrOS never updates by itself, and your sticks are not touched.

- This check sends a normal web request to github.com and nothing else. If it fails, you see nothing.
- To turn it off, set `SYCROS_NO_UPDATE_CHECK=1`.
- Updating works from v0.3.0 on. For older versions, run the install line again.

### Or build it yourself

Needs [Rust](https://rustup.rs):
```bash
git clone https://github.com/Unknows05/sycrOS && cd sycrOS
cargo build --release                 # program: target/release/sycros
packaging/linux/build-deb.sh          # Debian/Ubuntu package: dist/sycros-linux-amd64.deb
```

The first run downloads the Ventoy 1.1.18 boot files (~20 MB, with `curl`) and checks them.
Offline? Download `ventoy-1.1.18-linux.tar.gz` from Ventoy's release page and run
`sycros engine --tarball <file>`.

## The app

Run `sycros` with nothing after it. It opens one centred panel in your terminal (arrow keys +
Enter, Esc = back):

```
                    ╭──────────────── sycrOS 0.3.0 ────────────────╮
                    │        SanDisk 3.2Gen1  57.3 GB  sdb         │
                    │   ██████████████████████████░░░░░░░░░░░░░░   │
                    │   ■ sycrOS 23.3 GB  ■ boot 32.0 MB           │
                    │   ■ Sequoia 16.1 GB ■ Mojave 5.7 GB          │
                    │                                              │
                    │      › Prepare a USB stick (erases it)       │
                    │        Add files to a stick (ISO, DMG, ...)  │
                    │        ...                                   │
                    ╰─────── ↑↓ choose   Enter select   q quit ────╯
```

- It stays centred when you resize the window.
- It shows a picture of your stick, and notices sticks being plugged in and out.
- Long jobs show a progress bar and the time left.
- It follows your terminal's colours, so light and dark themes both work.
- It adds about 0.5 MB. It redraws only when something changes, and the spinner runs only while
  working. (It does look for sticks every 2 seconds, or 5 on Windows.)

In a very small or basic terminal, or with `sycros --simple`, you get the plain menu instead.

**Choosing the macOS space:** it is fixed when you prepare the stick. Changing it later means
erasing the stick again. One full macOS installer needs about 18G, two need about 34G, and a
Recovery image about 2G. If you type 0, sycrOS asks you to confirm, because you then cannot
add macOS images to that stick.

When you run it with `sudo` on Linux, the stick is opened for *your* user, so you can also
drag ISO files onto it with the Files app.

You can drag files from your file manager into
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

- Erasing asks you to type the exact disk name, such as `ERASE sdb`. It shows the model, size
  and what is on the disk first.
- USB SSDs look just like USB sticks to a computer. So sycrOS also looks at what is on a disk:
  one that seems to hold Windows, Linux or macOS is not offered in the app, and in typed
  commands it needs `--internal`. This is a best guess from the partitions, so always check
  the model and size yourself.
- `sycros init <file>` asks before overwriting an existing file.
- Disks that hold your running system are refused. Internal disks need `--internal`.
- Everything written is read back and compared with the original. A macOS section or ISO only
  appears on the stick after this check passes, so adding files takes about twice as long.
- **Safely remove a stick** in the menu (or `sycros eject <disk>`) finishes all writing, so the
  stick can be unplugged without damaging files.
- Removing a macOS section clears its first megabyte so no system finds stale data.

## Limits

- Tested so far:
  - Stick images on Linux, checked with `sgdisk -v` and byte for byte against Ventoy's files.
  - Real Sequoia 15.8.1 and Mojave 10.14.6 images from mkmaciso: added to a stick image, and
    Linux opens the resulting macOS sections.
  - Every push runs the program on GitHub's virtual Linux, Windows and Apple silicon Mac
    machines, including the menu in a real terminal. The Mac one builds a stick image with
    Apple-made DMGs. The Windows one writes to a virtual disk the same way as to a stick.
- Not yet confirmed:
  - Starting from a stick on a real PC or a real Mac.
  - Writing to a real USB stick from Windows or macOS.
  - The Intel Mac program has been built but never run.
  - **Safely remove** on Windows and macOS.
- No "upgrade boot engine in place" yet. To move to a newer Ventoy, re-run `init` (this erases the stick).
- 4K-sector "native" disks are not supported (normal USB sticks use 512-byte sectors).

## Tests

```bash
cargo test
```

The tests build small fake macOS volumes (HFS+, APFS, GPT, Apple Partition Map,
compressed DMG with zlib + LZFSE), so no Mac and no root are needed. The full-stick test
builds a real image and checks it with `sgdisk` and against Ventoy's own files, byte for byte.


## Credits / license

- Boot engine: [Ventoy](https://www.ventoy.net) by longpanda, GPLv3. sycrOS downloads the
  official release at run time and does not change its boot files.
- Idea for macOS images: [macos-iso-builder / mkmaciso](https://github.com/LongQT-sea/macos-iso-builder) by LongQT-sea.
