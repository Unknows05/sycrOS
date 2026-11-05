//! Find the bootable macOS volume inside a .dmg / .iso / .img file.
//!
//! Handles: plain HFS+ volume (header "H+"/"HX" at byte 1024, e.g. mkmaciso ISOs),
//! APFS container ("NXSB" at byte 32), GPT disk images (mkmaciso .dmg.img, hdiutil UDRW),
//! Apple Partition Map (old hybrid CDs), MBR type 0xAF, and compressed UDIF .dmg.

use crate::gpt::{self, Gpt, Guid};
use crate::{udif, Result};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fs {
    Hfs,
    Apfs,
}

impl Fs {
    pub fn label(&self) -> &'static str {
        match self {
            Fs::Hfs => "HFS+",
            Fs::Apfs => "APFS",
        }
    }
    pub fn gpt_type(&self) -> Guid {
        match self {
            Fs::Hfs => gpt::APPLE_HFS,
            Fs::Apfs => gpt::APPLE_APFS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Installer,
    Recovery,
    MacOs,
}

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Installer => "installer",
            Kind::Recovery => "recovery",
            Kind::MacOs => "macos",
        }
    }
}

#[derive(Debug)]
pub struct MacVolume {
    /// File holding the raw bytes (maybe a temp unpacked copy).
    pub source: PathBuf,
    pub offset: u64,
    pub length: u64,
    pub fs: Fs,
    pub label: String,
    /// True if `source` is our temp file (deleted on drop).
    pub temp: bool,
}

impl MacVolume {
    /// installer / recovery / macos (best guess from the name).
    pub fn kind(&self) -> Kind {
        let low = self.label.to_lowercase();
        if low.contains("install") {
            Kind::Installer
        } else if low.contains("base system") || low.contains("recovery") || low.contains("basesystem") {
            Kind::Recovery
        } else {
            Kind::MacOs
        }
    }
}

impl Drop for MacVolume {
    fn drop(&mut self) {
        if self.temp {
            let _ = fs::remove_file(&self.source);
        }
    }
}

fn read(f: &mut File, off: u64, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    if f.seek(SeekFrom::Start(off)).is_err() {
        return vec![];
    }
    let mut got = 0;
    while got < n {
        match f.read(&mut buf[got..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => got += k,
        }
    }
    buf.truncate(got);
    buf
}

fn be16(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}
fn be32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_be_bytes(s.try_into().unwrap()))
}

/// (length, label) if an HFS+ volume starts at `off`.
pub fn hfs_info(f: &mut File, off: u64) -> Option<(u64, String)> {
    let h = read(f, off + 1024, 512);
    if h.len() < 512 || !(h.starts_with(b"H+") || h.starts_with(b"HX")) {
        return None;
    }
    let block_size = be32(&h, 40)?;
    let total_blocks = be32(&h, 44)?;
    if block_size < 512 || !block_size.is_power_of_two() || total_blocks == 0 {
        return None;
    }
    let label = hfs_label(f, off, &h, block_size).unwrap_or_default();
    Some((block_size as u64 * total_blocks as u64, label))
}

/// Volume name = name of the root folder record (parent ID 1) in the catalog B-tree.
fn hfs_label(f: &mut File, off: u64, hdr: &[u8], block_size: u32) -> Option<String> {
    // catalog file fork at 272: logicalSize(8) clump(4) totalBlocks(4) extents 8x(start,count)
    let start_block = be32(hdr, 272 + 16)?;
    let cat_off = off + start_block as u64 * block_size as u64;
    let node0 = read(f, cat_off, 512);
    let first_leaf = be32(&node0, 14 + 10)?;
    let node_size = be16(&node0, 14 + 18)? as usize;
    if node_size < 512 {
        return None;
    }
    // the first leaf is assumed inside the first extent (true in practice)
    let leaf = read(f, cat_off + first_leaf as u64 * node_size as u64, node_size);
    let rec = be16(&leaf, node_size - 2)? as usize;
    if be32(&leaf, rec + 2)? != 1 {
        return None;
    }
    let n = be16(&leaf, rec + 6)? as usize;
    let units: Vec<u16> = leaf.get(rec + 8..rec + 8 + 2 * n)?.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    Some(String::from_utf16_lossy(&units))
}

pub fn apfs_info(f: &mut File, off: u64) -> Option<u64> {
    let b = read(f, off, 64);
    if b.len() < 48 || &b[32..36] != b"NXSB" {
        return None;
    }
    let bs = gpt::le32(&b, 36) as u64;
    let count = gpt::le64(&b, 40);
    bs.checked_mul(count)
}

fn sniff(f: &mut File, off: u64, hint: &str) -> Option<(u64, Fs, String)> {
    if let Some((len, label)) = hfs_info(f, off) {
        return Some((len, Fs::Hfs, if label.is_empty() { hint.into() } else { label }));
    }
    apfs_info(f, off).map(|len| (len, Fs::Apfs, hint.to_string()))
}

/// Every partition-like area: (offset, max_length, name).
fn candidates(f: &mut File, size: u64) -> Vec<(u64, u64, String)> {
    let mut out = vec![(0, size, String::new())];
    let head = read(f, 0, 64 * 1024);
    for sec in [512u64, 4096] {
        if let Some(g) = Gpt::parse_bytes(&head, sec) {
            for (_, p) in g.partitions() {
                if p.type_guid != gpt::EFI_SYSTEM {
                    out.push((p.first * sec, p.sectors() * sec, p.name.clone()));
                }
            }
        }
    }
    // Apple Partition Map
    if head.starts_with(b"ER") {
        let blk = be16(&head, 2).filter(|&b| b > 0).unwrap_or(512) as u64;
        let units: Vec<u64> = if blk == 512 { vec![512] } else { vec![512, blk] };
        for &unit in &units {
            for i in 1..64u64 {
                let e = read(f, i * unit, 512);
                if !e.starts_with(b"PM") {
                    break;
                }
                let (start, count) = (be32(&e, 8).unwrap_or(0) as u64, be32(&e, 12).unwrap_or(0) as u64);
                if e.get(48..57) == Some(b"Apple_HFS") {
                    let name = String::from_utf8_lossy(&e[16..48]).trim_end_matches('\0').to_string();
                    for &u in &units {
                        out.push((start * u, count * u, name.clone()));
                    }
                }
            }
        }
    }
    // MBR, type 0xAF (HFS+)
    if head.len() >= 512 && head[510..512] == [0x55, 0xAA] {
        for i in 0..4 {
            let e = &head[446 + 16 * i..462 + 16 * i];
            if e[4] == 0xAF {
                out.push((gpt::le32(e, 8) as u64 * 512, gpt::le32(e, 12) as u64 * 512, String::new()));
            }
        }
    }
    out
}

fn temp_path(workdir: Option<&str>) -> PathBuf {
    let dir = workdir.map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let mut r = [0u8; 6];
    let _ = getrandom::getrandom(&mut r);
    let tag: String = r.iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("sycros-{tag}.raw"))
}

/// Locate the macOS volume in `path`. Errors if there is none.
pub fn find_volume(path: &str, workdir: Option<&str>) -> Result<MacVolume> {
    let hint = name_from_file(path);
    let (src, temp) = if udif::is_udif(path) {
        let raw = temp_path(workdir);
        if let Err(e) = udif::convert(path, &raw.to_string_lossy()) {
            let _ = fs::remove_file(&raw);
            return Err(e);
        }
        (raw, true)
    } else {
        (PathBuf::from(path), false)
    };
    // `vol` owns the temp file from here on: dropping it (also on error) deletes it.
    let mut vol = MacVolume { source: src.clone(), offset: 0, length: 0, fs: Fs::Hfs, label: String::new(), temp };

    let mut f = File::open(&src)?;
    let size = f.metadata()?.len();
    let mut best: Option<(u64, u64, Fs, String)> = None;
    for (off, max_len, pname) in candidates(&mut f, size) {
        let h = if pname.is_empty() { hint.clone() } else { pname };
        if let Some((len, fs, label)) = sniff(&mut f, off, &h) {
            let len = len.min(max_len).min(size.saturating_sub(off));
            // prefer the biggest volume (an installer beats a tiny helper volume)
            if best.as_ref().map_or(true, |b| len > b.1) {
                best = Some((off, len, fs, label));
            }
        }
    }
    match best {
        Some((off, len, fs, label)) => {
            vol.offset = off;
            vol.length = len;
            vol.fs = fs;
            vol.label = if label.is_empty() { hint } else { label };
            Ok(vol)
        }
        None => Err(format!(
            "{}: no macOS volume found inside (not a macOS installer/recovery image?)",
            Path::new(path).file_name().unwrap_or_default().to_string_lossy()
        )
        .into()),
    }
}

/// Cheap check used to auto-sort files: macOS image, or Windows/Linux ISO?
pub fn looks_like_mac(path: &str) -> bool {
    let low = path.to_lowercase();
    if low.ends_with(".dmg") || low.ends_with(".dmg.img") || udif::is_udif(path) {
        return true;
    }
    let Ok(mut f) = File::open(path) else { return false };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    // Some Linux ISOs carry a small HFS+ part so that old Macs can start them. That does
    // not make them macOS images: the Mac volume must fill most of the file.
    candidates(&mut f, size).into_iter().any(|(off, max_len, _)| {
        sniff(&mut f, off, "").is_some_and(|(len, _, _)| {
            let len = len.min(max_len).min(size.saturating_sub(off));
            len.saturating_mul(2) >= size
        })
    })
}

fn name_from_file(path: &str) -> String {
    let base = Path::new(path).file_name().unwrap_or_default().to_string_lossy().to_string();
    let low = base.to_lowercase();
    let mut stem = base.as_str();
    for ext in [".dmg.img", ".dmg", ".iso", ".img", ".cdr"] {
        if low.ends_with(ext) {
            stem = &base[..base.len() - ext.len()];
            break;
        }
    }
    let s = stem.replace('_', " ").trim().to_string();
    if s.is_empty() { "macOS".into() } else { s }
}
