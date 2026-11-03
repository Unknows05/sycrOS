//! sycrOS disk layout: build it, read it, add/remove macOS slots.
//!
//! GPT disk, same first two partitions as Ventoy so Ventoy's boot engine works:
//!
//! ```text
//! sector 0         protective MBR + Ventoy boot.img (legacy BIOS boot)
//! sectors 34-2047  Ventoy core.img (GRUB core for BIOS)
//! partition 1      "Ventoy"   exFAT  - ISO/WIM/IMG/VHD files (Windows/Linux/Tools)
//! partition 2      "VTOYEFI"  32 MiB - Ventoy UEFI boot files (BIOS+UEFI, Secure Boot shim)
//! partition 3..N   macOS slots, HFS+ or APFS, one per macOS image (Mac boot menu)
//! free             room for more macOS slots ("Mac space", chosen at init)
//! ```
//!
//! PC -> Ventoy menu. Intel Mac (Option key) -> "EFI Boot" + every macOS slot.
//! Apple silicon -> macOS full-installer slots only.

use crate::engine::{Engine, EFI_PART_SECTORS};
use crate::gpt::{self, Disk as _, Gpt, Partition};
use crate::hosts::{self, DiskInfo};
use crate::library;
use crate::macimg::{self, Fs, Kind};
use crate::rawdisk::{human, Target, SECTOR};
use crate::Result;
use std::path::PathBuf;

pub const MIB: u64 = 1 << 20;
const ALIGN: u64 = 2048; // 1 MiB in sectors
/// Ventoy's boot code expects these two names.
const DATA_NAME: &str = "Ventoy";
const EFI_NAME: &str = "VTOYEFI";
const MIN_DATA: u64 = 256 * MIB;

/// A USB disk (by id) or an image file, ready for raw access.
pub struct Disk {
    pub name: String,
    pub is_image: bool,
    pub info: Option<DiskInfo>,
    pub raw: Target,
    /// loop / hdiutil device while an image is attached
    attached: Option<String>,
}

fn looks_like_image(target: &str) -> bool {
    let low = target.to_lowercase();
    low.ends_with(".img") || low.ends_with(".bin") || low.ends_with(".raw") || std::path::Path::new(target).is_file()
}

/// Safety checks for a real disk: never the running system, and internal
/// disks only with `allow_internal`. Returns the disk info.
pub fn check_disk(target: &str, writable: bool, allow_internal: bool) -> Result<DiskInfo> {
    let info = hosts::find_disk(target)?;
    if info.system {
        return Err(format!("{} holds your running system - refusing to touch it", info.id).into());
    }
    if writable && !info.removable && !allow_internal {
        return Err(format!(
            "{} ({}) is an internal disk, not a USB/SD stick. If you really mean it, add --internal.",
            info.id, info.model
        )
        .into());
    }
    Ok(info)
}

/// True if `target` names an image file rather than a disk.
pub fn is_image_target(target: &str) -> bool {
    looks_like_image(target)
}

impl Disk {
    /// `allow_internal`: also accept non-removable disks (user passed --internal).
    pub fn open(target: &str, create_size: Option<u64>, writable: bool, allow_internal: bool) -> Result<Disk> {
        if looks_like_image(target) {
            if let Some(size) = create_size {
                std::fs::File::create(target)?.set_len(size)?;
            }
            if !std::path::Path::new(target).is_file() {
                return Err(format!("{target} does not exist (add --size to create a new image)").into());
            }
            let size = std::fs::metadata(target)?.len();
            let raw = Target::open(target, size, true, writable)?;
            return Ok(Disk { name: target.into(), is_image: true, info: None, raw, attached: None });
        }
        let info = check_disk(target, writable, allow_internal)?;
        if writable && hosts::IS_MAC {
            hosts::release_disk(&info, false)?; // macOS refuses raw writes while volumes are mounted
        }
        let raw = Target::open(&info.path, info.size, false, writable)
            .map_err(|e| format!("cannot open {}: {e}", info.path))?;
        Ok(Disk { name: info.id.clone(), is_image: false, info: Some(info), raw, attached: None })
    }

    pub fn read_gpt(&mut self) -> Result<Gpt> {
        let n = self.raw.sectors();
        Ok(Gpt::read(&mut self.raw, n)?)
    }

    fn reread(&mut self) {
        if let Some(info) = &self.info {
            hosts::reread_partitions(info);
        }
    }

    /// Where the data partition is mounted (for copying ISO files).
    pub fn data_folder(&mut self) -> Option<PathBuf> {
        if let Some(info) = &self.info {
            return match hosts::mount_data(info, 1) {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("  {e}");
                    None
                }
            };
        }
        self.raw.flush();
        let dev = hosts::attach_image(&self.name)?;
        self.attached = Some(dev.clone());
        let fake = DiskInfo { id: dev.clone(), path: dev, size: self.raw.size, model: "image".into(), removable: true, system: false };
        hosts::mount_data(&fake, 1).ok()
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        self.raw.flush();
        if let Some(dev) = self.attached.take() {
            hosts::detach_image(&dev);
        }
    }
}

// ---------------------------------------------------------------------------
// reading a sycrOS / Ventoy layout
// ---------------------------------------------------------------------------
pub fn check_layout(g: &Gpt) -> Result<(Partition, Partition)> {
    match (g.get(1), g.get(2)) {
        (Some(p1), Some(p2)) if p2.name == EFI_NAME && p2.sectors() == EFI_PART_SECTORS => Ok((p1.clone(), p2.clone())),
        _ => Err("this disk is not a sycrOS/Ventoy GPT disk (run: sycros init)".into()),
    }
}

pub fn mac_slots(g: &Gpt) -> Vec<(usize, Partition)> {
    g.partitions()
        .filter(|(n, p)| *n >= 3 && (p.type_guid == gpt::APPLE_HFS || p.type_guid == gpt::APPLE_APFS))
        .map(|(n, p)| (n, p.clone()))
        .collect()
}

fn slot_fs(p: &Partition) -> Fs {
    if p.type_guid == gpt::APPLE_APFS { Fs::Apfs } else { Fs::Hfs }
}

/// Biggest free gap (bytes) after partition 2.
pub fn mac_free(g: &Gpt) -> u64 {
    let Ok((_, p2)) = check_layout(g) else { return 0 };
    g.free_ranges(p2.last + 1)
        .into_iter()
        .map(|(a, b)| {
            let first = a.div_ceil(ALIGN) * ALIGN;
            if first > b { 0 } else { (b - first + 1) * SECTOR }
        })
        .max()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------
/// (part1_first, part1_last, part2_first): same maths as Ventoy2Disk with reserve.
pub fn plan(total_sectors: u64, mac_space: u64) -> Result<(u64, u64, u64)> {
    let mac_sectors = mac_space.div_ceil(MIB) * ALIGN;
    let reserved = 33 + mac_sectors;
    let need = reserved + EFI_PART_SECTORS + 2048;
    if total_sectors <= need || (total_sectors - need) * SECTOR < MIN_DATA {
        return Err(format!(
            "disk too small: {} total, {} asked for macOS -> less than {} left for ISO files",
            human(total_sectors * SECTOR),
            human(mac_space),
            human(MIN_DATA)
        )
        .into());
    }
    let mut p1_count = total_sectors - need;
    p1_count -= p1_count % 8; // keep partition 2 on a 4 KB boundary
    Ok((2048, 2048 + p1_count - 1, 2048 + p1_count))
}

pub fn build_mbr(engine: &Engine, total_sectors: u64) -> Result<[u8; 512]> {
    let boot = engine.boot_img()?;
    let mut mbr = [0u8; 512];
    mbr[..440].copy_from_slice(&boot[..440]);
    mbr[92] = 0x22; // GRUB boot.img: core.img starts at sector 34 (0x22)
    let mut rnd = [0u8; 20];
    getrandom::getrandom(&mut rnd).map_err(|e| format!("random: {e}"))?;
    mbr[0x180..0x190].copy_from_slice(&rnd[..16]); // Ventoy disk UUID
    mbr[0x1B8..0x1BC].copy_from_slice(&rnd[16..]); // disk signature
    mbr[446..462].copy_from_slice(&gpt::protective_mbr_entry(total_sectors));
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    Ok(mbr)
}

pub fn init(disk: &mut Disk, engine: &Engine, mac_space: u64, label: &str, verbose: bool) -> Result<Gpt> {
    let total = disk.raw.sectors();
    let (p1_first, p1_last, p2_first) = plan(total, mac_space)?;
    let p2_last = p2_first + EFI_PART_SECTORS - 1;
    let log = |s: String| {
        if verbose {
            println!("{s}");
        }
    };

    log(format!("Layout for {} ({}):", disk.name, human(disk.raw.size)));
    log(format!("  1  ISO files (exFAT)  {}", human((p1_last - p1_first + 1) * SECTOR)));
    log("  2  boot files (EFI)   32.0 MB".into());
    log(format!("  -  macOS space        {}", human((total - 34 - p2_last) * SECTOR)));

    if let Some(info) = &disk.info {
        hosts::release_disk(info, true)?;
    }

    let raw = &mut disk.raw;
    // wipe old partition tables / filesystem signatures at both ends
    raw.zero(0, MIB)?;
    raw.zero((total - ALIGN) * SECTOR, ALIGN * SECTOR)?;
    raw.zero(p1_first * SECTOR, MIB)?;

    raw.write_at(0, &build_mbr(engine, total)?)?;
    let mut core = engine.core_img()?;
    core.resize(core.len().div_ceil(512) * 512, 0);
    raw.write_at(34 * SECTOR, &core)?;
    raw.write_at(34 * SECTOR + 500, &[0x23])?; // first blocklist sector = 35

    let mut g = Gpt::new(total);
    g.add(Partition::new(gpt::MS_BASIC_DATA, p1_first, p1_last, 0, DATA_NAME), Some(1))?;
    g.add(Partition::new(gpt::MS_BASIC_DATA, p2_first, p2_last, gpt::ATTR_NO_DRIVE_LETTER, EFI_NAME), Some(2))?;
    g.write(raw)?;

    log("  writing boot files ...".into());
    let efi = engine.efi_img_path()?;
    raw.copy_from_file(&efi.to_string_lossy(), 0, EFI_PART_SECTORS * SECTOR, p2_first * SECTOR, "boot files")?;
    raw.flush();

    log("  formatting ISO partition (exFAT) ...".into());
    let p1_bytes = (p1_last - p1_first + 1) * SECTOR;
    if disk.is_image {
        if hosts::IS_MAC {
            let dev = hosts::attach_image(&disk.name).ok_or("could not attach the image with hdiutil")?;
            let fake = DiskInfo {
                id: dev.clone(),
                path: dev.replacen("/dev/", "/dev/r", 1),
                size: disk.raw.size,
                model: "image".into(),
                removable: true,
                system: false,
            };
            let r = hosts::format_exfat(engine, &fake, 1, label);
            hosts::detach_image(&dev);
            r?;
        } else {
            hosts::format_exfat_image(engine, &disk.name, p1_first * SECTOR, p1_bytes, label, disk.raw.size)?;
        }
    } else {
        let info = disk.info.clone().unwrap();
        // reopen read-only after formatting; the OS now owns the new partitions
        disk.raw = Target::open(&info.path, info.size, false, false)?;
        hosts::reread_partitions(&info);
        hosts::format_exfat(engine, &info, 1, label)?;
    }
    Ok(g)
}

// ---------------------------------------------------------------------------
// macOS slots
// ---------------------------------------------------------------------------
pub fn add_mac(disk: &mut Disk, image: &str, name: Option<&str>, workdir: Option<&str>, verbose: bool) -> Result<usize> {
    let mut g = disk.read_gpt()?;
    let (_, p2) = check_layout(&g)?;
    let log = |s: String| {
        if verbose {
            println!("{s}");
        }
    };

    log(format!("Looking inside {} ...", std::path::Path::new(image).file_name().unwrap_or_default().to_string_lossy()));
    let vol = macimg::find_volume(image, workdir)?; // temp file removed when `vol` drops
    let label: String = name.unwrap_or(&vol.label).chars().take(36).collect();
    let label = if label.is_empty() { "macOS".to_string() } else { label };
    let need = vol.length.div_ceil(MIB) * ALIGN; // sectors, whole MiB
    log(format!("  found {} volume '{}' ({}), {}", vol.fs.label(), vol.label, vol.kind().label(), human(vol.length)));

    let first = g.find_space(need, p2.last + 1, ALIGN).ok_or_else(|| {
        let total_mac = (g.last_usable().saturating_sub(p2.last)) * SECTOR;
        if total_mac < MIB {
            format!(
                "This stick has no macOS space (it was prepared with macOS space 0).\n  \
                 Prepare it again and choose macOS space of at least {} (this erases the stick).",
                human(need * SECTOR)
            )
        } else {
            format!(
                "Not enough macOS space left: this image needs {}, the biggest free part is {}.\n  \
                 Remove a macOS section, or prepare the stick again with more macOS space (this erases it).",
                human(need * SECTOR),
                human(mac_free(&g))
            )
        }
    })?;
    let part = Partition::new(vol.fs.gpt_type(), first, first + need - 1, 0, &label);

    disk.raw.copy_from_file(&vol.source.to_string_lossy(), vol.offset, vol.length, first * SECTOR, "writing macOS")?;
    let tail = need * SECTOR - vol.length;
    if tail > 0 {
        disk.raw.zero(first * SECTOR + vol.length, tail)?;
    }
    let num = g.add(part, None)?;
    g.write(&mut disk.raw)?;
    disk.raw.flush();
    disk.reread();
    log(format!("  macOS slot {num}: '{label}' ready"));
    if vol.kind() == Kind::Recovery {
        log("  note: Recovery images start on Intel Macs only (Apple silicon needs a full installer).".into());
    }
    Ok(num)
}

pub fn remove_mac(disk: &mut Disk, number: usize, verbose: bool) -> Result<Partition> {
    let mut g = disk.read_gpt()?;
    check_layout(&g)?;
    if !mac_slots(&g).iter().any(|(n, _)| *n == number) {
        return Err(format!("partition {number} is not a macOS slot").into());
    }
    if let Some(info) = &disk.info {
        hosts::unmount_slot(info, number);
    }
    let p = g.remove(number)?;
    disk.raw.zero(p.first * SECTOR, MIB.min(p.size()))?; // so no OS finds the old volume
    g.write(&mut disk.raw)?;
    disk.raw.flush();
    disk.reread();
    if verbose {
        println!("Removed macOS slot {number} ('{}'), freed {}", p.name, human(p.size()));
    }
    Ok(p)
}

/// Update folders, ventoy.json and the macOS F6 menu on the data partition.
pub fn refresh_menus(disk: &mut Disk) -> Result<Option<PathBuf>> {
    let g = disk.read_gpt()?;
    let slots: Vec<(usize, String, Fs)> = mac_slots(&g).into_iter().map(|(n, p)| (n, p.name.clone(), slot_fs(&p))).collect();
    let Some(root) = disk.data_folder() else {
        println!("  (data partition not mounted - menus will be written on the next 'add' from a USB disk)");
        return Ok(None);
    };
    library::ensure_layout(&root)?;
    library::write_mac_menu(&root, &slots)?;
    Ok(Some(root))
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------
pub fn status(disk: &mut Disk) -> Result<()> {
    let g = disk.read_gpt()?;
    let (p1, p2) = check_layout(&g)?;
    println!("{}  ({})  Ventoy-compatible GPT layout", disk.name, human(disk.raw.size));
    println!("  1  ISO files      {:>10}   (Windows / Linux / Tools folders)", human(p1.size()));
    println!("  2  boot files     {:>10}", human(p2.size()));
    for (n, p) in mac_slots(&g) {
        println!("  {n}  macOS ({})    {:>10}   {}", slot_fs(&p).label(), human(p.size()), p.name);
    }
    println!("     free for macOS {:>10}", human(mac_free(&g)));
    Ok(())
}
