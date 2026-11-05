//! The data partition: OS folders, Ventoy menu settings and the Mac menu.
//!
//! ```text
//! /Windows/   Windows installers, WinPE, .wim/.vhd(x)
//! /Linux/     Linux ISOs
//! /Tools/     everything else (rescue, memtest, ...)
//! /ventoy/ventoy.json       menu settings (tree view = one menu per OS folder)
//! /ventoy/ventoy_grub.cfg   extra menu (F6): macOS slots for Intel Macs
//! ```

use crate::macimg::Fs;
use crate::rawdisk::{verify_files, Progress};
use crate::Result;
use regex::Regex;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OsKind {
    Windows,
    Linux,
    Tools,
    Mac,
}

impl OsKind {
    pub fn folder(&self) -> &'static str {
        match self {
            OsKind::Windows => "Windows",
            OsKind::Linux => "Linux",
            OsKind::Tools => "Tools",
            OsKind::Mac => "macOS",
        }
    }
}

pub const FOLDERS: [&str; 3] = ["Windows", "Linux", "Tools"];
const BOOTABLE_EXT: [&str; 7] = [".iso", ".wim", ".img", ".vhd", ".vhdx", ".efi", ".vtoy"];

const MAC_BEGIN: &str = "# >>> sycrOS macOS entries (generated, do not edit between these lines)";
const MAC_END: &str = "# <<< sycrOS macOS entries";

fn win_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)(^|[^a-z])(win(dows)?|win1[01]|win[78]|server20\d\d|winpe|hiren|cc[cs][ao]|[a-z0-9]*x64fre|x86fre|esd-iso|tiny1[01]|atlas|revios)",
        )
        .unwrap()
    })
}

fn linux_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)ubuntu|debian|fedora|arch|mint|kali|manjaro|suse|centos|rocky|alma|rhel|pop[-_]?os|elementary|zorin|endeavour|cachy|nixos|gentoo|void|alpine|mx[-_]|tails|proxmox|linux|garuda|bazzite|slackware|steamos|parrot|deepin|solus|nobara|kubuntu|xubuntu|lubuntu|bluefin|vanilla|clonezilla|gparted|systemrescue|freebsd|truenas|opnsense|pfsense|chromeos|fydeos",
        )
        .unwrap()
    })
}

/// Volume label of an ISO 9660 image ("" if none).
pub fn iso_label(path: &str) -> String {
    let Ok(mut f) = File::open(path) else { return String::new() };
    let mut pvd = [0u8; 2048];
    if f.seek(SeekFrom::Start(16 * 2048)).is_err() || f.read_exact(&mut pvd).is_err() {
        return String::new();
    }
    if &pvd[1..6] == b"CD001" {
        String::from_utf8_lossy(&pvd[40..72]).trim().to_string()
    } else {
        String::new()
    }
}

/// Windows / Linux / Tools for a bootable file.
pub fn classify(path: &str) -> OsKind {
    let low = path.to_lowercase();
    if low.ends_with(".wim") || low.ends_with(".vhd") || low.ends_with(".vhdx") {
        return OsKind::Windows;
    }
    if low.ends_with(".efi") {
        return OsKind::Tools;
    }
    let name = Path::new(path).file_name().unwrap_or_default().to_string_lossy();
    let text = format!("{} {}", name, iso_label(path));
    if win_re().is_match(&text) {
        OsKind::Windows
    } else if linux_re().is_match(&text) {
        OsKind::Linux
    } else {
        OsKind::Tools
    }
}

/// Make the OS folders and merge our settings into /ventoy/ventoy.json
/// (keys the user already set are never changed).
pub fn ensure_layout(root: &Path) -> Result<()> {
    for name in FOLDERS {
        fs::create_dir_all(root.join(name))?;
    }
    fs::create_dir_all(root.join("ventoy"))?;
    let cfg = root.join("ventoy/ventoy.json");
    let existed = cfg.exists();
    let mut data: Value = if existed {
        match fs::read_to_string(&cfg).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            Some(v) => v,
            None => {
                crate::ui::say(format!("  note: {} is not valid JSON, leaving it alone", cfg.display()));
                return Ok(());
            }
        }
    } else {
        json!({})
    };
    let Some(obj) = data.as_object_mut() else { return Ok(()) };
    let control = obj.entry("control").or_insert_with(|| json!([]));
    let Some(list) = control.as_array_mut() else { return Ok(()) };
    let have: Vec<String> =
        list.iter().filter_map(|i| i.as_object()).flat_map(|o| o.keys().cloned()).collect();
    let mut changed = !existed;
    for (k, v) in [
        ("VTOY_DEFAULT_MENU_MODE", "1"),        // tree view: Windows / Linux / Tools menus
        ("VTOY_FILT_DOT_UNDERSCORE_FILE", "1"), // hide macOS "._" junk files
    ] {
        if !have.iter().any(|h| h == k) {
            list.push(json!({ k: v }));
            changed = true;
        }
    }
    if changed {
        let mut text = serde_json::to_string_pretty(&data)?;
        text.push('\n');
        fs::write(&cfg, text)?;
    }
    Ok(())
}

/// Copy a file into its OS folder, with progress. Returns the new path.
pub fn copy_file(src: &str, root: &Path, kind: OsKind) -> Result<PathBuf> {
    let folder = root.join(kind.folder());
    fs::create_dir_all(&folder)?;
    let name = Path::new(src).file_name().ok_or("bad file name")?;
    let dest = folder.join(name);
    let tmp = folder.join(format!("{}.sycros-part", name.to_string_lossy()));
    // The file only gets its real name after it was copied AND checked.
    match copy_and_check(Path::new(src), &tmp, kind) {
        Ok(()) => {
            fs::rename(&tmp, &dest)?;
            Ok(dest)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn copy_and_check(src: &Path, tmp: &Path, kind: OsKind) -> Result<()> {
    let size = fs::metadata(src)?.len();
    let mut fi = File::open(src)?;
    let mut fo = File::create(tmp)?;
    let mut buf = vec![0u8; 8 << 20];
    let mut done = 0u64;
    let mut p = Progress::new(&format!("copying to {}/", kind.folder()), size);
    loop {
        let n = fi.read(&mut buf)?;
        if n == 0 {
            break;
        }
        fo.write_all(&buf[..n])?;
        done += n as u64;
        p.update(done);
    }
    fo.sync_all()?;
    drop(fo);
    p.finish();
    verify_files(src, tmp, "checking copy")?;
    Ok(())
}

fn is_bootable(name: &str) -> bool {
    let low = name.to_lowercase();
    !name.starts_with("._") && BOOTABLE_EXT.iter().any(|e| low.ends_with(e))
}

/// (folder, file name, size) of bootable files on the data partition.
pub fn list_files(root: &Path) -> Vec<(String, String, u64)> {
    let mut out = vec![];
    let mut scan = |dir: PathBuf, label: &str| {
        let Ok(rd) = fs::read_dir(&dir) else { return };
        let mut items: Vec<_> = rd.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            let name = e.file_name().to_string_lossy().to_string();
            if let Ok(m) = e.metadata() {
                if m.is_file() && is_bootable(&name) {
                    out.push((label.to_string(), name, m.len()));
                }
            }
        }
    };
    for f in FOLDERS {
        scan(root.join(f), f);
    }
    scan(root.to_path_buf(), "/");
    out
}

/// Rewrite the macOS part of ventoy_grub.cfg (the F6 menu).
/// `slots` = (partition number, label, fs). GRUB can start HFS+ volumes;
/// it cannot read APFS, so APFS slots only get a hint entry.
pub fn write_mac_menu(root: &Path, slots: &[(usize, String, Fs)]) -> Result<()> {
    let path = root.join("ventoy/ventoy_grub.cfg");
    let old = fs::read_to_string(&path).unwrap_or_default();
    let re = Regex::new(&format!(r"(?s){}.*?{}\n?", regex::escape(MAC_BEGIN), regex::escape(MAC_END)))?;
    let mut keep = re.replace_all(&old, "").to_string();

    if slots.is_empty() && keep.trim().is_empty() {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let mut block = String::new();
    if !slots.is_empty() {
        block.push_str(MAC_BEGIN);
        block.push('\n');
        for (num, label, fs) in slots {
            let safe = label.replace('"', "'");
            match fs {
                Fs::Hfs => block.push_str(&format!(
                    "menuentry \"macOS: {safe}  (Intel Mac only)\" --class=macOS {{\n    insmod part_gpt\n    insmod hfsplus\n    insmod chain\n    set root=(${{vtoydev}},gpt{num})\n    chainloader /System/Library/CoreServices/boot.efi\n}}\n"
                )),
                Fs::Apfs => block.push_str(&format!(
                    "menuentry \"macOS: {safe}  (APFS - use the Mac boot menu: hold Option)\" --class=macOS {{\n    echo \"This macOS volume is APFS. Restart and hold the Option key to start it.\"\n    sleep 5\n}}\n"
                )),
            }
        }
        block.push_str(MAC_END);
        block.push('\n');
    }
    if !keep.contains("VTOY_RET") {
        keep.push_str("menuentry '<-- Return to previous menu [Esc]' --class=vtoyret VTOY_RET {\n    echo 'Return ...'\n}\n");
    }
    fs::write(&path, block + &keep)?;
    Ok(())
}
