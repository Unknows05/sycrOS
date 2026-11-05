//! Updating sycrOS like normal software: look for a newer release on GitHub,
//! then download it, check its SHA-256 and replace the program.
//! Uses the system's `curl`, like the boot engine download.

use crate::{engine, hosts, ui, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const RELEASES: &str = "https://github.com/Unknows05/sycrOS/releases";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn curl() -> Command {
    let mut c = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    c.stdin(Stdio::null());
    c
}

/// "0.3.1" -> [0, 3, 1]
fn parts(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    parts(candidate) > parts(current)
}

/// ".../releases/tag/v0.3.1" -> "0.3.1"
pub fn version_from_url(url: &str) -> Option<String> {
    let (_, tag) = url.trim().rsplit_once("/tag/")?;
    let v = tag.trim_start_matches('v');
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| v.to_string())
}

/// The newest released version. One small request with a 5 second limit.
/// GitHub redirects "latest" to the newest tag, so no data needs downloading.
pub fn latest_version() -> Result<String> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let out = curl()
        .args(["-fsIL", "--max-time", "5", "-o", null, "-w", "%{url_effective}", &format!("{RELEASES}/latest")])
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    let url = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err("Could not reach GitHub. Check your internet connection.".into());
    }
    version_from_url(&url).ok_or_else(|| "GitHub did not say what the newest version is.".into())
}

/// A newer version, if there is one.
pub fn newer_version() -> Result<Option<String>> {
    let latest = latest_version()?;
    Ok(is_newer(&latest, CURRENT).then_some(latest))
}

/// The release file holding the plain program for this computer.
pub fn program_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("sycros-linux-amd64"),
        ("windows", "x86_64") => Some("sycros-windows-x64.exe"),
        ("macos", "aarch64") => Some("sycros-macos-apple-silicon"),
        ("macos", "x86_64") => Some("sycros-macos-intel"),
        _ => None,
    }
}

/// The checksum for `name` in a SHA256SUMS file.
pub fn checksum_for<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then_some(hash)
    })
}

/// A new private folder for downloads. It must not already exist, so nobody
/// else can place files in it first.
fn private_folder() -> Result<PathBuf> {
    let mut r = [0u8; 8];
    getrandom::getrandom(&mut r).map_err(|e| format!("random: {e}"))?;
    let tag: String = r.iter().map(|b| format!("{b:02x}")).collect();
    let dir = std::env::temp_dir().join(format!("sycros-update-{tag}"));
    std::fs::create_dir(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let ok = curl()
        .args(["-fsSL", "--retry", "2", "--max-time", "300", "-o"])
        .arg(dest)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(format!("Download failed: {url}").into())
    }
}

/// Download `asset` and its checksum list into `dir`; refuse a file that does not match.
fn fetch_checked(base: &str, asset: &str, dir: &Path) -> Result<PathBuf> {
    let sums_path = dir.join("SHA256SUMS");
    download(&format!("{base}/SHA256SUMS"), &sums_path)?;
    let sums = std::fs::read_to_string(&sums_path)?;
    let want = checksum_for(&sums, asset).ok_or_else(|| format!("{asset} is not in the release's checksum list."))?;
    let file = dir.join(asset);
    ui::say(format!("Downloading {asset} ..."));
    download(&format!("{base}/{asset}"), &file)?;
    let got = engine::sha256_file(&file)?;
    if !got.eq_ignore_ascii_case(want) {
        return Err("The downloaded file does not match its checksum. Nothing was changed. Try again later.".into());
    }
    ui::say(format!("Checked the download's fingerprint: {}...", &got[..16]));
    Ok(file)
}

/// Download the program from `base`, check it, and put it at `target`.
/// The old program stays in place unless every check passes.
pub fn install_program(base: &str, asset: &str, version: &str, target: &Path) -> Result<()> {
    let dir = private_folder()?;
    let result = (|| -> Result<()> {
        let new = fetch_checked(base, asset, &dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
        }
        // It must start and report the right version before it replaces anything.
        let out = Command::new(&new).arg("--version").stdin(Stdio::null()).output();
        let said = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        if !said.contains(version) {
            return Err("The downloaded program did not start correctly. Nothing was changed.".into());
        }
        swap_in(&new, target)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn old_copy(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Replace the program file. Works while it is running: Unix keeps the old file open,
/// and Windows lets a running program be renamed (the `.old` copy is removed next time).
fn swap_in(new: &Path, target: &Path) -> Result<()> {
    let mut staged = target.as_os_str().to_owned();
    staged.push(".new");
    let staged = PathBuf::from(staged);
    let denied = |e: std::io::Error| -> crate::Error {
        let how = if hosts::IS_WIN { "open sycrOS as Administrator" } else { "use: sudo sycros" };
        format!("Could not replace {} ({e}). If it needs admin rights, {how}.", target.display()).into()
    };
    std::fs::copy(new, &staged).map_err(denied)?;
    if cfg!(windows) {
        let old = old_copy(target);
        let _ = std::fs::remove_file(&old);
        if let Err(e) = std::fs::rename(target, &old) {
            let _ = std::fs::remove_file(&staged);
            return Err(denied(e));
        }
    }
    if let Err(e) = std::fs::rename(&staged, target) {
        let _ = std::fs::remove_file(&staged);
        return Err(denied(e));
    }
    Ok(())
}

/// Remove what a Windows update left behind.
pub fn cleanup() {
    if cfg!(windows) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::fs::remove_file(old_copy(&exe));
        }
    }
}

/// Installed by the .deb package: let apt replace it, so apt keeps knowing the version.
fn installed_by_deb(exe: &Path) -> bool {
    hosts::IS_LINUX
        && exe == Path::new("/usr/bin/sycros")
        && Command::new("dpkg")
            .args(["-S", "/usr/bin/sycros"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

fn update_with_apt(base: &str) -> Result<()> {
    if !hosts::is_admin() {
        return Err("Updating needs admin rights: use sudo sycros.".into());
    }
    let dir = private_folder()?;
    let result = (|| -> Result<()> {
        let deb = fetch_checked(base, "sycros-linux-amd64.deb", &dir)?;
        ui::say("Installing ...");
        let path = deb.to_string_lossy().to_string();
        hosts::run(&["apt-get", "install", "-y", &path], true).map(|_| ())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The whole update, for the `update` command and the menus.
pub fn update() -> Result<()> {
    ui::say("Looking for a newer version ...");
    let Some(version) = newer_version()? else {
        ui::say(format!("You already have the newest version ({CURRENT})."));
        return Ok(());
    };
    ui::say(format!("New version: {version} (you have {CURRENT})"));
    let base = format!("{RELEASES}/download/v{version}");
    let exe = std::env::current_exe()?.canonicalize()?;
    if installed_by_deb(&exe) {
        update_with_apt(&base)?;
    } else {
        let asset = program_asset().ok_or_else(|| format!("No ready-made program for this computer. See {RELEASES}"))?;
        install_program(&base, asset, &version, &exe)?;
    }
    ui::say(format!("Updated to {version}. Close sycrOS and open it again to use the new version."));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_as_numbers() {
        assert!(is_newer("0.3.1", "0.3.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
    }

    #[test]
    fn reads_the_version_from_the_redirect() {
        assert_eq!(version_from_url("https://github.com/a/b/releases/tag/v0.3.1").as_deref(), Some("0.3.1"));
        assert_eq!(version_from_url("https://github.com/a/b/releases"), None);
        assert_eq!(version_from_url("https://x/tag/v1;rm -rf"), None);
    }

    #[test]
    fn finds_checksums_by_exact_file_name() {
        let sums = "aaa  sycros-linux-amd64\nbbb  sycros-linux-amd64.deb\nccc *sycros-windows-x64.exe\n";
        assert_eq!(checksum_for(sums, "sycros-linux-amd64"), Some("aaa"));
        assert_eq!(checksum_for(sums, "sycros-linux-amd64.deb"), Some("bbb"));
        assert_eq!(checksum_for(sums, "sycros-windows-x64.exe"), Some("ccc"));
        assert_eq!(checksum_for(sums, "other"), None);
    }
}
