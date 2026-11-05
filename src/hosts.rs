//! Everything that differs between Linux, macOS and Windows: listing disks,
//! safety checks, unmounting, re-reading the partition table, formatting the
//! exFAT data partition and finding where it is mounted.

use crate::engine::Engine;
use crate::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

pub const IS_LINUX: bool = cfg!(target_os = "linux");
pub const IS_MAC: bool = cfg!(target_os = "macos");
pub const IS_WIN: bool = cfg!(windows);

#[derive(Clone, Debug, PartialEq)]
pub struct DiskInfo {
    /// What the user types: /dev/sdb, /dev/disk4, 2 (Windows disk number).
    pub id: String,
    /// What we open for raw access.
    pub path: String,
    pub size: u64,
    pub model: String,
    pub removable: bool,
    /// Holds the running OS -> never touch.
    pub system: bool,
    pub parts: Vec<DiskPart>,
}

/// One partition, as far as the system can tell without opening it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiskPart {
    /// Volume label, or partition name if there is no label.
    pub label: String,
    /// File system: exfat, ntfs, ext4, hfsplus, ... ("" if unknown).
    pub fs: String,
    /// Partition type, lower case: a GUID on Linux/Windows, a name on macOS.
    pub kind: String,
    /// Partition name in the disk's table ("" if none).
    pub name: String,
    pub size: u64,
}

const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
const APFS: &str = "7c3457ef-0000-11aa-aa11-00306543ecac";
const WINDOWS_KINDS: [&str; 5] =
    ["e3c9e316-0b5c-4db8-817d-f92df00215ae", "de94bba4-06d1-4d40-a16a-bfd50179d6ac", "microsoft reserved", "windows recovery", "windows_ntfs"];
const LINUX_KINDS: [&str; 5] = [
    "4f68bce3-e8cd-4db1-96e7-fbcaf984b709",
    "e6d6d379-f507-44c2-a23c-238f2a3df928",
    "ca7d7ccb-63ed-4c53-861c-1742536059cc",
    "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f",
    "linux swap",
];

/// True if this is already a sycrOS (or Ventoy) stick.
pub fn is_sycros(parts: &[DiskPart]) -> bool {
    parts.iter().any(|p| p.name == "sycrOS-boot" || p.name == "VTOYEFI" || p.label == "VTOYEFI")
}

/// Which system this disk seems to hold, judging by its partitions.
/// A best guess: it protects against mistakes, it cannot prove a disk is safe.
pub fn holds_system(parts: &[DiskPart]) -> Option<&'static str> {
    if is_sycros(parts) {
        return None;
    }
    let has_kind = |list: &[&str]| parts.iter().any(|p| list.contains(&p.kind.as_str()));
    let has_fs = |list: &[&str]| parts.iter().any(|p| list.contains(&p.fs.as_str()));
    let esp = parts.iter().any(|p| p.kind == ESP || p.kind == "efi" || p.kind == "efi system partition");
    if has_kind(&WINDOWS_KINDS) || (esp && has_fs(&["ntfs"])) {
        Some("Windows")
    } else if has_kind(&LINUX_KINDS)
        || has_fs(&["crypto_luks", "lvm2_member", "swap"])
        || (esp && has_fs(&["ext4", "ext3", "btrfs", "xfs", "f2fs"]))
    {
        Some("Linux")
    } else if parts.iter().any(|p| p.kind == APFS || p.kind == "apple_apfs" || p.fs == "apfs") {
        Some("macOS")
    } else {
        None
    }
}

/// Plain-words summary, e.g. "sycrOS stick" or "Windows? 4 parts: Data, Recovery".
pub fn describe(parts: &[DiskPart]) -> String {
    if is_sycros(parts) {
        return "sycrOS stick".into();
    }
    if parts.is_empty() {
        return "empty".into();
    }
    let names: Vec<&str> = parts.iter().map(|p| p.label.as_str()).filter(|l| !l.is_empty()).take(3).collect();
    let count = if parts.len() == 1 { "1 part".to_string() } else { format!("{} parts", parts.len()) };
    let what = if names.is_empty() { count } else { format!("{count}: {}", names.join(", ")) };
    match holds_system(parts) {
        Some(os) => format!("has {os}? {what}"),
        None => what,
    }
}

/// Words the user must type to erase a disk: "ERASE sdb", "ERASE disk4", "ERASE 2".
pub fn erase_words(id: &str) -> String {
    format!("ERASE {}", id.rsplit('/').next().unwrap_or(id))
}

/// A mount at any of these places is normal for a USB stick; anywhere else
/// means the running system is using this disk.
pub fn is_system_mount(m: &str) -> bool {
    m.starts_with("[SWAP") || !["/media/", "/run/media/", "/mnt/", "/tmp/", "/Volumes/"].iter().any(|p| m.starts_with(p))
}

/// Run a command; return stdout. With `check`, a non-zero exit is an error.
pub fn run(cmd: &[&str], check: bool) -> Result<String> {
    let out = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", cmd[0]))?;
    if check && !out.status.success() {
        let msg = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr });
        return Err(format!("command failed: {}\n{}", cmd.join(" "), msg.trim()).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn quiet(cmd: &[&str]) -> bool {
    Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a PowerShell snippet (Windows).
pub fn ps(script: &str, check: bool) -> Result<String> {
    run(&["powershell", "-NoProfile", "-NonInteractive", "-Command", script], check)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

pub fn is_admin() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(windows)]
    {
        ps("([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)", false)
            .map(|s| s.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// listing
// ---------------------------------------------------------------------------
pub fn list_disks() -> Result<Vec<DiskInfo>> {
    if IS_LINUX {
        linux_disks()
    } else if IS_MAC {
        mac_disks()
    } else if IS_WIN {
        win_disks()
    } else {
        Err("unsupported system".into())
    }
}

fn walk_json<'a>(n: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(n);
    if let Some(ch) = n.get("children").and_then(|c| c.as_array()) {
        for c in ch {
            walk_json(c, out);
        }
    }
}

fn mountpoints(n: &Value) -> Vec<String> {
    if let Some(one) = n.get("mountpoint").and_then(|m| m.as_str()) {
        return vec![one.to_string()]; // older lsblk
    }
    n.get("mountpoints")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn linux_disks() -> Result<Vec<DiskInfo>> {
    // MOUNTPOINTS needs util-linux 2.37; older systems only know MOUNTPOINT
    let base = "NAME,PATH,SIZE,MODEL,RM,HOTPLUG,TYPE,TRAN,FSTYPE,LABEL,PARTTYPE,PARTLABEL";
    let text = run(&["lsblk", "-J", "-b", "-o", &format!("{base},MOUNTPOINTS")], true)
        .or_else(|_| run(&["lsblk", "-J", "-b", "-o", &format!("{base},MOUNTPOINT")], true))?;
    let j: Value = serde_json::from_str(&text)?;
    let mut disks = vec![];
    for d in j["blockdevices"].as_array().cloned().unwrap_or_default() {
        if d["type"] != "disk" {
            continue;
        }
        let mut nodes = vec![];
        walk_json(&d, &mut nodes);
        let mounts: Vec<String> = nodes.iter().flat_map(|n| mountpoints(n)).collect();
        let system = mounts.iter().any(|m| is_system_mount(m));
        let text = |v: &Value| v.as_str().unwrap_or("").trim().to_string();
        let parts: Vec<DiskPart> = nodes
            .iter()
            .filter(|n| n["type"] == "part")
            .map(|n| DiskPart {
                label: if text(&n["label"]).is_empty() { text(&n["partlabel"]) } else { text(&n["label"]) },
                fs: text(&n["fstype"]).to_lowercase(),
                kind: text(&n["parttype"]).to_lowercase(),
                name: text(&n["partlabel"]),
                size: n["size"].as_u64().or_else(|| n["size"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0),
            })
            .collect();
        let truthy = |v: &Value| v.as_bool().unwrap_or(false) || v.as_str() == Some("1") || v.as_i64() == Some(1);
        let removable = truthy(&d["rm"]) || truthy(&d["hotplug"]) || d["tran"] == "usb";
        let size = d["size"].as_u64().or_else(|| d["size"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0);
        let path = d["path"].as_str().unwrap_or_default().to_string();
        disks.push(DiskInfo {
            id: path.clone(),
            path,
            size,
            model: d["model"].as_str().unwrap_or("").trim().to_string(),
            removable,
            system,
            parts,
        });
    }
    Ok(disks)
}

fn plist_dict(text: &str) -> Result<plist::Dictionary> {
    plist::Value::from_reader(std::io::Cursor::new(text.as_bytes()))
        .map_err(|e| format!("diskutil output: {e}"))?
        .into_dictionary()
        .ok_or_else(|| "diskutil output is not a dictionary".into())
}

/// Whole disks the running Mac system starts from (an external disk counts too).
fn mac_boot_disks() -> Vec<String> {
    let whole = |d: &str| d.trim_start_matches("/dev/").split('s').take(2).collect::<Vec<_>>().join("s");
    let Ok(text) = run(&["diskutil", "info", "-plist", "/"], false) else { return vec![] };
    let Ok(info) = plist_dict(&text) else { return vec![] };
    let mut out = vec![];
    if let Some(stores) = info.get("APFSPhysicalStores").and_then(|v| v.as_array()) {
        for s in stores {
            if let Some(d) = s.as_dictionary().and_then(|d| d.get("APFSPhysicalStore")).and_then(|v| v.as_string()) {
                out.push(whole(d));
            }
        }
    }
    if let Some(d) = info.get("ParentWholeDisk").and_then(|v| v.as_string()) {
        out.push(whole(d));
    }
    out
}

fn mac_disks() -> Result<Vec<DiskInfo>> {
    let list = plist_dict(&run(&["diskutil", "list", "-plist", "physical"], true)?)?;
    let boot = mac_boot_disks();
    let entries = list.get("AllDisksAndPartitions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut disks = vec![];
    for name in list.get("WholeDisks").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let Some(name) = name.as_string() else { continue };
        let info = plist_dict(&run(&["diskutil", "info", "-plist", name], true)?)?;
        let b = |k: &str, d: bool| info.get(k).and_then(|v| v.as_boolean()).unwrap_or(d);
        let internal = b("Internal", true);
        let entry = entries.iter().filter_map(|e| e.as_dictionary()).find(|e| {
            e.get("DeviceIdentifier").and_then(|v| v.as_string()) == Some(name)
        });
        let parts = entry
            .and_then(|e| e.get("Partitions"))
            .and_then(|v| v.as_array())
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| p.as_dictionary())
                    .map(|p| {
                        let t = |k: &str| p.get(k).and_then(|v| v.as_string()).unwrap_or("").to_string();
                        DiskPart {
                            label: t("VolumeName"),
                            fs: String::new(),
                            kind: t("Content").to_lowercase(),
                            name: t("VolumeName"),
                            size: p.get("Size").and_then(|v| v.as_unsigned_integer()).unwrap_or(0),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        disks.push(DiskInfo {
            id: format!("/dev/{name}"),
            path: format!("/dev/r{name}"),
            size: info.get("TotalSize").and_then(|v| v.as_unsigned_integer()).unwrap_or(0),
            model: info.get("MediaName").and_then(|v| v.as_string()).unwrap_or("").to_string(),
            removable: !internal,
            system: boot.iter().any(|d| d == name) || (internal && !b("RemovableMediaOrExternalDevice", false)),
            parts,
        });
    }
    Ok(disks)
}

fn win_disks() -> Result<Vec<DiskInfo>> {
    let raw = ps("Get-Disk | Select-Object Number,Size,FriendlyName,BusType,IsBoot,IsSystem | ConvertTo-Json", true)?;
    let v: Value = serde_json::from_str(if raw.trim().is_empty() { "[]" } else { &raw })?;
    let items = match v {
        Value::Array(a) => a,
        other => vec![other],
    };
    let parts_raw = ps(
        "Get-Partition -ErrorAction SilentlyContinue | ForEach-Object { $v = $_ | Get-Volume -ErrorAction SilentlyContinue; \
         [pscustomobject]@{ D = $_.DiskNumber; S = $_.Size; K = $_.GptType; T = $_.Type; L = $v.FileSystemLabel; F = $v.FileSystem } } | ConvertTo-Json",
        false,
    )
    .unwrap_or_default();
    let parts: Vec<Value> = match serde_json::from_str(if parts_raw.trim().is_empty() { "[]" } else { &parts_raw }) {
        Ok(Value::Array(a)) => a,
        Ok(other) => vec![other],
        Err(_) => vec![],
    };
    Ok(items
        .iter()
        .map(|d| {
            let n = d["Number"].as_u64().unwrap_or(0).to_string();
            let bus = d["BusType"].as_str().map(String::from).unwrap_or_else(|| d["BusType"].to_string());
            DiskInfo {
                path: format!(r"\\.\PhysicalDrive{n}"),
                id: n,
                size: d["Size"].as_u64().unwrap_or(0),
                model: d["FriendlyName"].as_str().unwrap_or("").to_string(),
                // BusType may come as text ("USB") or number (7 = USB, 12 = SD, 13 = MMC)
                removable: ["USB", "SD", "MMC", "7", "12", "13"].contains(&bus.as_str()),
                system: d["IsBoot"].as_bool().unwrap_or(false) || d["IsSystem"].as_bool().unwrap_or(false),
                parts: parts
                    .iter()
                    .filter(|p| p["D"].as_u64() == d["Number"].as_u64())
                    .map(|p| {
                        let t = |k: &str| p[k].as_str().unwrap_or("").trim().to_string();
                        let guid = t("K").trim_matches(|c| c == '{' || c == '}').to_lowercase();
                        DiskPart {
                            label: t("L"),
                            fs: t("F").to_lowercase(),
                            kind: if guid.is_empty() { t("T").to_lowercase() } else { guid },
                            name: t("L"),
                            size: p["S"].as_u64().unwrap_or(0),
                        }
                    })
                    .collect(),
            }
        })
        .collect())
}

pub fn find_disk(user_id: &str) -> Result<DiskInfo> {
    let mut norm = user_id.trim().to_string();
    if IS_WIN {
        let low = norm.to_lowercase();
        if let Some(i) = low.find("physicaldrive") {
            norm = norm[i + "physicaldrive".len()..].to_string();
        }
    }
    if IS_MAC {
        norm = norm.replace("/dev/rdisk", "/dev/disk");
        if !norm.starts_with("/dev/") {
            norm = format!("/dev/{norm}");
        }
    }
    list_disks()?
        .into_iter()
        .find(|d| d.id == norm)
        .ok_or_else(|| format!("disk '{user_id}' not found. Run: sycros disks").into())
}

// ---------------------------------------------------------------------------
// partition names
// ---------------------------------------------------------------------------
pub fn part_path(disk: &DiskInfo, n: usize) -> String {
    if IS_LINUX {
        if disk.id.ends_with(|c: char| c.is_ascii_digit()) {
            format!("{}p{n}", disk.id)
        } else {
            format!("{}{n}", disk.id)
        }
    } else if IS_MAC {
        format!("{}s{n}", disk.id)
    } else {
        format!("{}:{n}", disk.id)
    }
}

// ---------------------------------------------------------------------------
// before / after raw writes
// ---------------------------------------------------------------------------
fn lsblk_nodes(dev: &str) -> Vec<(String, Vec<String>)> {
    let Ok(text) = run(&["lsblk", "-J", "-o", "PATH,MOUNTPOINTS", dev], false) else { return vec![] };
    let Ok(j) = serde_json::from_str::<Value>(&text) else { return vec![] };
    let mut nodes = vec![];
    // lsblk lists loop partitions flat but real disks nested: walk both shapes
    for d in j["blockdevices"].as_array().into_iter().flatten() {
        walk_json(d, &mut nodes);
    }
    nodes.iter().map(|n| (n["path"].as_str().unwrap_or("").to_string(), mountpoints(n))).collect()
}

/// Unmount everything on the disk so we can write raw bytes to it.
pub fn release_disk(disk: &DiskInfo, wipe: bool) -> Result<()> {
    if IS_LINUX {
        for (path, mounts) in lsblk_nodes(&disk.id) {
            if !mounts.is_empty() {
                let _ = run(&["umount", &path], false);
            }
        }
    } else if IS_MAC {
        run(&["diskutil", "unmountDisk", "force", &disk.id], true)?;
    } else if IS_WIN && wipe {
        let n = &disk.id;
        let _ = ps(
            &format!(
                "Clear-Disk -Number {n} -RemoveData -RemoveOEM -Confirm:$false -ErrorAction SilentlyContinue; \
                 Set-Disk -Number {n} -IsOffline $false -ErrorAction SilentlyContinue; \
                 Set-Disk -Number {n} -IsReadOnly $false"
            ),
            false,
        );
    }
    Ok(())
}

/// Tell the OS the partition table changed.
pub fn reread_partitions(disk: &DiskInfo) {
    if IS_LINUX {
        let id = disk.id.as_str();
        for cmd in [vec!["blockdev", "--rereadpt", id], vec!["partprobe", id], vec!["partx", "-u", id]] {
            if quiet(&cmd) {
                break;
            }
        }
        quiet(&["udevadm", "settle"]);
    } else if IS_MAC {
        sleep(Duration::from_secs(2)); // macOS re-reads by itself once the raw device is closed
    } else if IS_WIN {
        let _ = ps(&format!("Update-HostStorageCache; Update-Disk -Number {}", disk.id), false);
        sleep(Duration::from_secs(2));
    }
}

fn wait_for(path: &str) -> Result<()> {
    for _ in 0..30 {
        if Path::new(path).exists() {
            return Ok(());
        }
        sleep(Duration::from_millis(500));
    }
    Err(format!("partition {path} did not show up").into())
}

// ---------------------------------------------------------------------------
// exFAT
// ---------------------------------------------------------------------------
/// Same rule as Ventoy: 128 KB clusters above 32 GB, 32 KB below.
pub fn exfat_cluster_sectors(disk_size: u64) -> u64 {
    if disk_size > 32 << 30 { 256 } else { 64 }
}

fn linux_mkfs(engine: &Engine, target: &str, label: &str, cluster_sectors: u64) -> Result<()> {
    if which("mkfs.exfat").is_some() {
        let c = (cluster_sectors * 512).to_string();
        if quiet(&["mkfs.exfat", "-n", label, "-c", &c, target]) {
            return Ok(());
        }
    }
    let mk = engine.mkexfatfs().ok_or("no exFAT formatter found. Install exfatprogs (mkfs.exfat).")?;
    run(&[&mk.to_string_lossy(), "-n", label, "-s", &cluster_sectors.to_string(), target], true)?;
    Ok(())
}

/// Format partition `n` of a real disk as exFAT.
pub fn format_exfat(engine: &Engine, disk: &DiskInfo, n: usize, label: &str) -> Result<()> {
    let cs = exfat_cluster_sectors(disk.size);
    if IS_LINUX {
        let p = part_path(disk, n);
        wait_for(&p)?;
        linux_mkfs(engine, &p, label, cs)
    } else if IS_MAC {
        run(&["diskutil", "eraseVolume", "ExFAT", label, &part_path(disk, n)], true).map(|_| ())
    } else {
        let id = &disk.id;
        ps(
            &format!(
                "$p = Get-Partition -DiskNumber {id} -PartitionNumber {n}; \
                 $p | Format-Volume -FileSystem exFAT -NewFileSystemLabel '{label}' -AllocationUnitSize {} -Force -Confirm:$false | Out-Null; \
                 if (-not ($p | Get-Partition).DriveLetter) {{ $p | Add-PartitionAccessPath -AssignDriveLetter }}",
                cs * 512
            ),
            true,
        )
        .map(|_| ())
    }
}

/// Format an exFAT partition that lives inside an image file (Linux).
pub fn format_exfat_image(engine: &Engine, image: &str, offset: u64, length: u64, label: &str, disk_size: u64) -> Result<()> {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    if !IS_LINUX {
        return Err("building .img files is supported on Linux and macOS. On Windows, write to a USB disk directly.".into());
    }
    let dir = Path::new(image).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".sycros-exfat-{}", std::process::id()));
    let result = (|| -> Result<()> {
        File::create(&tmp)?.set_len(length)?;
        linux_mkfs(engine, &tmp.to_string_lossy(), label, exfat_cluster_sectors(disk_size))?;
        // copy only the non-empty parts back (keeps the image sparse)
        let mut src = File::open(&tmp)?;
        let mut dst = OpenOptions::new().write(true).open(image)?;
        let mut buf = vec![0u8; 1 << 20];
        let mut pos = 0u64;
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if buf[..n].iter().any(|&b| b != 0) {
                dst.seek(SeekFrom::Start(offset + pos))?;
                dst.write_all(&buf[..n])?;
            }
            pos += n as u64;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

// ---------------------------------------------------------------------------
// mounting the data partition (to copy ISO files)
// ---------------------------------------------------------------------------
/// When running as root through sudo: the real user's (name, uid, gid).
fn sudo_user() -> Option<(String, u32, u32)> {
    if !IS_LINUX || !is_admin() {
        return None;
    }
    let name = std::env::var("SUDO_USER").ok().filter(|n| !n.is_empty() && n != "root")?;
    let uid = std::env::var("SUDO_UID").ok()?.parse().ok()?;
    let gid = std::env::var("SUDO_GID").ok()?.parse().ok()?;
    Some((name, uid, gid))
}

fn owned_by_root(path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).map(|m| m.uid() == 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// A folder where partition `n` is mounted (mounting it if needed).
/// On Linux under sudo it is mounted for the real user, so the Files app can open it.
pub fn mount_data(disk: &DiskInfo, n: usize) -> Result<PathBuf> {
    if IS_LINUX {
        let p = part_path(disk, n);
        wait_for(&p)?;
        let user = sudo_user();
        let current = run(&["lsblk", "-no", "MOUNTPOINT", &p], false)?
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty());
        if let Some(mp) = current {
            // Mounted by root (e.g. by an earlier `sudo sycros`)? Then the owner's file
            // manager cannot open it: remount it for the user who ran sudo.
            if user.is_some() && owned_by_root(&mp) {
                let _ = run(&["umount", &p], false);
            } else {
                return Ok(PathBuf::from(mp));
            }
        }
        if let Some((name, uid, gid)) = user {
            // 1) let the desktop mount it as the real user (shows up in the Files app)
            if which("runuser").is_some() && which("udisksctl").is_some() {
                let out = run(&["runuser", "-u", &name, "--", "udisksctl", "mount", "--no-user-interaction", "-b", &p], false)?;
                if let Some(i) = out.find(" at ") {
                    return Ok(PathBuf::from(out[i + 4..].trim().trim_end_matches('.')));
                }
            }
            // 2) mount it ourselves, owned by the real user
            let label = run(&["lsblk", "-no", "LABEL", &p], false)?.trim().to_string();
            let mnt = PathBuf::from(format!("/media/{name}/{}", if label.is_empty() { "sycrOS" } else { &label }));
            std::fs::create_dir_all(&mnt)?;
            let opts = format!("uid={uid},gid={gid},umask=0022");
            run(&["mount", "-o", &opts, &p, &mnt.to_string_lossy()], true)?;
            return Ok(mnt);
        }
        if which("udisksctl").is_some() {
            let out = run(&["udisksctl", "mount", "--no-user-interaction", "-b", &p], false)?;
            if let Some(i) = out.find(" at ") {
                return Ok(PathBuf::from(out[i + 4..].trim().trim_end_matches('.')));
            }
        }
        let mnt = std::env::temp_dir().join(format!("sycros-mnt-{}", std::process::id()));
        std::fs::create_dir_all(&mnt)?;
        run(&["mount", &p, &mnt.to_string_lossy()], true)?;
        Ok(mnt)
    } else if IS_MAC {
        let p = part_path(disk, n);
        let _ = run(&["diskutil", "mount", &p], false);
        let info = plist_dict(&run(&["diskutil", "info", "-plist", &p], true)?)?;
        info.get("MountPoint")
            .and_then(|v| v.as_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("could not mount {p}").into())
    } else {
        let id = &disk.id;
        let letter = ps(
            &format!(
                "$p = Get-Partition -DiskNumber {id} -PartitionNumber {n}; \
                 if (-not $p.DriveLetter) {{ $p | Add-PartitionAccessPath -AssignDriveLetter; \
                 $p = Get-Partition -DiskNumber {id} -PartitionNumber {n} }}; $p.DriveLetter"
            ),
            true,
        )?;
        let letter = letter.trim();
        if letter.is_empty() {
            return Err("could not give the data partition a drive letter".into());
        }
        Ok(PathBuf::from(format!("{letter}:\\")))
    }
}

/// Finish all writing and unmount every part of the disk, so it can be unplugged.
pub fn eject_disk(disk: &DiskInfo) -> Result<()> {
    if IS_LINUX {
        quiet(&["sync"]);
        for _ in 0..3 {
            for (path, mounts) in lsblk_nodes(&disk.id) {
                if !mounts.is_empty() {
                    let _ = run(&["umount", &path], false);
                }
            }
            sleep(Duration::from_secs(1));
        }
        let busy: Vec<String> = lsblk_nodes(&disk.id).into_iter().flat_map(|(_, m)| m).collect();
        if !busy.is_empty() {
            return Err(format!(
                "The stick is still in use at: {}\n  Close any window or terminal open there, then try again.",
                busy.join(", ")
            )
            .into());
        }
        // Power off where possible; being unmounted is already enough to be safe.
        quiet(&["udisksctl", "power-off", "--no-user-interaction", "-b", &disk.id]);
        Ok(())
    } else if IS_MAC {
        run(&["diskutil", "eject", &disk.id], true).map(|_| ())
    } else {
        ps(
            &format!(
                "Get-Partition -DiskNumber {} | Where-Object {{ $_.DriveLetter -match '[A-Z]' }} | ForEach-Object {{ \
                 Write-VolumeCache -DriveLetter $_.DriveLetter; \
                 (New-Object -ComObject Shell.Application).Namespace(17).ParseName(\"$($_.DriveLetter):\").InvokeVerb('Eject') }}",
                disk.id
            ),
            true,
        )
        .map(|_| ())
    }
}

/// macOS auto-mounts HFS+/APFS slots; unmount one before overwriting it.
pub fn unmount_slot(disk: &DiskInfo, n: usize) {
    let p = part_path(disk, n);
    if IS_MAC {
        let _ = run(&["diskutil", "unmount", "force", &p], false);
    } else if IS_LINUX && Path::new(&p).exists() {
        let _ = run(&["umount", &p], false);
    }
}

// ---------------------------------------------------------------------------
// image files: attach as a disk device (Linux: udisks, no root; macOS: hdiutil)
// ---------------------------------------------------------------------------
pub fn attach_image(image: &str) -> Option<String> {
    if IS_LINUX && which("udisksctl").is_some() {
        let out = run(&["udisksctl", "loop-setup", "--no-user-interaction", "-f", image], false).ok()?;
        let i = out.find("/dev/loop")?;
        let dev: String = out[i..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '/').collect();
        return Some(dev.trim_end_matches('.').to_string());
    }
    if IS_MAC {
        let out = run(&["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", image], true).ok()?;
        // the first /dev/diskN printed is the whole disk (partitions follow as /dev/diskNsM)
        return out.split_whitespace().find(|w| w.starts_with("/dev/disk")).map(String::from);
    }
    None
}

pub fn detach_image(dev: &str) {
    if IS_LINUX {
        // The desktop may auto-mount partitions a moment after attach, so keep
        // unmounting until nothing shows up 3 checks in a row. loop-setup sets
        // "autoclear": the loop device goes away once nothing is mounted
        // (loop-delete itself would need a password).
        quiet(&["sync"]);
        quiet(&["udevadm", "settle", "--timeout=5"]);
        let mut clean = 0;
        for _ in 0..15 {
            let nodes = lsblk_nodes(dev);
            if nodes.is_empty() {
                break; // loop device already gone
            }
            let mounted: Vec<String> =
                nodes.into_iter().filter(|(p, m)| p != dev && !m.is_empty()).map(|(p, _)| p).collect();
            for p in &mounted {
                let _ = run(&["udisksctl", "unmount", "--no-user-interaction", "-b", p], false);
            }
            clean = if mounted.is_empty() { clean + 1 } else { 0 };
            if clean >= 3 {
                break;
            }
            sleep(Duration::from_secs(1));
        }
        let _ = run(&["udisksctl", "loop-delete", "--no-user-interaction", "-b", dev], false);
    } else if IS_MAC {
        let _ = run(&["hdiutil", "detach", dev], false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(label: &str, fs: &str, kind: &str) -> DiskPart {
        DiskPart { label: label.into(), fs: fs.into(), kind: kind.into(), name: label.into(), size: 1 }
    }

    #[test]
    fn recognises_disks_that_hold_a_system() {
        let linux = [part("", "vfat", ESP), part("", "ext4", "0fc63daf-8483-4772-8e79-3d69d8477de4")];
        assert_eq!(holds_system(&linux), Some("Linux"));
        let windows = [part("", "vfat", ESP), part("", "", WINDOWS_KINDS[0]), part("C", "ntfs", "x")];
        assert_eq!(holds_system(&windows), Some("Windows"));
        assert_eq!(holds_system(&[part("", "crypto_luks", "x")]), Some("Linux"));
        assert_eq!(holds_system(&[part("Mac", "", "apple_apfs")]), Some("macOS"));
    }

    #[test]
    fn normal_sticks_are_not_systems() {
        assert_eq!(holds_system(&[part("SANDISK", "exfat", "0x7")]), None);
        assert_eq!(holds_system(&[part("Ubuntu 24", "iso9660", "0x0"), part("ESP", "vfat", ESP)]), None);
        assert_eq!(holds_system(&[]), None);
        // a sycrOS stick with an APFS section is still a stick
        let ours = [part("sycrOS", "exfat", "x"), part("VTOYEFI", "vfat", "x"), part("Base", "", APFS)];
        assert_eq!(holds_system(&ours), None);
        assert_eq!(describe(&ours), "sycrOS stick");
    }

    #[test]
    fn system_mounts_and_erase_words() {
        for m in ["/", "/boot/efi", "/home", "/var", "/nix", "[SWAP]"] {
            assert!(is_system_mount(m), "{m}");
        }
        for m in ["/media/me/sycrOS", "/run/media/me/x", "/mnt/usb", "/Volumes/sycrOS"] {
            assert!(!is_system_mount(m), "{m}");
        }
        assert_eq!(erase_words("/dev/sdb"), "ERASE sdb");
        assert_eq!(erase_words("2"), "ERASE 2");
    }
}
