//! "Check this computer": battery health, parts and disk health, shown on screen.
//!
//! Nothing is written anywhere. Linux reads the system's own files, Windows asks PowerShell,
//! macOS asks `ioreg`, `sysctl` and `system_profiler`. Every reader is split into "get the text"
//! and "understand the text", so the second half can be tested with made-up data.

use crate::hosts;
use crate::rawdisk::human;
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub name: String,
    pub maker: String,
    pub status: String,
    /// Charge now, in percent.
    pub percent: Option<u32>,
    /// What the battery could hold when new, and what it can hold today ("Wh" or "mAh").
    pub design: Option<f64>,
    pub full: Option<f64>,
    pub unit: &'static str,
    pub cycles: Option<u32>,
}

impl Battery {
    /// Today's capacity as a share of the new capacity, in percent.
    pub fn health(&self) -> Option<f64> {
        match (self.design, self.full) {
            (Some(d), Some(f)) if d > 0.0 => Some(f / d * 100.0),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cpu {
    pub model: String,
    pub cores: Option<u32>,
    pub threads: Option<u32>,
    pub max_mhz: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stick {
    pub slot: String,
    pub size: u64,
    pub kind: String,
    pub speed_mhz: Option<u32>,
    pub maker: String,
    pub part: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ram {
    pub total: u64,
    pub sticks: Vec<Stick>,
    /// The most memory the main board accepts, and how many slots it has.
    pub max: Option<u64>,
    pub slots: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiskCheck {
    pub name: String,
    pub model: String,
    pub size: u64,
    pub kind: String,
    /// "Good", "Warning", ... or None when the disk does not tell.
    pub health: Option<String>,
    /// Share of the disk's life already used, in percent.
    pub used_pct: Option<u32>,
    pub hours: Option<u64>,
    pub temp_c: Option<i32>,
    pub written_tb: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Machine {
    pub vendor: String,
    pub model: String,
    pub serial: String,
    pub bios: String,
    pub os: String,
    pub gpus: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub machine: Machine,
    pub cpu: Cpu,
    pub ram: Ram,
    pub batteries: Vec<Battery>,
    pub disks: Vec<DiskCheck>,
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------
fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn num(p: &Path) -> Option<f64> {
    read(p)?.parse().ok()
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn int(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)).or_else(|| v.as_str()?.trim().parse().ok())
}

/// A JSON value that PowerShell made either a list or (for one item) a single object.
fn list(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => vec![],
        other => vec![other],
    }
}

fn clean(s: &str) -> String {
    let s = s.trim();
    let junk = ["", "to be filled by o.e.m.", "default string", "system manufacturer", "system product name", "not specified", "unknown", "none"];
    if junk.contains(&s.to_lowercase().as_str()) { String::new() } else { s.to_string() }
}

fn run(cmd: &[&str]) -> Option<String> {
    hosts::run(cmd, false).ok().filter(|s| !s.trim().is_empty())
}

// ---------------------------------------------------------------------------
// asking the computer
// ---------------------------------------------------------------------------
pub fn collect() -> Report {
    let mut r = if hosts::IS_LINUX {
        collect_linux(Path::new("/"))
    } else if hosts::IS_MAC {
        collect_mac()
    } else {
        collect_windows()
    };
    if hosts::IS_LINUX || hosts::IS_MAC {
        r.disks = disks_from_system();
    }
    if !hosts::is_admin() {
        let how = if hosts::IS_WIN { "open sycrOS as Administrator" } else { "run: sudo sycros check" };
        r.notes.push(format!("For the full picture ({}), {how}.", if hosts::IS_LINUX { "memory sticks, serial number, disk health" } else { "disk health" }));
    }
    r
}

// ---- Linux ----------------------------------------------------------------
/// Everything the Linux system files tell, read below `root` ("/" on a real computer).
pub fn collect_linux(root: &Path) -> Report {
    let dmi = root.join("sys/class/dmi/id");
    let d = |name: &str| clean(&read(&dmi.join(name)).unwrap_or_default());
    let mut machine = Machine {
        vendor: d("sys_vendor"),
        model: [d("product_name"), d("product_version")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "),
        serial: d("product_serial"),
        bios: [d("bios_vendor"), d("bios_version"), d("bios_date")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "),
        os: String::new(),
        gpus: vec![],
    };
    machine.os = read(&root.join("etc/os-release"))
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
        .unwrap_or_else(|| "Linux".into());
    if let Some(k) = read(&root.join("proc/sys/kernel/osrelease")) {
        machine.os = format!("{} (kernel {k})", machine.os);
    }
    machine.gpus = run(&["lspci", "-mm"]).map(|t| parse_lspci(&t)).unwrap_or_default();

    let mut cpu = parse_cpuinfo(&read(&root.join("proc/cpuinfo")).unwrap_or_default());
    cpu.max_mhz = num(&root.join("sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")).map(|k| (k / 1000.0) as u32);

    let mut ram = Ram::default();
    let notes = vec![];
    let entries = root.join("sys/firmware/dmi/entries");
    let mut names: Vec<String> = fs::read_dir(&entries).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    for name in names {
        let Some(kind) = name.split('-').next().and_then(|k| k.parse::<u8>().ok()) else { continue };
        if kind != 16 && kind != 17 {
            continue;
        }
        match fs::read(entries.join(&name).join("raw")) {
            Ok(raw) if kind == 17 => ram.sticks.extend(parse_smbios_memory_device(&raw)),
            Ok(raw) => {
                if let Some((max, slots)) = parse_smbios_memory_array(&raw) {
                    ram.max = Some(ram.max.unwrap_or(0) + max);
                    ram.slots = Some(ram.slots.unwrap_or(0) + slots);
                }
            }
            Err(_) => {}
        }
    }
    ram.total = if ram.sticks.is_empty() {
        read(&root.join("proc/meminfo"))
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("MemTotal:").and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())))
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    } else {
        ram.sticks.iter().map(|s| s.size).sum()
    };

    Report { machine, cpu, ram, batteries: linux_batteries(root), disks: vec![], notes }
}

/// Real batteries only (not a wireless mouse's battery).
pub fn linux_batteries(root: &Path) -> Vec<Battery> {
    let dir = root.join("sys/class/power_supply");
    let mut names: Vec<String> = fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    let mut out = vec![];
    for name in names {
        let d = dir.join(&name);
        if read(&d.join("type")).as_deref() != Some("Battery") || read(&d.join("scope")).as_deref() == Some("Device") {
            continue;
        }
        let volts = num(&d.join("voltage_min_design")).or_else(|| num(&d.join("voltage_now"))).map(|uv| uv / 1e6);
        let mut b = Battery {
            name,
            maker: [read(&d.join("manufacturer")), read(&d.join("model_name"))].into_iter().flatten().collect::<Vec<_>>().join(" "),
            status: read(&d.join("status")).unwrap_or_default(),
            percent: num(&d.join("capacity")).map(|c| c as u32),
            cycles: num(&d.join("cycle_count")).map(|c| c as u32).filter(|c| *c > 0),
            unit: "Wh",
            ..Default::default()
        };
        if let (Some(design), Some(full)) = (num(&d.join("energy_full_design")), num(&d.join("energy_full"))) {
            (b.design, b.full) = (Some(design / 1e6), Some(full / 1e6));
        } else if let (Some(design), Some(full)) = (num(&d.join("charge_full_design")), num(&d.join("charge_full"))) {
            match volts {
                Some(v) => (b.design, b.full) = (Some(design / 1e6 * v), Some(full / 1e6 * v)),
                None => (b.design, b.full, b.unit) = (Some(design / 1e3), Some(full / 1e3), "mAh"),
            }
        }
        out.push(b);
    }
    out
}

pub fn parse_cpuinfo(t: &str) -> Cpu {
    let mut cpu = Cpu::default();
    let mut threads = 0;
    let mut cores = std::collections::HashSet::new();
    let (mut pkg, mut core) = (String::new(), String::new());
    for line in t.lines().chain(std::iter::once("")) {
        let Some((k, v)) = line.split_once(':') else {
            if line.trim().is_empty() && !core.is_empty() {
                cores.insert((std::mem::take(&mut pkg), std::mem::take(&mut core)));
            }
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "processor" => threads += 1,
            "model name" | "Model Name" if cpu.model.is_empty() => cpu.model = v.to_string(),
            "Hardware" | "Model" if cpu.model.is_empty() => cpu.model = v.to_string(),
            "physical id" => pkg = v.to_string(),
            "core id" => core = v.to_string(),
            _ => {}
        }
    }
    cpu.threads = (threads > 0).then_some(threads);
    cpu.cores = if cores.is_empty() { cpu.threads } else { Some(cores.len() as u32) };
    cpu
}

/// Graphics cards from `lspci -mm` output.
pub fn parse_lspci(t: &str) -> Vec<String> {
    t.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('"').collect();
            let class = f.get(1)?;
            (class.contains("VGA") || class.contains("3D controller") || class.contains("Display controller"))
                .then(|| format!("{} {}", f.get(3).unwrap_or(&""), f.get(5).unwrap_or(&"")).trim().to_string())
        })
        .collect()
}

fn smbios_string(raw: &[u8], index: u8) -> String {
    if index == 0 || raw.len() < 2 {
        return String::new();
    }
    let start = raw[1] as usize;
    raw.get(start..)
        .and_then(|rest| rest.split(|b| *b == 0).nth(index as usize - 1))
        .map(|s| clean(&String::from_utf8_lossy(s)))
        .unwrap_or_default()
}

fn w16(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn w32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}
fn w64(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()))
}

/// One memory slot (SMBIOS type 17). None if the slot is empty.
pub fn parse_smbios_memory_device(raw: &[u8]) -> Option<Stick> {
    if raw.len() < 0x15 || raw[0] != 17 {
        return None;
    }
    let size = match w16(raw, 0x0C)? {
        0 | 0xFFFF => return None,
        0x7FFF => w32(raw, 0x1C)? as u64 * (1 << 20),
        s if s & 0x8000 != 0 => (s & 0x7FFF) as u64 * 1024,
        s => s as u64 * (1 << 20),
    };
    let kind = match raw.get(0x12).copied().unwrap_or(0) {
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "",
    };
    let speed = w16(raw, 0x20).filter(|s| *s > 0 && *s != 0xFFFF).or_else(|| w16(raw, 0x15)).filter(|s| *s > 0 && *s != 0xFFFF);
    Some(Stick {
        slot: smbios_string(raw, raw[0x10]),
        size,
        kind: kind.to_string(),
        speed_mhz: speed.map(|s| s as u32),
        maker: smbios_string(raw, raw.get(0x17).copied().unwrap_or(0)),
        part: smbios_string(raw, raw.get(0x1A).copied().unwrap_or(0)),
    })
}

/// The memory area of the main board (SMBIOS type 16): (most memory in bytes, slots).
pub fn parse_smbios_memory_array(raw: &[u8]) -> Option<(u64, u32)> {
    if raw.len() < 0x0F || raw[0] != 16 || raw[3] != 3 {
        return None; // only "system memory"
    }
    let kb = w32(raw, 0x07)?;
    let max = if kb == 0x8000_0000 { w64(raw, 0x0F)? } else { kb as u64 * 1024 };
    Some((max, w16(raw, 0x0D)? as u32))
}

// ---- disks (Linux and macOS) -----------------------------------------------
fn disks_from_system() -> Vec<DiskCheck> {
    let Ok(list) = hosts::list_disks() else { return vec![] };
    list.into_iter()
        .filter(|d| !d.id.contains("/zram") && !d.id.contains("/ram") && d.size > 0)
        .map(|d| {
            let short = d.id.rsplit('/').next().unwrap_or(&d.id).to_string();
            let mut c = DiskCheck { name: short.clone(), model: d.model.clone(), size: d.size, ..Default::default() };
            let stick = d.removable && !d.system;
            if hosts::IS_LINUX {
                let rotational = read(Path::new(&format!("/sys/block/{short}/queue/rotational"))).as_deref() == Some("1");
                c.kind = if short.starts_with("nvme") {
                    "NVMe SSD"
                } else if stick {
                    "USB / removable"
                } else if rotational {
                    "hard disk"
                } else {
                    "SSD"
                }
                .to_string();
                if !stick || short.starts_with("nvme") {
                    linux_disk_health(&d.id, &mut c);
                }
            } else {
                c.kind = if d.removable { "USB / removable" } else { "internal" }.to_string();
                if let Some(t) = run(&["diskutil", "info", "-plist", &d.id]) {
                    if let Some(s) = plist::Value::from_reader(std::io::Cursor::new(t.as_bytes())).ok().and_then(|v| v.into_dictionary()).and_then(|m| m.get("SMARTStatus").and_then(|v| v.as_string().map(String::from))) {
                        c.health = match s.as_str() {
                            "Verified" => Some("Good".to_string()),
                            "Failing" => Some("FAILING - back up your files now".to_string()),
                            _ => None,
                        };
                    }
                }
            }
            c
        })
        .collect()
}

fn linux_disk_health(dev: &str, c: &mut DiskCheck) {
    if dev.contains("nvme") {
        if let Some(buf) = nvme_log(dev) {
            apply_nvme(&parse_nvme_smart(&buf), c);
            return;
        }
    }
    if let Some(json) = run(&["smartctl", "-j", "-H", "-A", dev]) {
        if let Ok(v) = serde_json::from_str::<Value>(&json) {
            apply_smartctl(&v, c);
        }
    }
}

/// What an NVMe drive reports about itself (the standard 512-byte health page).
#[derive(Debug, PartialEq)]
pub struct NvmeSmart {
    pub critical_warning: u8,
    pub temp_c: i32,
    pub spare_pct: u8,
    pub used_pct: u8,
    pub hours: u64,
    pub written_tb: f64,
    pub media_errors: u64,
}

pub fn parse_nvme_smart(b: &[u8]) -> NvmeSmart {
    let kelvin = w16(b, 1).unwrap_or(0) as i32;
    NvmeSmart {
        critical_warning: b.first().copied().unwrap_or(0),
        temp_c: kelvin - 273,
        spare_pct: b.get(3).copied().unwrap_or(0),
        used_pct: b.get(5).copied().unwrap_or(0),
        hours: w64(b, 128).unwrap_or(0),
        written_tb: w64(b, 48).unwrap_or(0) as f64 * 512_000.0 / 1e12,
        media_errors: w64(b, 160).unwrap_or(0),
    }
}

fn apply_nvme(n: &NvmeSmart, c: &mut DiskCheck) {
    c.used_pct = Some(n.used_pct.min(100) as u32);
    c.hours = Some(n.hours);
    c.temp_c = (n.temp_c > -100 && n.temp_c < 200).then_some(n.temp_c);
    c.written_tb = Some(n.written_tb);
    c.health = Some(if n.critical_warning != 0 || n.media_errors > 0 {
        "Warning - the drive reports problems".to_string()
    } else if n.used_pct >= 90 {
        "Almost worn out".to_string()
    } else if n.used_pct >= 70 {
        "Getting old".to_string()
    } else {
        "Good".to_string()
    });
}

fn apply_smartctl(v: &Value, c: &mut DiskCheck) {
    if let Some(ok) = v["smart_status"]["passed"].as_bool() {
        c.health = Some(if ok { "Good".to_string() } else { "FAILING - back up your files now".to_string() });
    }
    c.temp_c = v["temperature"]["current"].as_i64().map(|t| t as i32);
    c.hours = v["power_on_time"]["hours"].as_u64();
    if let Some(n) = v.get("nvme_smart_health_information_log") {
        c.used_pct = n["percentage_used"].as_u64().map(|p| p.min(100) as u32);
    }
}

#[cfg(target_os = "linux")]
fn nvme_log(path: &str) -> Option<Vec<u8>> {
    use std::os::fd::AsRawFd;
    // struct nvme_admin_cmd from <linux/nvme_ioctl.h>: 72 bytes
    #[repr(C)]
    #[derive(Default)]
    struct AdminCmd {
        opcode: u8,
        flags: u8,
        rsvd1: u16,
        nsid: u32,
        cdw2: u32,
        cdw3: u32,
        metadata: u64,
        addr: u64,
        metadata_len: u32,
        data_len: u32,
        cdw10: u32,
        cdw11: u32,
        cdw12: u32,
        cdw13: u32,
        cdw14: u32,
        cdw15: u32,
        timeout_ms: u32,
        result: u32,
    }
    let f = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 512];
    // "get log page" 0x02 (health), the whole 512 bytes. Read only: it changes nothing on the drive.
    let mut cmd = AdminCmd { opcode: 0x02, nsid: 0xFFFF_FFFF, addr: buf.as_mut_ptr() as u64, data_len: 512, cdw10: (127 << 16) | 0x02, ..Default::default() };
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), 0xC048_4E41u32 as _, &mut cmd as *mut AdminCmd) };
    (rc == 0).then_some(buf)
}

#[cfg(not(target_os = "linux"))]
fn nvme_log(_path: &str) -> Option<Vec<u8>> {
    None
}

// ---- macOS ----------------------------------------------------------------
fn collect_mac() -> Report {
    let sysctl = |k: &str| run(&["sysctl", "-n", k]).map(|s| s.trim().to_string()).unwrap_or_default();
    let mut r = Report {
        cpu: Cpu {
            model: sysctl("machdep.cpu.brand_string"),
            cores: sysctl("hw.physicalcpu").parse().ok(),
            threads: sysctl("hw.logicalcpu").parse().ok(),
            max_mhz: sysctl("hw.cpufrequency_max").parse::<u64>().ok().map(|hz| (hz / 1_000_000) as u32),
        },
        ..Default::default()
    };
    r.ram.total = sysctl("hw.memsize").parse().unwrap_or(0);
    r.machine.os = format!("macOS {}", run(&["sw_vers", "-productVersion"]).unwrap_or_default().trim());
    if let Some(j) = run(&["system_profiler", "-json", "SPHardwareDataType", "SPDisplaysDataType"]).and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        let hw = &j["SPHardwareDataType"][0];
        r.machine.vendor = "Apple".into();
        r.machine.model = [text(&hw["machine_name"]), text(&hw["machine_model"])].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        r.machine.serial = text(&hw["serial_number"]);
        r.machine.bios = text(&hw["boot_rom_version"]);
        r.machine.gpus = list(&j["SPDisplaysDataType"]).into_iter().map(|g| text(&g["sppci_model"])).filter(|s| !s.is_empty()).collect();
        if r.cpu.model.is_empty() {
            r.cpu.model = [text(&hw["chip_type"]), text(&hw["cpu_type"])].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
        }
    }
    if let Some(t) = run(&["ioreg", "-rn", "AppleSmartBattery", "-a"]) {
        if let Some(b) = plist::Value::from_reader(std::io::Cursor::new(t.as_bytes())).ok().and_then(|v| v.into_array()).and_then(|a| a.into_iter().next()).and_then(|v| v.into_dictionary()).and_then(|d| parse_mac_battery(&d)) {
            r.batteries.push(b);
        }
    }
    r
}

/// The battery record of a Mac (`AppleSmartBattery`).
pub fn parse_mac_battery(d: &plist::Dictionary) -> Option<Battery> {
    let n = |k: &str| d.get(k).and_then(|v| v.as_signed_integer().or_else(|| v.as_unsigned_integer().map(|u| u as i64)));
    let design = n("DesignCapacity").filter(|v| *v > 0)?;
    // Apple silicon Macs put a percentage in MaxCapacity; the real number is in AppleRawMaxCapacity.
    let full = n("AppleRawMaxCapacity").or_else(|| n("NominalChargeCapacity")).or_else(|| n("MaxCapacity").filter(|v| *v > 150))?;
    let charging = d.get("IsCharging").and_then(|v| v.as_boolean()).unwrap_or(false);
    let plugged = d.get("ExternalConnected").and_then(|v| v.as_boolean()).unwrap_or(false);
    Some(Battery {
        name: "Battery".into(),
        maker: d.get("DeviceName").and_then(|v| v.as_string()).unwrap_or("").to_string(),
        status: if charging { "Charging" } else if plugged { "Plugged in" } else { "Discharging" }.to_string(),
        percent: n("CurrentCapacity").filter(|p| (0..=100).contains(p)).map(|p| p as u32),
        design: Some(design as f64),
        full: Some(full as f64),
        unit: "mAh",
        cycles: n("CycleCount").map(|c| c as u32),
    })
}

// ---- Windows --------------------------------------------------------------
const WINDOWS_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$cs = Get-CimInstance Win32_ComputerSystem
$bios = Get-CimInstance Win32_BIOS
$os = Get-CimInstance Win32_OperatingSystem
$disks = @(Get-PhysicalDisk | ForEach-Object { $r = $_ | Get-StorageReliabilityCounter; [pscustomobject]@{ Name = $_.FriendlyName; Size = $_.Size; Media = [string]$_.MediaType; Bus = [string]$_.BusType; Health = [string]$_.HealthStatus; Wear = $r.Wear; Temp = $r.Temperature; Hours = $r.PowerOnHours } })
[pscustomobject]@{
  Vendor = $cs.Manufacturer; Model = $cs.Model; Serial = $bios.SerialNumber
  Bios = ($bios.SMBIOSBIOSVersion + ' ' + $bios.ReleaseDate); Os = $os.Caption
  Cpu = @(Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed)
  Sticks = @(Get-CimInstance Win32_PhysicalMemory | Select-Object Capacity, Speed, SMBIOSMemoryType, Manufacturer, PartNumber, DeviceLocator)
  RamArray = @(Get-CimInstance Win32_PhysicalMemoryArray | Select-Object MaxCapacity, MemoryDevices)
  Gpus = @(Get-CimInstance Win32_VideoController | Select-Object Name)
  BatStatic = @(Get-CimInstance -Namespace root\wmi -ClassName BatteryStaticData | Select-Object DesignedCapacity, ManufactureName, DeviceName)
  BatFull = @(Get-CimInstance -Namespace root\wmi -ClassName BatteryFullChargedCapacity | Select-Object FullChargedCapacity)
  BatCycles = @(Get-CimInstance -Namespace root\wmi -ClassName BatteryCycleCount | Select-Object CycleCount)
  BatNow = @(Get-CimInstance -Namespace root\wmi -ClassName BatteryStatus | Select-Object RemainingCapacity, Charging, PowerOnline)
  Disks = $disks
} | ConvertTo-Json -Depth 5 -Compress
"#;

fn collect_windows() -> Report {
    match hosts::ps(WINDOWS_SCRIPT, false).ok().and_then(|t| serde_json::from_str::<Value>(t.trim()).ok()) {
        Some(v) => parse_windows(&v),
        None => Report { notes: vec!["Windows did not answer the questions (PowerShell failed).".into()], ..Default::default() },
    }
}

/// Understand PowerShell's answer (see WINDOWS_SCRIPT).
pub fn parse_windows(v: &Value) -> Report {
    let mut r = Report {
        machine: Machine {
            vendor: clean(&text(&v["Vendor"])),
            model: clean(&text(&v["Model"])),
            serial: clean(&text(&v["Serial"])),
            bios: text(&v["Bios"]),
            os: text(&v["Os"]),
            gpus: list(&v["Gpus"]).into_iter().map(|g| text(&g["Name"])).filter(|s| !s.is_empty()).collect(),
        },
        ..Default::default()
    };
    let cpus = list(&v["Cpu"]);
    r.cpu = Cpu {
        model: cpus.first().map(|c| text(&c["Name"])).unwrap_or_default(),
        cores: Some(cpus.iter().filter_map(|c| int(&c["NumberOfCores"])).sum::<u64>() as u32).filter(|c| *c > 0),
        threads: Some(cpus.iter().filter_map(|c| int(&c["NumberOfLogicalProcessors"])).sum::<u64>() as u32).filter(|c| *c > 0),
        max_mhz: cpus.first().and_then(|c| int(&c["MaxClockSpeed"])).map(|m| m as u32),
    };
    for s in list(&v["Sticks"]) {
        r.ram.sticks.push(Stick {
            slot: text(&s["DeviceLocator"]),
            size: int(&s["Capacity"]).unwrap_or(0),
            kind: match int(&s["SMBIOSMemoryType"]) {
                Some(0x18) => "DDR3",
                Some(0x1A) => "DDR4",
                Some(0x1E) => "LPDDR4",
                Some(0x22) => "DDR5",
                Some(0x23) => "LPDDR5",
                _ => "",
            }
            .to_string(),
            speed_mhz: int(&s["Speed"]).map(|m| m as u32),
            maker: clean(&text(&s["Manufacturer"])),
            part: clean(&text(&s["PartNumber"])),
        });
    }
    r.ram.total = r.ram.sticks.iter().map(|s| s.size).sum();
    let arrays = list(&v["RamArray"]);
    r.ram.max = Some(arrays.iter().filter_map(|a| int(&a["MaxCapacity"])).sum::<u64>() * 1024).filter(|m| *m > 0);
    r.ram.slots = Some(arrays.iter().filter_map(|a| int(&a["MemoryDevices"])).sum::<u64>() as u32).filter(|m| *m > 0);

    let (statics, fulls, cycles, nows) = (list(&v["BatStatic"]), list(&v["BatFull"]), list(&v["BatCycles"]), list(&v["BatNow"]));
    for (i, s) in statics.iter().enumerate() {
        let design = int(&s["DesignedCapacity"]).map(|m| m as f64 / 1000.0);
        let full = fulls.get(i).and_then(|f| int(&f["FullChargedCapacity"])).map(|m| m as f64 / 1000.0);
        let now = nows.get(i);
        let percent = match (now.and_then(|n| int(&n["RemainingCapacity"])), full) {
            (Some(rem), Some(f)) if f > 0.0 => Some((rem as f64 / 1000.0 / f * 100.0).round().min(100.0) as u32),
            _ => None,
        };
        r.batteries.push(Battery {
            name: format!("Battery {}", i + 1),
            maker: [text(&s["ManufactureName"]), text(&s["DeviceName"])].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" "),
            status: match now {
                Some(n) if n["Charging"].as_bool() == Some(true) => "Charging",
                Some(n) if n["PowerOnline"].as_bool() == Some(true) => "Plugged in",
                Some(_) => "Discharging",
                None => "",
            }
            .to_string(),
            percent,
            design,
            full,
            unit: "Wh",
            cycles: cycles.get(i).and_then(|c| int(&c["CycleCount"])).map(|c| c as u32).filter(|c| *c > 0),
        });
    }
    for d in list(&v["Disks"]) {
        let health = text(&d["Health"]);
        r.disks.push(DiskCheck {
            name: text(&d["Name"]),
            model: String::new(),
            size: int(&d["Size"]).unwrap_or(0),
            kind: match text(&d["Bus"]).as_str() {
                "USB" => "USB / removable".to_string(),
                _ => match text(&d["Media"]).as_str() {
                    "SSD" => "SSD".to_string(),
                    "HDD" => "hard disk".to_string(),
                    other => other.to_string(),
                },
            },
            health: match health.as_str() {
                "Healthy" => Some("Good".to_string()),
                "" => None,
                other => Some(format!("Warning - Windows says: {other}")),
            },
            used_pct: int(&d["Wear"]).map(|w| w.min(100) as u32),
            hours: int(&d["Hours"]),
            temp_c: int(&d["Temp"]).map(|t| t as i32).filter(|t| *t > 0),
            written_tb: None,
        });
    }
    r
}

// ---------------------------------------------------------------------------
// writing it out in plain words
// ---------------------------------------------------------------------------
fn row(label: &str, value: impl AsRef<str>) -> String {
    format!("  {label:<17}{}", value.as_ref())
}

/// "Good", "Getting weak" or "Worn out", from today's capacity in percent.
pub fn battery_verdict(health: f64) -> &'static str {
    if health >= 80.0 {
        "Good"
    } else if health >= 60.0 {
        "Getting weak"
    } else {
        "Worn out - a new battery is worth it"
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn render(r: &Report) -> Vec<String> {
    let mut o: Vec<String> = vec![];
    let m = &r.machine;

    o.push("BATTERY".into());
    if r.batteries.is_empty() {
        o.push("  No battery found (a desktop computer, or it is not reported).".into());
    }
    for b in &r.batteries {
        let title = if b.maker.is_empty() { b.name.clone() } else { format!("{}  ({})", b.name, b.maker) };
        o.push(format!("  {title}"));
        if let Some(h) = b.health() {
            o.push(row("  Health", format!("{:.0}%  {}", h.min(100.0), battery_verdict(h))));
        }
        if let (Some(d), Some(f)) = (b.design, b.full) {
            let p = |x: f64| if b.unit == "Wh" { format!("{x:.1} Wh") } else { format!("{x:.0} mAh") };
            o.push(row("  When new", p(d)));
            o.push(row("  Holds now", p(f)));
        }
        if let Some(c) = b.cycles {
            o.push(row("  Charge cycles", c.to_string()));
        }
        let now = [b.percent.map(|p| format!("{p}%")), Some(b.status.clone()).filter(|s| !s.is_empty())].into_iter().flatten().collect::<Vec<_>>().join(", ");
        if !now.is_empty() {
            o.push(row("  Right now", now));
        }
        if b.design.is_none() {
            o.push("    This battery does not report its size.".into());
        }
    }

    o.push(String::new());
    o.push("COMPUTER".into());
    let name = [m.vendor.as_str(), m.model.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    if !name.is_empty() {
        o.push(row("Model", name));
    }
    if !m.serial.is_empty() {
        o.push(row("Serial number", &m.serial));
    }
    if !m.bios.trim().is_empty() {
        o.push(row("BIOS", m.bios.trim()));
    }
    if !m.os.trim().is_empty() {
        o.push(row("System", m.os.trim()));
    }

    o.push(String::new());
    o.push("PROCESSOR".into());
    let c = &r.cpu;
    o.push(row("Name", if c.model.is_empty() { "unknown" } else { &c.model }));
    let parts = [c.cores.map(|n| format!("{n} cores")), c.threads.map(|n| format!("{n} threads")), c.max_mhz.map(|m| format!("up to {:.1} GHz", m as f64 / 1000.0))];
    let parts: Vec<String> = parts.into_iter().flatten().collect();
    if !parts.is_empty() {
        o.push(row("Size", parts.join(", ")));
    }

    o.push(String::new());
    o.push("MEMORY".into());
    // Without the stick list, the total is only what the system can use (a little less than is installed).
    let label = if r.ram.sticks.is_empty() && hosts::IS_LINUX { "Usable" } else { "Installed" };
    o.push(row(label, if r.ram.total > 0 { human(r.ram.total) } else { "unknown".into() }));
    if let (Some(max), Some(slots)) = (r.ram.max, r.ram.slots) {
        o.push(row("Board can take", format!("up to {} in {slots} slots", human(max))));
    }
    for s in &r.ram.sticks {
        let speed = s.speed_mhz.map(|m| format!("{m} MHz")).unwrap_or_default();
        let line = [human(s.size), s.kind.clone(), speed, s.maker.clone(), s.part.clone()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("  ");
        o.push(row(&format!("  {}", s.slot.chars().take(14).collect::<String>()), line));
    }

    if !m.gpus.is_empty() {
        o.push(String::new());
        o.push("GRAPHICS".into());
        for g in &m.gpus {
            o.push(format!("  {g}"));
        }
    }

    o.push(String::new());
    o.push("DISKS".into());
    if r.disks.is_empty() {
        o.push("  No disks found.".into());
    }
    for d in &r.disks {
        let name = [d.name.as_str(), d.model.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("  ");
        o.push(format!("  {name}  {}  {}", human(d.size), d.kind));
        let mut bits = vec![];
        if let Some(h) = &d.health {
            bits.push(format!("Health: {h}"));
        }
        if let Some(u) = d.used_pct {
            bits.push(format!("{u}% of its life used"));
        }
        if let Some(h) = d.hours {
            bits.push(format!("on for {} hours", thousands(h)));
        }
        if let Some(t) = d.temp_c {
            bits.push(format!("{t} C"));
        }
        if let Some(w) = d.written_tb {
            bits.push(format!("{w:.1} TB written"));
        }
        if bits.is_empty() {
            let why = if d.kind.starts_with("USB") {
                "health: not checked on removable sticks"
            } else {
                "health: not available (try admin rights)"
            };
            o.push(format!("    {why}"));
        } else {
            for chunk in bits.chunks(2) {
                o.push(format!("    {}", chunk.join("   ")));
            }
        }
    }

    for n in &r.notes {
        o.push(String::new());
        o.push(n.clone());
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_health_and_words() {
        let b = Battery { design: Some(50.0), full: Some(41.0), unit: "Wh", ..Default::default() };
        assert_eq!(b.health().map(|h| h.round()), Some(82.0));
        assert_eq!(battery_verdict(82.0), "Good");
        assert_eq!(battery_verdict(70.0), "Getting weak");
        assert!(battery_verdict(40.0).starts_with("Worn out"));
        assert_eq!(Battery::default().health(), None);
    }

    #[test]
    fn numbers_with_commas() {
        assert_eq!((thousands(5), thousands(5213), thousands(1234567)), ("5".into(), "5,213".into(), "1,234,567".into()));
    }

    #[test]
    fn cpu_cores_are_counted_once_per_core() {
        let t = "processor : 0\nmodel name : Test CPU @ 3GHz\nphysical id : 0\ncore id : 0\n\nprocessor : 1\nmodel name : Test CPU @ 3GHz\nphysical id : 0\ncore id : 0\n\nprocessor : 2\nmodel name : Test CPU @ 3GHz\nphysical id : 0\ncore id : 1\n\nprocessor : 3\nmodel name : Test CPU @ 3GHz\nphysical id : 0\ncore id : 1\n";
        let c = parse_cpuinfo(t);
        assert_eq!((c.model.as_str(), c.cores, c.threads), ("Test CPU @ 3GHz", Some(2), Some(4)));
    }

    #[test]
    fn graphics_cards_from_lspci() {
        let t = "00:02.0 \"VGA compatible controller\" \"Intel Corporation\" \"Raptor Lake-P\" -r04 \"X\" \"Y\"\n01:00.0 \"3D controller\" \"NVIDIA Corporation\" \"AD107M\" -ra1 \"A\" \"B\"\n00:14.0 \"USB controller\" \"Intel\" \"xHCI\" \"A\" \"B\"\n";
        assert_eq!(parse_lspci(t), vec!["Intel Corporation Raptor Lake-P", "NVIDIA Corporation AD107M"]);
    }

    /// A made-up SMBIOS "memory device" record, laid out like the real one.
    fn memory_device(size_mb: u16, kind: u8, speed: u16, strings: &[&str]) -> Vec<u8> {
        let mut r = vec![0u8; 0x22];
        r[0] = 17;
        r[1] = 0x22;
        r[0x0C..0x0E].copy_from_slice(&size_mb.to_le_bytes());
        r[0x10] = 1; // locator = string 1
        r[0x12] = kind;
        r[0x15..0x17].copy_from_slice(&speed.to_le_bytes());
        r[0x17] = 2; // maker = string 2
        r[0x1A] = 3; // part = string 3
        r[0x20..0x22].copy_from_slice(&speed.to_le_bytes());
        for s in strings {
            r.extend_from_slice(s.as_bytes());
            r.push(0);
        }
        r.push(0);
        r
    }

    #[test]
    fn memory_sticks_from_smbios() {
        let raw = memory_device(8192, 0x1A, 3200, &["DIMM A1", "Kingston", "KF3200C16"]);
        let s = parse_smbios_memory_device(&raw).unwrap();
        assert_eq!((s.slot.as_str(), s.size, s.kind.as_str(), s.speed_mhz), ("DIMM A1", 8 << 30, "DDR4", Some(3200)));
        assert_eq!((s.maker.as_str(), s.part.as_str()), ("Kingston", "KF3200C16"));
        assert_eq!(parse_smbios_memory_device(&memory_device(0, 0x1A, 0, &["A", "B", "C"])), None, "empty slot");
        assert_eq!(parse_smbios_memory_device(&[1, 2, 3]), None);
        let d5 = parse_smbios_memory_device(&memory_device(16384, 0x22, 5600, &["Slot 1", "X", "Y"])).unwrap();
        assert_eq!((d5.kind.as_str(), d5.size), ("DDR5", 16 << 30));
    }

    #[test]
    fn most_memory_the_board_takes() {
        let mut raw = vec![0u8; 0x0F];
        raw[0] = 16;
        raw[1] = 0x0F;
        raw[3] = 3;
        raw[0x07..0x0B].copy_from_slice(&(64u32 << 20).to_le_bytes()); // 64 GB in KB
        raw[0x0D..0x0F].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(parse_smbios_memory_array(&raw), Some((64 << 30, 2)));
        raw[3] = 5; // not system memory (for example a cache)
        assert_eq!(parse_smbios_memory_array(&raw), None);
    }

    #[test]
    fn nvme_health_page() {
        let mut b = vec![0u8; 512];
        b[1..3].copy_from_slice(&311u16.to_le_bytes()); // 38 C
        b[3] = 100;
        b[5] = 7;
        b[48..56].copy_from_slice(&24_000_000u64.to_le_bytes()); // 12.3 TB
        b[128..136].copy_from_slice(&5213u64.to_le_bytes());
        let n = parse_nvme_smart(&b);
        assert_eq!((n.temp_c, n.used_pct, n.hours), (38, 7, 5213));
        assert!((n.written_tb - 12.288).abs() < 0.001);
        let mut c = DiskCheck::default();
        apply_nvme(&n, &mut c);
        assert_eq!(c.health.as_deref(), Some("Good"));
        b[5] = 95;
        apply_nvme(&parse_nvme_smart(&b), &mut c);
        assert_eq!(c.health.as_deref(), Some("Almost worn out"));
        b[0] = 1; // critical warning
        apply_nvme(&parse_nvme_smart(&b), &mut c);
        assert!(c.health.unwrap().starts_with("Warning"));
    }

    #[test]
    fn smartctl_answer() {
        let v: Value = serde_json::from_str(r#"{"smart_status":{"passed":false},"temperature":{"current":41},"power_on_time":{"hours":900}}"#).unwrap();
        let mut c = DiskCheck::default();
        apply_smartctl(&v, &mut c);
        assert!(c.health.unwrap().starts_with("FAILING"));
        assert_eq!((c.temp_c, c.hours), (Some(41), Some(900)));
    }

    #[test]
    fn windows_answer_one_battery_given_as_a_single_object() {
        let v: Value = serde_json::from_str(
            r#"{"Vendor":"LENOVO","Model":"83LK","Serial":"PF123","Bios":"N3 2024","Os":"Microsoft Windows 11 Home",
            "Cpu":{"Name":"Intel Core i7","NumberOfCores":10,"NumberOfLogicalProcessors":12,"MaxClockSpeed":2400},
            "Sticks":[{"Capacity":8589934592,"Speed":5600,"SMBIOSMemoryType":34,"Manufacturer":"Samsung","PartNumber":"M425","DeviceLocator":"Controller0-ChannelA"}],
            "RamArray":{"MaxCapacity":67108864,"MemoryDevices":2},"Gpus":[{"Name":"Intel Iris Xe"}],
            "BatStatic":{"DesignedCapacity":50000,"ManufactureName":"SMP","DeviceName":"L22"},
            "BatFull":{"FullChargedCapacity":40000},"BatCycles":{"CycleCount":210},
            "BatNow":{"RemainingCapacity":20000,"Charging":false,"PowerOnline":false},
            "Disks":{"Name":"Samsung SSD","Size":512110190592,"Media":"SSD","Bus":"NVMe","Health":"Healthy","Wear":4,"Temp":35,"Hours":1200}}"#,
        )
        .unwrap();
        let r = parse_windows(&v);
        let b = &r.batteries[0];
        assert_eq!((b.design, b.full, b.cycles, b.percent), (Some(50.0), Some(40.0), Some(210), Some(50)));
        assert_eq!(b.health().map(|h| h.round()), Some(80.0));
        assert_eq!((r.ram.total, r.ram.max, r.ram.slots), (8 << 30, Some(64 << 30), Some(2)));
        assert_eq!(r.ram.sticks[0].kind, "DDR5");
        assert_eq!((r.cpu.cores, r.cpu.threads), (Some(10), Some(12)));
        assert_eq!((r.disks[0].health.as_deref(), r.disks[0].used_pct, r.disks[0].kind.as_str()), (Some("Good"), Some(4), "SSD"));
        let text = render(&r).join("\n");
        assert!(text.contains("80%") && text.contains("up to 64.0 GB in 2 slots") && text.contains("Intel Iris Xe"), "{text}");
    }

    #[test]
    fn mac_battery_on_apple_silicon_uses_the_real_capacity() {
        let mut d = plist::Dictionary::new();
        d.insert("DesignCapacity".into(), plist::Value::Integer(6075.into()));
        d.insert("MaxCapacity".into(), plist::Value::Integer(100.into())); // a percentage here
        d.insert("AppleRawMaxCapacity".into(), plist::Value::Integer(5400.into()));
        d.insert("CycleCount".into(), plist::Value::Integer(180.into()));
        d.insert("CurrentCapacity".into(), plist::Value::Integer(64.into()));
        d.insert("IsCharging".into(), plist::Value::Boolean(false));
        let b = parse_mac_battery(&d).unwrap();
        assert_eq!((b.full, b.cycles, b.percent), (Some(5400.0), Some(180), Some(64)));
        assert_eq!(b.health().map(|h| h.round()), Some(89.0));
    }

    /// A fake /sys and /proc tree, read the way a real one is.
    #[test]
    fn linux_files_become_a_report() {
        let root = std::env::temp_dir().join(format!("sycros-sysinfo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let w = |p: &str, v: &str| {
            let f = root.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, v).unwrap();
        };
        w("sys/class/power_supply/BAT0/type", "Battery\n");
        w("sys/class/power_supply/BAT0/energy_full_design", "50000000\n");
        w("sys/class/power_supply/BAT0/energy_full", "41000000\n");
        w("sys/class/power_supply/BAT0/cycle_count", "312\n");
        w("sys/class/power_supply/BAT0/capacity", "87\n");
        w("sys/class/power_supply/BAT0/status", "Discharging\n");
        w("sys/class/power_supply/BAT0/manufacturer", "SMP\n");
        w("sys/class/power_supply/hidpp_battery_0/type", "Battery\n"); // a mouse
        w("sys/class/power_supply/hidpp_battery_0/scope", "Device\n");
        w("sys/class/power_supply/ADP0/type", "Mains\n");
        w("sys/class/dmi/id/sys_vendor", "LENOVO\n");
        w("sys/class/dmi/id/product_name", "83LK\n");
        w("sys/class/dmi/id/product_serial", "To Be Filled By O.E.M.\n");
        w("proc/meminfo", "MemTotal:       16000000 kB\n");
        w("proc/cpuinfo", "processor : 0\nmodel name : Test CPU\n");
        w("etc/os-release", "PRETTY_NAME=\"Test Linux 1\"\n");
        let r = collect_linux(&root);
        assert_eq!(r.batteries.len(), 1, "the mouse is not a battery of this computer");
        let b = &r.batteries[0];
        assert_eq!((b.design, b.full, b.cycles, b.percent), (Some(50.0), Some(41.0), Some(312), Some(87)));
        assert_eq!(r.machine.vendor, "LENOVO");
        assert_eq!(r.machine.serial, "", "placeholder text is dropped");
        assert_eq!((r.ram.total, r.cpu.model.as_str()), (16_000_000 * 1024, "Test CPU"));
        let text = render(&r).join("\n");
        assert!(text.contains("82%") && text.contains("Good") && text.contains("312") && text.contains("Test Linux 1"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_report_without_a_battery_says_so() {
        let text = render(&Report::default()).join("\n");
        assert!(text.contains("No battery found") && text.contains("DISKS"), "{text}");
    }
}
