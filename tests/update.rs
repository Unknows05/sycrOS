//! The update's download, checks and program swap, using a fake release in a local folder
//! (no internet, and the installed program is never touched).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use sycros::{engine, update};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ASSET: &str = "test-program.exe";

struct Fake {
    root: PathBuf,
    base: String,
    target: PathBuf,
}

/// A "release" holding the real program + checksums, and an old program to replace.
fn fake(tag: &str) -> Fake {
    let root = std::env::temp_dir().join(format!("sycros-update-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("release")).unwrap();
    let asset = root.join("release").join(ASSET);
    fs::copy(env!("CARGO_BIN_EXE_sycros"), &asset).unwrap();
    let hash = engine::sha256_file(&asset).unwrap();
    fs::write(root.join("release/SHA256SUMS"), format!("{hash}  {ASSET}\n")).unwrap();
    let target = root.join("installed.exe");
    fs::write(&target, b"old program").unwrap();
    let path = root.join("release").display().to_string().replace('\\', "/");
    let base = format!("file://{}{}", if cfg!(windows) { "/" } else { "" }, path);
    Fake { root, base, target }
}

fn old_is_kept(p: &Path) -> bool {
    fs::read(p).unwrap() == b"old program"
}

#[test]
fn replaces_the_program_and_the_new_one_runs() {
    let f = fake("ok");
    update::install_program(&f.base, ASSET, VERSION, &f.target).unwrap();
    let out = Command::new(&f.target).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains(VERSION));
    fs::remove_dir_all(&f.root).unwrap();
}

#[test]
fn wrong_checksum_changes_nothing() {
    let f = fake("badsum");
    fs::write(f.root.join("release/SHA256SUMS"), format!("{}  {ASSET}\n", "0".repeat(64))).unwrap();
    let e = update::install_program(&f.base, ASSET, VERSION, &f.target).unwrap_err().to_string();
    assert!(e.contains("does not match"), "{e}");
    assert!(old_is_kept(&f.target));
    fs::remove_dir_all(&f.root).unwrap();
}

#[test]
fn program_reporting_another_version_changes_nothing() {
    let f = fake("badver");
    let e = update::install_program(&f.base, ASSET, "99.99.99", &f.target).unwrap_err().to_string();
    assert!(e.contains("did not start correctly"), "{e}");
    assert!(old_is_kept(&f.target));
    fs::remove_dir_all(&f.root).unwrap();
}

#[test]
fn file_missing_from_the_list_changes_nothing() {
    let f = fake("nosum");
    fs::write(f.root.join("release/SHA256SUMS"), "").unwrap();
    assert!(update::install_program(&f.base, ASSET, VERSION, &f.target).is_err());
    assert!(old_is_kept(&f.target));
    fs::remove_dir_all(&f.root).unwrap();
}
