//! Build small fake macOS images for tests (no Mac or root needed).
#![allow(dead_code)]

use flate2::write::ZlibEncoder;
use std::io::Write;
use sycros::gpt::{self, Gpt, Guid, MemDisk, Partition};

pub const BLOCK: usize = 4096;

/// A tiny HFS+ volume: header + catalog whose first leaf holds the root folder name.
pub fn hfs_volume(label: &str, blocks: usize) -> Vec<u8> {
    let mut v = vec![0u8; blocks * BLOCK];
    let h = 1024;
    v[h..h + 2].copy_from_slice(b"H+");
    v[h + 2..h + 4].copy_from_slice(&4u16.to_be_bytes());
    v[h + 40..h + 44].copy_from_slice(&(BLOCK as u32).to_be_bytes());
    v[h + 44..h + 48].copy_from_slice(&(blocks as u32).to_be_bytes());
    // catalog fork at 272: logicalSize, clump, totalBlocks, extent[0] = (start 2, count 2)
    v[h + 272..h + 280].copy_from_slice(&((2 * BLOCK) as u64).to_be_bytes());
    v[h + 288..h + 292].copy_from_slice(&2u32.to_be_bytes());
    v[h + 292..h + 296].copy_from_slice(&2u32.to_be_bytes());
    let cat = 2 * BLOCK;
    // header node: descriptor (14 bytes), then depth(2) root(4) leafRecords(4) firstLeaf(4) ... nodeSize at +18
    v[cat + 8] = 1;
    v[cat + 14 + 10..cat + 14 + 14].copy_from_slice(&1u32.to_be_bytes());
    v[cat + 14 + 18..cat + 14 + 20].copy_from_slice(&(BLOCK as u16).to_be_bytes());
    // leaf node 1: one record, key = keyLength, parentID 1, name
    let leaf = cat + BLOCK;
    v[leaf + 8] = 0xFF;
    let name: Vec<u16> = label.encode_utf16().collect();
    let rec = 14;
    let mut key = vec![];
    key.extend_from_slice(&((6 + 2 * name.len()) as u16).to_be_bytes());
    key.extend_from_slice(&1u32.to_be_bytes());
    key.extend_from_slice(&(name.len() as u16).to_be_bytes());
    for u in &name {
        key.extend_from_slice(&u.to_be_bytes());
    }
    v[leaf + rec..leaf + rec + key.len()].copy_from_slice(&key);
    v[leaf + BLOCK - 2..leaf + BLOCK].copy_from_slice(&(rec as u16).to_be_bytes());
    // HFS+ keeps a backup copy of its header 1024 bytes before the end of the volume
    let header: Vec<u8> = v[h..h + 512].to_vec();
    let backup = v.len() - 1024;
    v[backup..backup + 512].copy_from_slice(&header);
    let end = v.len() - BLOCK;
    v[end..end + 11].copy_from_slice(b"SYCROS-TEST"); // recognisable bytes at the end
    v
}

pub fn apfs_container(blocks: usize) -> Vec<u8> {
    let mut c = vec![0u8; blocks * BLOCK];
    c[32..36].copy_from_slice(b"NXSB");
    c[36..40].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    c[40..48].copy_from_slice(&(blocks as u64).to_le_bytes());
    c
}

/// Wrap a volume in a GPT disk image with an EFI partition first (like hdiutil makes).
pub fn gpt_disk(volume: &[u8], ptype: Guid, name: &str) -> Vec<u8> {
    let vol_sectors = (volume.len() / 512) as u64;
    let total = (4096 + vol_sectors + 2048).div_ceil(2048) * 2048 + 2048;
    let mut m = MemDisk(vec![0u8; (total * 512) as usize]);
    let mut g = Gpt::new(total);
    g.add(Partition::new(gpt::EFI_SYSTEM, 40, 2047, 0, "EFI System Partition"), None).unwrap();
    g.add(Partition::new(ptype, 4096, 4096 + vol_sectors - 1, 0, name), None).unwrap();
    g.write(&mut m).unwrap();
    m.0[446..462].copy_from_slice(&gpt::protective_mbr_entry(total));
    m.0[510] = 0x55;
    m.0[511] = 0xAA;
    m.0[4096 * 512..4096 * 512 + volume.len()].copy_from_slice(volume);
    m.0
}

/// Old Apple Partition Map image with one Apple_HFS partition.
pub fn apm_disk(volume: &[u8]) -> Vec<u8> {
    let start = 64usize;
    let mut img = vec![0u8; start * 512 + volume.len()];
    img[0..2].copy_from_slice(b"ER");
    img[2..4].copy_from_slice(&512u16.to_be_bytes());
    let e = 512;
    img[e..e + 2].copy_from_slice(b"PM");
    img[e + 4..e + 8].copy_from_slice(&1u32.to_be_bytes());
    img[e + 8..e + 12].copy_from_slice(&(start as u32).to_be_bytes());
    img[e + 12..e + 16].copy_from_slice(&((volume.len() / 512) as u32).to_be_bytes());
    img[e + 16..e + 26].copy_from_slice(b"disk image");
    img[e + 48..e + 57].copy_from_slice(b"Apple_HFS");
    img[start * 512..].copy_from_slice(volume);
    img
}

/// Encode raw bytes as a UDIF .dmg using zlib / raw / LZFSE / zero chunks.
pub fn udif(raw: &[u8], chunk_sectors: usize) -> Vec<u8> {
    let sectors = raw.len() / 512;
    let mut data = vec![];
    let mut chunks: Vec<(u32, u64, u64, u64, u64)> = vec![];
    for (i, s) in (0..sectors).step_by(chunk_sectors).enumerate() {
        let n = chunk_sectors.min(sectors - s);
        let piece = &raw[s * 512..(s + n) * 512];
        let off = data.len() as u64;
        if piece.iter().all(|&b| b == 0) {
            chunks.push((0, s as u64, n as u64, off, 0));
        } else if i % 3 == 1 {
            data.extend_from_slice(piece);
            chunks.push((1, s as u64, n as u64, off, piece.len() as u64));
        } else if i % 3 == 2 {
            let mut z = vec![];
            lzfse_rust::encode_bytes(piece, &mut z).unwrap();
            data.extend_from_slice(&z);
            chunks.push((0x8000_0007, s as u64, n as u64, off, z.len() as u64));
        } else {
            let mut enc = ZlibEncoder::new(vec![], flate2::Compression::default());
            enc.write_all(piece).unwrap();
            let z = enc.finish().unwrap();
            data.extend_from_slice(&z);
            chunks.push((0x8000_0005, s as u64, n as u64, off, z.len() as u64));
        }
    }
    chunks.push((0xFFFF_FFFF, sectors as u64, 0, data.len() as u64, 0));

    let mut mish = vec![0u8; 204];
    mish[0..4].copy_from_slice(b"mish");
    mish[4..8].copy_from_slice(&1u32.to_be_bytes());
    mish[16..24].copy_from_slice(&(sectors as u64).to_be_bytes());
    mish[200..204].copy_from_slice(&(chunks.len() as u32).to_be_bytes());
    for (kind, s, n, off, len) in &chunks {
        mish.extend_from_slice(&kind.to_be_bytes());
        mish.extend_from_slice(&0u32.to_be_bytes());
        for x in [s, n, off, len] {
            mish.extend_from_slice(&x.to_be_bytes());
        }
    }
    let mut blk = plist::Dictionary::new();
    blk.insert("Name".into(), plist::Value::String("whole disk".into()));
    blk.insert("Data".into(), plist::Value::Data(mish));
    let mut rf = plist::Dictionary::new();
    rf.insert("blkx".into(), plist::Value::Array(vec![plist::Value::Dictionary(blk)]));
    let mut root = plist::Dictionary::new();
    root.insert("resource-fork".into(), plist::Value::Dictionary(rf));
    let mut xml = vec![];
    plist::Value::Dictionary(root).to_writer_xml(&mut xml).unwrap();

    let mut koly = vec![0u8; 512];
    koly[0..4].copy_from_slice(b"koly");
    koly[4..8].copy_from_slice(&4u32.to_be_bytes());
    koly[8..12].copy_from_slice(&512u32.to_be_bytes());
    koly[32..40].copy_from_slice(&(data.len() as u64).to_be_bytes());
    koly[216..224].copy_from_slice(&(data.len() as u64).to_be_bytes());
    koly[224..232].copy_from_slice(&(xml.len() as u64).to_be_bytes());
    koly[492..500].copy_from_slice(&(sectors as u64).to_be_bytes());
    let mut out = data;
    out.extend_from_slice(&xml);
    out.extend_from_slice(&koly);
    out
}
