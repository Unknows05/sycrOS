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

#[derive(Clone, Debug)]
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
    n.get("mountpoints")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn linux_disks() -> Result<Vec<DiskInfo>> {
    let j: Value = serde_json::from_str(&run(
        &["lsblk", "-J", "-b", "-o", "NAME,PATH,SIZE,MODEL,RM,HOTPLUG,TYPE,MOUNTPOINTS,TRAN"],
        true,
    )?)?;
    let mut disks = vec![];
    for d in j["blockdevices"].as_array().cloned().unwrap_or_default() {
        if d["type"] != "disk" {
            continue;
        }
        let mut nodes = vec![];
        walk_json(&d, &mut nodes);
        let mounts: Vec<String> = nodes.iter().flat_map(|n| mountpoints(n)).collect();
        let system = mounts
            .iter()
            .any(|m| ["/", "/boot", "/boot/efi", "/usr", "/home"].contains(&m.as_str()) || m.starts_with("[SWAP"));
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

fn mac_disks() -> Result<Vec<DiskInfo>> {
    let list = plist_dict(&run(&["diskutil", "list", "-plist", "physical"], true)?)?;
    let mut disks = vec![];
    for name in list.get("WholeDisks").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let Some(name) = name.as_string() else { continue };
        let info = plist_dict(&run(&["diskutil", "info", "-plist", name], true)?)?;
        let b = |k: &str, d: bool| info.get(k).and_then(|v| v.as_boolean()).unwrap_or(d);
        let internal = b("Internal", true);
        disks.push(DiskInfo {
            id: format!("/dev/{name}"),
            path: format!("/dev/r{name}"),
            size: info.get("TotalSize").and_then(|v| v.as_unsigned_integer()).unwrap_or(0),
            model: info.get("MediaName").and_then(|v| v.as_string()).unwrap_or("").to_string(),
            removable: !internal,
            system: internal && !b("RemovableMediaOrExternalDevice", false),
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
