//! The boot engine: Ventoy's prebuilt boot files.
//!
//! sycrOS writes the disk layout itself, but the code that boots ISO files (a
//! patched GRUB for BIOS + UEFI, with Secure Boot shim) comes from the official
//! Ventoy release. We download one pinned version, check its SHA-256, and keep
//! only the files we need in a cache folder.

use crate::Result;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

pub const BOOT_FILES_VERSION: &str = "1.1.18";
const TARBALL: &str = "ventoy-1.1.18-linux.tar.gz";
const URL: &str = "https://github.com/ventoy/Ventoy/releases/download/v1.1.18/ventoy-1.1.18-linux.tar.gz";
/// From https://github.com/ventoy/Ventoy/releases/download/v1.1.18/sha256.txt
const SHA256: &str = "d86ff9de63d94c8b8f1f6b14d23c55af4b88e8207bf2893890892a6a97b1ad54";

const WANTED: &[&str] = &[
    "boot/boot.img",
    "boot/core.img.xz",
    "ventoy/ventoy.disk.img.xz",
    "tool/x86_64/mkexfatfs.xz",
    "tool/aarch64/mkexfatfs.xz",
    "tool/i386/mkexfatfs.xz",
];
const REQUIRED: &[&str] = &["boot/boot.img", "boot/core.img.xz", "ventoy/ventoy.disk.img.xz"];

/// Ventoy's EFI partition is always 32 MiB.
pub const EFI_PART_SECTORS: u64 = 65536;
/// Sectors 34..2047 on a GPT disk.
const CORE_MAX_SECTORS: usize = 2014;

pub fn cache_root() -> PathBuf {
    if let Ok(p) = std::env::var("SYCROS_CACHE") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    let base = if cfg!(windows) {
        std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| Path::new(&home).join("AppData/Local"))
    } else if cfg!(target_os = "macos") {
        Path::new(&home).join("Library/Caches")
    } else {
        std::env::var("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|_| Path::new(&home).join(".cache"))
    };
    base.join("sycros")
}

pub struct Engine {
    pub folder: PathBuf,
}

fn xz_file(path: &Path) -> Result<Vec<u8>> {
    let mut out = vec![];
    lzma_rs::xz_decompress(&mut BufReader::new(File::open(path)?), &mut out)
        .map_err(|e| format!("could not unpack {}: {e:?}", path.display()))?;
    Ok(out)
}

impl Engine {
    pub fn version(&self) -> &'static str {
        BOOT_FILES_VERSION
    }

    fn file(&self, rel: &str) -> Result<PathBuf> {
        let p = self.folder.join(rel);
        if !p.exists() {
            return Err(format!("boot file missing: {} (run: sycros engine --refresh)", p.display()).into());
        }
        Ok(p)
    }

    pub fn boot_img(&self) -> Result<Vec<u8>> {
        let d = fs::read(self.file("boot/boot.img")?)?;
        if d.len() != 512 {
            return Err("boot.img has the wrong size".into());
        }
        Ok(d)
    }

    /// core.img, cut to the 2014 sectors that fit the GPT boot gap.
    /// The file is padded to 2047 sectors (MBR style); Ventoy writes only the
    /// first 2014 on GPT, so the cut-off tail must be empty padding.
    pub fn core_img(&self) -> Result<Vec<u8>> {
        let mut d = xz_file(&self.file("boot/core.img.xz")?)?;
        let keep = CORE_MAX_SECTORS * 512;
        if d.len() > keep {
            if d[keep..].iter().any(|&b| b != 0) {
                return Err("core.img is too big for the GPT boot gap".into());
            }
            d.truncate(keep);
        }
        Ok(d)
    }

    /// Decompressed 32 MiB EFI partition image (cached on disk).
    pub fn efi_img_path(&self) -> Result<PathBuf> {
        let out = self.folder.join("ventoy/ventoy.disk.img");
        if !out.exists() {
            let data = xz_file(&self.file("ventoy/ventoy.disk.img.xz")?)?;
            let tmp = out.with_extension("part");
            fs::write(&tmp, data)?;
            fs::rename(&tmp, &out)?;
        }
        Ok(out)
    }

    /// Ventoy's static exFAT formatter for Linux (used when mkfs.exfat is missing).
    pub fn mkexfatfs(&self) -> Option<PathBuf> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let sub = match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            "x86" => "i386",
            _ => return None,
        };
        let out = self.folder.join(format!("tool/{sub}/mkexfatfs"));
        if !out.exists() {
            let src = self.folder.join(format!("tool/{sub}/mkexfatfs.xz"));
            let data = xz_file(&src).ok()?;
            fs::write(&out, data).ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).ok()?;
            }
        }
        Some(out)
    }
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut h = Sha256::new();
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Return a ready Engine, downloading + verifying it the first time.
pub fn get_engine(refresh: bool, tarball: Option<&str>, quiet: bool) -> Result<Engine> {
    let folder = cache_root().join(format!("boot-{BOOT_FILES_VERSION}"));
    let marker = folder.join(".ok");
    if marker.exists() && !refresh {
        return Ok(Engine { folder });
    }
    fs::create_dir_all(&folder)?;

    let tar_path = match tarball {
        Some(t) => PathBuf::from(t),
        None => cache_root().join(TARBALL),
    };
    if tarball.is_none() && (refresh || !tar_path.exists()) {
        if !quiet {
            crate::ui::say(format!("Downloading boot files ({BOOT_FILES_VERSION}, ~20 MB) ..."));
        }
        let tmp = tar_path.with_extension("part");
        // curl ships with Linux, macOS and Windows 10+; it keeps sycrOS free of TLS code
        let tmp_s = tmp.to_string_lossy().to_string();
        let ok = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
            .args([if crate::ui::attached() { "-fsSL" } else { "-fL" }, "--retry", "3", "-o", &tmp_s, URL])
            .stdin(std::process::Stdio::null())
            .stdout(if crate::ui::attached() { std::process::Stdio::null() } else { std::process::Stdio::inherit() })
            .stderr(if crate::ui::attached() { std::process::Stdio::null() } else { std::process::Stdio::inherit() })
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            let _ = fs::remove_file(&tmp);
            return Err(format!(
                "download failed. Download it yourself:\n  {URL}\nthen run: sycros engine --tarball <that file>"
            )
            .into());
        }
        fs::rename(&tmp, &tar_path)?;
    }

    let got = sha256_file(&tar_path)?;
    if got != SHA256 {
        return Err(format!(
            "boot files checksum does not match!\n  expected {SHA256}\n  got      {got}\nDelete {} and try again.",
            tar_path.display()
        )
        .into());
    }

    let gz = flate2::read::GzDecoder::new(File::open(&tar_path)?);
    let mut ar = tar::Archive::new(gz);
    for entry in ar.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().trim_start_matches("./").to_string();
        let rel = match name.split_once('/') {
            Some((_top, rest)) => rest.to_string(),
            None => continue,
        };
        // only exact, known file names are written: no path tricks possible
        if !WANTED.contains(&rel.as_str()) || !entry.header().entry_type().is_file() {
            continue;
        }
        let dest = folder.join(&rel);
        fs::create_dir_all(dest.parent().unwrap())?;
        io::copy(&mut entry, &mut File::create(&dest)?)?;
    }
    for r in REQUIRED {
        if !folder.join(r).exists() {
            return Err(format!("boot files are incomplete, missing {r}").into());
        }
    }
    fs::write(&marker, BOOT_FILES_VERSION)?;
    if !quiet {
        crate::ui::say(format!("Boot files ready in {}", folder.display()));
    }
    Ok(Engine { folder })
}
