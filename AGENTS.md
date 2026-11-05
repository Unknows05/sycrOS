# sycrOS - agent notes

Multi-boot USB maker (Windows / Linux ISOs via Ventoy's boot engine + macOS DMG/ISO slots).
Written in **Rust**. All crates are pure Rust (no C, no TLS): keep it that way, so building
for Windows/macOS from anywhere stays easy. Downloads go through the system `curl`.

## Map (src/)
- `gpt.rs` - GPT read/write + CRCs, `Guid`, `Disk` trait, `MemDisk` (testable in memory)
- `rawdisk.rs` - `Target`: sector-safe raw I/O on devices and image files; `human()`, `Progress`
- `engine.rs` - pinned Ventoy release download + SHA-256 (bump `VENTOY_VERSION`, `URL`, `TARBALL` AND `SHA256` together)
- `layout.rs` - `Disk::open`, `check_disk` (safety), `init` / `add_mac` / `remove_mac` / `status`.
  Partitions 1+2 MUST stay compatible with Ventoy (part 1 GPT name "Ventoy"; part 2 GPT name is ours,
  "sycrOS-boot", old sticks "VTOYEFI"; its FAT LABEL must stay VTOYEFI or Ventoy's boot program stops; 32 MiB part 2,
  attr bit 63, MBR byte 92 = 0x22, core.img at LBA 34, byte 34*512+500 = 0x23)
- `macimg.rs`, `udif.rs` - find HFS+/APFS volume in dmg/iso/img (zlib, bzip2, LZMA, LZFSE, ADC)
- `library.rs` - OS folders, ventoy.json merge, F6 macOS menu (ventoy_grub.cfg), file sorting
- `hosts.rs` - all OS-specific commands (lsblk/udisksctl, diskutil/hdiutil, PowerShell)
- `main.rs` - clap CLI (`run_cmd`); no command = `menu.rs` (dialoguer menu, reuses `run_cmd`).
- `tests/` - integration tests; `tests/common` builds fake Mac images.
- `install.sh` / `install.ps1` - one-line installers (download releases/latest; file names have no version).
- `packaging/` - `linux/build-deb.sh` (.deb + .desktop, static musl build), Windows/macOS double-click launchers.
  `.cargo/config.toml` builds the C runtime into the Windows .exe (no VCRUNTIME140.dll).
  `.github/workflows/build.yml` builds all packages; tag `v*` publishes a Release.

## Rules
- Before saying done: `cargo clippy --all-targets` clean and `cargo test` green.
  Also `cargo clippy --target x86_64-pc-windows-gnu` and `--target aarch64-apple-darwin`
  (rustup targets are installed) so Windows/macOS code still compiles.
- Menu changes: `tests/menu_pty.rs` drives the real menu in a real terminal on all three systems.
- Windows CI writes to a virtual disk (.vhd) with init/add/remove/eject; still not a real USB stick.
- Packages must run on any machine: Linux = musl static (check `file` says static), Windows .exe must
  not import VCRUNTIME140.dll (check `objdump -p`), macOS binaries must pass `codesign --verify` (CI does).
- macOS sections must be EXACTLY the volume's size (only the start is aligned): HFS+ needs its
  backup header in the last 1024 bytes of the partition. Check with a real image: Linux must mount it.
- `macimg::looks_like_mac`: the Mac volume must fill at least half the file (Linux ISOs may hold a tiny HFS+ part).
- GitHub's test machines are virtual. Do not call them real hardware.
- Never write to a real disk in tests; use image files (sparse) only.
- Windows and macOS host code is compile-checked only, not run on real machines. Say so when reporting.
- Owner reads plain English: no jargon in messages, README or CLI output.
