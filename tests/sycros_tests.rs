//! sycrOS tests. Run:  cargo test
//!
//! Most tests need no internet and no root. The full-disk test needs the Ventoy
//! boot engine (downloaded once, or SYCROS_TARBALL=path) and Linux with sgdisk/sfdisk.

mod common;

use common::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use sycros::gpt;
use sycros::library::{self, OsKind};
use sycros::macimg::{self, Fs, Kind};
use sycros::gpt::Disk as _;
use sycros::{engine, layout, rawdisk, udif};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let mut r = [0u8; 6];
        getrandom::getrandom(&mut r).unwrap();
        let name: String = r.iter().map(|b| format!("{b:02x}")).collect();
        let p = std::env::temp_dir().join(format!("sycros-test-{tag}-{name}"));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn file(&self, name: &str, data: &[u8]) -> String {
        let p = self.0.join(name);
        fs::write(&p, data).unwrap();
        p.to_string_lossy().into_owned()
    }
    fn s(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn check_vol(path: &str, workdir: &str, vol: &[u8], label: &str, kind: Kind) {
    let v = macimg::find_volume(path, Some(workdir)).unwrap();
    assert_eq!(v.fs, Fs::Hfs);
    assert_eq!(v.label, label);
    assert_eq!(v.length, vol.len() as u64);
    assert_eq!(v.kind(), kind);
    let data = fs::read(&v.source).unwrap();
    assert_eq!(&data[v.offset as usize..v.offset as usize + vol.len()], vol);
}

// ---------------------------------------------------------------------------
// macOS images
// ---------------------------------------------------------------------------
#[test]
fn plain_hfs_iso() {
    // mkmaciso-style ISO: HFS+ header at byte 1024
    let t = TempDir::new("iso");
    let vol = hfs_volume("Install macOS Sonoma", 2048);
    check_vol(&t.file("Sonoma.iso", &vol), t.s(), &vol, "Install macOS Sonoma", Kind::Installer);
}

#[test]
fn gpt_dmg_img() {
    // mkmaciso .dmg.img = raw GPT disk
    let t = TempDir::new("gpt");
    let vol = hfs_volume("Install macOS Sonoma", 2048);
    let img = gpt_disk(&vol, gpt::APPLE_HFS, "Install macOS Test");
    check_vol(&t.file("macOS_Sonoma.dmg.img", &img), t.s(), &vol, "Install macOS Sonoma", Kind::Installer);
}

#[test]
fn compressed_udif_dmg_with_lzfse() {
    let t = TempDir::new("udif");
    let vol = hfs_volume("Install macOS Sonoma", 2048);
    let dmg = udif(&gpt_disk(&vol, gpt::APPLE_HFS, ""), 256);
    let p = t.file("Sonoma.dmg", &dmg);
    assert!(udif::is_udif(&p));
    check_vol(&p, t.s(), &vol, "Install macOS Sonoma", Kind::Installer);
    // the temp unpacked file is gone again
    let left: Vec<_> = fs::read_dir(&t.0).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "raw")).collect();
    assert!(left.is_empty());
}

#[test]
fn apple_partition_map() {
    let t = TempDir::new("apm");
    let vol = hfs_volume("Install macOS Sonoma", 2048);
    check_vol(&t.file("old.cdr", &apm_disk(&vol)), t.s(), &vol, "Install macOS Sonoma", Kind::Installer);
}

#[test]
fn apfs_recovery() {
    let t = TempDir::new("apfs");
    let c = apfs_container(1024);
    let p = t.file("macOS Base System.img", &gpt_disk(&c, gpt::APPLE_APFS, ""));
    let v = macimg::find_volume(&p, Some(t.s())).unwrap();
    assert_eq!((v.fs, v.length, v.kind()), (Fs::Apfs, c.len() as u64, Kind::Recovery));
}

#[test]
fn recovery_kind() {
    let t = TempDir::new("rec");
    let vol = hfs_volume("macOS Base System", 2048);
    check_vol(&t.file("rec.iso", &vol), t.s(), &vol, "macOS Base System", Kind::Recovery);
}

#[test]
fn not_mac() {
    let t = TempDir::new("notmac");
    let mut iso = vec![0u8; 32768];
    iso.extend_from_slice(b"\x01CD001");
    iso.resize(40000, 0);
    let p = t.file("ubuntu.iso", &iso);
    assert!(!macimg::looks_like_mac(&p));
    assert!(macimg::find_volume(&p, None).is_err());
}

// ---------------------------------------------------------------------------
// library
// ---------------------------------------------------------------------------
#[test]
fn classify_by_name() {
    let t = TempDir::new("cls");
    for (name, want) in [
        ("Win11_24H2_English_x64.iso", OsKind::Windows),
        ("en-us_windows_server_2025.iso", OsKind::Windows),
        ("boot.wim", OsKind::Windows),
        ("ubuntu-24.04.1-desktop-amd64.iso", OsKind::Linux),
        ("archlinux-2026.10.01-x86_64.iso", OsKind::Linux),
        ("Fedora-Workstation-Live-41.iso", OsKind::Linux),
        ("memtest86-usb.img", OsKind::Tools),
        ("shell.efi", OsKind::Tools),
    ] {
        assert_eq!(library::classify(&t.file(name, b"")), want, "{name}");
    }
}

#[test]
fn classify_by_iso_label() {
    let t = TempDir::new("lbl");
    let mut iso = vec![0u8; 32768];
    let mut pvd = vec![0u8; 2048];
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[40..72].copy_from_slice(format!("{:<32}", "CCCOMA_X64FRE_EN-US_DV9").as_bytes());
    iso.extend_from_slice(&pvd);
    assert_eq!(library::classify(&t.file("x.iso", &iso)), OsKind::Windows);
}

#[test]
fn ventoy_json_merge_and_mac_menu() {
    let t = TempDir::new("cfg");
    let root = Path::new(t.s());
    fs::create_dir_all(root.join("ventoy")).unwrap();
    fs::write(root.join("ventoy/ventoy.json"), r#"{"control":[{"VTOY_DEFAULT_MENU_MODE":"0"}],"theme":{"x":1}}"#).unwrap();
    library::ensure_layout(root).unwrap();
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("ventoy/ventoy.json")).unwrap()).unwrap();
    assert_eq!(v["theme"]["x"], 1); // user settings kept
    let control = v["control"].as_array().unwrap();
    assert!(control.contains(&serde_json::json!({"VTOY_DEFAULT_MENU_MODE": "0"}))); // not overridden
    assert!(control.contains(&serde_json::json!({"VTOY_FILT_DOT_UNDERSCORE_FILE": "1"})));
    for d in ["Windows", "Linux", "Tools"] {
        assert!(root.join(d).is_dir());
    }

    let cfg = root.join("ventoy/ventoy_grub.cfg");
    fs::write(&cfg, "menuentry 'my own' { true }\n").unwrap();
    library::write_mac_menu(root, &[(3, "Install macOS Sonoma".into(), Fs::Hfs), (4, "Base".into(), Fs::Apfs)]).unwrap();
    let text = fs::read_to_string(&cfg).unwrap();
    assert!(text.contains("set root=(${vtoydev},gpt3)"));
    assert!(text.contains("my own") && text.contains("VTOY_RET"));
    library::write_mac_menu(root, &[]).unwrap();
    let text = fs::read_to_string(&cfg).unwrap();
    assert!(!text.contains("gpt3") && text.contains("my own"));
}

// ---------------------------------------------------------------------------
// full flow on an image file
// ---------------------------------------------------------------------------
fn sgdisk_ok(img: &str) -> serde_json::Value {
    let out = Command::new("sgdisk").args(["-v", img]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("No problems found"), "sgdisk: {text}");
    let j = Command::new("sfdisk").args(["--json", img]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&j.stdout).unwrap();
    v["partitiontable"]["partitions"].clone()
}

#[test]
fn full_flow_on_image() {
    if !cfg!(target_os = "linux") || Command::new("sgdisk").arg("--version").output().is_err() {
        eprintln!("skipped: needs Linux + sgdisk");
        return;
    }
    let eng = match engine::get_engine(false, std::env::var("SYCROS_TARBALL").ok().as_deref(), true) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("skipped: boot engine not available: {e}");
            return;
        }
    };
    let t = TempDir::new("flow");
    let img = t.0.join("stick.img").to_string_lossy().into_owned();
    {
        let mut d = layout::Disk::open(&img, Some(1 << 30), true, false).unwrap();
        layout::init(&mut d, &eng, 200 * layout::MIB, "sycrOS", false).unwrap();
    }
    let parts = sgdisk_ok(&img);
    assert_eq!(parts[0]["name"], "Ventoy");
    assert_eq!(parts[1]["name"], "VTOYEFI");
    assert_eq!(parts[1]["size"], 65536);
    assert_eq!(parts[1]["start"].as_u64().unwrap() % 8, 0);
    assert_eq!(parts[1]["attrs"], "GUID:63");
    let data = fs::read(&img).unwrap();
    assert_eq!((data[92], data[450], &data[510..512]), (0x22, 0xEE, &[0x55u8, 0xAA][..]));
    assert_eq!(data[34 * 512 + 500], 0x23);
    let p1 = parts[0]["start"].as_u64().unwrap() as usize * 512;
    assert_eq!(&data[p1 + 3..p1 + 11], b"EXFAT   ");
    // boot code and EFI partition are Ventoy's, byte for byte
    let boot = eng.boot_img().unwrap();
    assert_eq!(&data[..92], &boot[..92]);
    assert_eq!(&data[93..0x180], &boot[93..0x180]);
    let core = eng.core_img().unwrap();
    assert_eq!(&data[34 * 512..34 * 512 + 500], &core[..500]);
    assert_eq!(&data[34 * 512 + 501..34 * 512 + core.len()], &core[501..]);
    let p2 = parts[1]["start"].as_u64().unwrap() as usize * 512;
    assert!(data[p2..p2 + (32 << 20)] == fs::read(eng.efi_img_path().unwrap()).unwrap()[..]);
    drop(data);

    let vol = hfs_volume("Install macOS Tahoe", 4096); // 16 MiB
    let dmg = t.file("Tahoe.dmg", &udif(&gpt_disk(&vol, gpt::APPLE_HFS, ""), 256));
    let rec = hfs_volume("macOS Base System", 2048);
    let iso = t.file("Recovery.iso", &rec);
    {
        let mut d = layout::Disk::open(&img, None, true, false).unwrap();
        assert_eq!(layout::add_mac(&mut d, &dmg, None, Some(t.s()), false).unwrap(), 3);
        assert_eq!(layout::add_mac(&mut d, &iso, None, None, false).unwrap(), 4);
    }
    let parts = sgdisk_ok(&img);
    assert_eq!(parts[2]["type"], gpt::APPLE_HFS.to_string());
    assert_eq!(parts[2]["name"], "Install macOS Tahoe");
    assert_eq!(parts[3]["name"], "macOS Base System");
    let data = fs::read(&img).unwrap();
    let s3 = parts[2]["start"].as_u64().unwrap() as usize * 512;
    let s4 = parts[3]["start"].as_u64().unwrap() as usize * 512;
    assert!(data[s3..s3 + vol.len()] == vol[..]);
    assert!(data[s4..s4 + rec.len()] == rec[..]);
    drop(data);

    // too big for the space left -> clear error, table unchanged
    let big = t.file("big.iso", &hfs_volume("Install macOS Huge", 60000));
    {
        let mut d = layout::Disk::open(&img, None, true, false).unwrap();
        let e = layout::add_mac(&mut d, &big, None, None, false).unwrap_err();
        assert!(e.to_string().contains("Not enough macOS space left"));
    }
    assert_eq!(sgdisk_ok(&img).as_array().unwrap().len(), 4);

    {
        let mut d = layout::Disk::open(&img, None, true, false).unwrap();
        layout::remove_mac(&mut d, 3, false).unwrap();
        assert_eq!(layout::add_mac(&mut d, &iso, Some("Recovery again"), None, false).unwrap(), 3);
    }
    let parts = sgdisk_ok(&img);
    assert!(parts.as_array().unwrap().iter().any(|p| p["name"] == "Recovery again"));
}

#[test]
fn clear_message_when_stick_has_no_mac_space() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Ok(eng) = engine::get_engine(false, std::env::var("SYCROS_TARBALL").ok().as_deref(), true) else {
        eprintln!("skipped: boot engine not available");
        return;
    };
    let t = TempDir::new("nomac");
    let img = t.0.join("stick.img").to_string_lossy().into_owned();
    {
        let mut d = layout::Disk::open(&img, Some(512 << 20), true, false).unwrap();
        layout::init(&mut d, &eng, 0, "sycrOS", false).unwrap();
    }
    let iso = t.file("Sequoia.iso", &hfs_volume("Install macOS Sequoia", 1024));
    let mut d = layout::Disk::open(&img, None, true, false).unwrap();
    let e = layout::add_mac(&mut d, &iso, None, None, false).unwrap_err().to_string();
    assert!(e.contains("no macOS space") && e.contains("Prepare it again"), "{e}");
}

/// A real HFS+ volume must end exactly where its partition ends: the backup header
/// has to be in the last 1024 bytes (Linux refuses the volume otherwise, and this
/// is the suspected cause of the globe on the Mac). The size here is not a whole MiB.
#[test]
fn section_is_exactly_the_volume_size() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Ok(eng) = engine::get_engine(false, std::env::var("SYCROS_TARBALL").ok().as_deref(), true) else {
        eprintln!("skipped: boot engine not available");
        return;
    };
    let t = TempDir::new("exact");
    let img = t.0.join("stick.img").to_string_lossy().into_owned();
    let vol = hfs_volume("Install macOS Sequoia", 2053); // 8 MiB + 20 KiB
    let second = hfs_volume("Install macOS Mojave", 1030);
    assert_ne!(vol.len() as u64 % layout::MIB, 0);
    let (a, b) = (t.file("a.iso", &vol), t.file("b.iso", &second));
    let mut d = layout::Disk::open(&img, Some(512 << 20), true, false).unwrap();
    layout::init(&mut d, &eng, 64 * layout::MIB, "sycrOS", false).unwrap();
    layout::add_mac(&mut d, &a, None, None, false).unwrap();
    layout::add_mac(&mut d, &b, None, None, false).unwrap();
    let g = d.read_gpt().unwrap();
    drop(d);
    let data = fs::read(&img).unwrap();
    for (n, v) in [(3, &vol), (4, &second)] {
        let p = g.get(n).unwrap();
        assert_eq!(p.size(), v.len() as u64, "partition {n} must equal the volume");
        assert_eq!(p.first % 2048, 0, "the start stays aligned to 1 MiB");
        let end = (p.last as usize + 1) * 512;
        assert_eq!(&data[end - 1024..end - 1022], b"H+", "backup header at the end of partition {n}");
    }
}

/// Fedora-style ISO: gigabytes of ISO with a tiny Mac part inside. Not a macOS image.
#[test]
fn linux_iso_with_small_mac_part_is_not_macos() {
    let t = TempDir::new("hybrid");
    let tiny = hfs_volume("Fedora-Live", 256); // 1 MiB
    let mut iso = gpt_disk(&tiny, gpt::APPLE_HFS, "");
    iso.resize(64 << 20, 0);
    iso[32768..32774].copy_from_slice(b"\x01CD001");
    let p = t.file("Fedora-Workstation-Live.iso", &iso);
    assert!(!macimg::looks_like_mac(&p));
    assert_eq!(library::classify(&p), OsKind::Linux);
    // an image that really is macOS is still recognised under any name
    let real = hfs_volume("Install macOS Sonoma", 2048);
    assert!(macimg::looks_like_mac(&t.file("Sonoma.iso", &real)));
    assert!(macimg::looks_like_mac(&t.file("x.img", &gpt_disk(&real, gpt::APPLE_HFS, ""))));
}

/// The check after writing must notice damaged data.
#[test]
fn check_after_writing_catches_bad_data() {
    let t = TempDir::new("verify");
    let src = t.file("src.bin", &vec![7u8; 5 << 20]);
    let img = t.file("stick.img", &vec![0u8; 8 << 20]);
    let mut raw = rawdisk::Target::open(&img, 8 << 20, true, true).unwrap();
    raw.copy_from_file(&src, 0, 5 << 20, 1 << 20, "w").unwrap();
    raw.verify_from_file(&src, 0, 5 << 20, 1 << 20, "c").unwrap();
    raw.write_at((3 << 20) + 5, &[9]).unwrap();
    assert!(raw.verify_from_file(&src, 0, 5 << 20, 1 << 20, "c").is_err());
    drop(raw);

    let copied = library::copy_file(&src, &t.0, OsKind::Tools).unwrap();
    assert_eq!(fs::read(&copied).unwrap().len(), 5 << 20);
    rawdisk::verify_files(Path::new(&src), &copied, "c").unwrap();
    let mut bad = fs::read(&copied).unwrap();
    bad[100] = 9;
    fs::write(&copied, bad).unwrap();
    assert!(rawdisk::verify_files(Path::new(&src), &copied, "c").is_err());
}
