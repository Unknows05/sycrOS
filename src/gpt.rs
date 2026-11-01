//! Read and write GPT partition tables (plus the protective MBR entry).
//! Works on anything that implements `Disk` (a real USB stick, an image file, or memory).

use std::fmt;

pub const SECTOR: u64 = 512;
pub const ENTRY_COUNT: usize = 128;
pub const ENTRY_SIZE: usize = 128;
pub const TABLE_SECTORS: u64 = (ENTRY_COUNT * ENTRY_SIZE) as u64 / SECTOR; // 32

/// Ventoy marks its small EFI partition with bit 63 ("no drive letter" on Windows).
pub const ATTR_NO_DRIVE_LETTER: u64 = 1 << 63;

/// Byte access used by the GPT code.
pub trait Disk {
    fn read_at(&mut self, off: u64, len: usize) -> std::io::Result<Vec<u8>>;
    fn write_at(&mut self, off: u64, data: &[u8]) -> std::io::Result<()>;
}

/// A GUID kept in GPT on-disk byte order (first three fields little-endian).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    pub const ZERO: Guid = Guid([0; 16]);

    /// Parse "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" (const so we can have constants).
    pub const fn parse(s: &str) -> Guid {
        let b = s.as_bytes();
        let mut raw = [0u8; 16];
        let mut i = 0;
        let mut n = 0;
        while i < b.len() {
            if b[i] != b'-' {
                let hi = hex(b[i]);
                let lo = hex(b[i + 1]);
                raw[n] = hi << 4 | lo;
                n += 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        // big-endian text -> mixed-endian disk order
        Guid([
            raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6], raw[8], raw[9], raw[10],
            raw[11], raw[12], raw[13], raw[14], raw[15],
        ])
    }

    pub fn random() -> Guid {
        let mut b = [0u8; 16];
        getrandom::getrandom(&mut b).expect("system random source");
        b[7] = (b[7] & 0x0F) | 0x40; // version 4 (byte 7 = high byte of field 3, LE)
        b[8] = (b[8] & 0x3F) | 0x80; // variant
        Guid(b)
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 16]
    }
}

const fn hex(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("bad hex digit in GUID"),
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-",
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9]
        )?;
        for x in &b[10..] {
            write!(f, "{:02X}", x)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub const MS_BASIC_DATA: Guid = Guid::parse("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7");
pub const EFI_SYSTEM: Guid = Guid::parse("C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
pub const APPLE_HFS: Guid = Guid::parse("48465300-0000-11AA-AA11-00306543ECAC");
pub const APPLE_APFS: Guid = Guid::parse("7C3457EF-0000-11AA-AA11-00306543ECAC");

pub fn type_name(g: &Guid) -> String {
    match *g {
        MS_BASIC_DATA => "basic data".into(),
        EFI_SYSTEM => "EFI system".into(),
        APPLE_HFS => "Apple HFS+".into(),
        APPLE_APFS => "Apple APFS".into(),
        other => other.to_string(),
    }
}

#[derive(Debug)]
pub struct GptError(pub String);

impl fmt::Display for GptError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for GptError {}

fn err<T>(msg: impl Into<String>) -> Result<T, GptError> {
    Err(GptError(msg.into()))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Partition {
    pub type_guid: Guid,
    pub part_guid: Guid,
    pub first: u64, // first LBA (inclusive)
    pub last: u64,  // last LBA (inclusive)
    pub attrs: u64,
    pub name: String,
}

impl Partition {
    pub fn new(type_guid: Guid, first: u64, last: u64, attrs: u64, name: &str) -> Self {
        Partition { type_guid, part_guid: Guid::random(), first, last, attrs, name: name.into() }
    }
    pub fn sectors(&self) -> u64 {
        self.last - self.first + 1
    }
    pub fn size(&self) -> u64 {
        self.sectors() * SECTOR
    }

    fn pack(&self) -> [u8; ENTRY_SIZE] {
        let mut e = [0u8; ENTRY_SIZE];
        e[0..16].copy_from_slice(&self.type_guid.0);
        e[16..32].copy_from_slice(&self.part_guid.0);
        e[32..40].copy_from_slice(&self.first.to_le_bytes());
        e[40..48].copy_from_slice(&self.last.to_le_bytes());
        e[48..56].copy_from_slice(&self.attrs.to_le_bytes());
        for (i, u) in self.name.encode_utf16().take(36).enumerate() {
            e[56 + 2 * i..58 + 2 * i].copy_from_slice(&u.to_le_bytes());
        }
        e
    }

    fn unpack(e: &[u8]) -> Option<Partition> {
        let mut t = [0u8; 16];
        t.copy_from_slice(&e[0..16]);
        if t == [0; 16] {
            return None;
        }
        let mut p = [0u8; 16];
        p.copy_from_slice(&e[16..32]);
        let units: Vec<u16> = e[56..128]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        Some(Partition {
            type_guid: Guid(t),
            part_guid: Guid(p),
            first: le64(e, 32),
            last: le64(e, 40),
            attrs: le64(e, 48),
            name: String::from_utf16_lossy(&units),
        })
    }
}

pub fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
pub fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

#[derive(Clone, Debug)]
pub struct Gpt {
    pub total_sectors: u64,
    pub disk_guid: Guid,
    /// Slot list; index 0 = partition 1. None = empty slot.
    pub slots: Vec<Option<Partition>>,
}

impl Gpt {
    pub fn new(total_sectors: u64) -> Self {
        Gpt { total_sectors, disk_guid: Guid::random(), slots: vec![None; ENTRY_COUNT] }
    }

    pub fn first_usable(&self) -> u64 {
        2 + TABLE_SECTORS // 34
    }
    pub fn last_usable(&self) -> u64 {
        self.total_sectors - 2 - TABLE_SECTORS // total - 34
    }
    fn backup_lba(&self) -> u64 {
        self.total_sectors - 1
    }

    /// (number, partition) for used slots. Numbers start at 1.
    pub fn partitions(&self) -> impl Iterator<Item = (usize, &Partition)> {
        self.slots.iter().enumerate().filter_map(|(i, p)| p.as_ref().map(|p| (i + 1, p)))
    }

    pub fn get(&self, number: usize) -> Option<&Partition> {
        self.slots.get(number.wrapping_sub(1)).and_then(|p| p.as_ref())
    }

    /// Free (first, last) LBA ranges at or after `start`.
    pub fn free_ranges(&self, start: u64) -> Vec<(u64, u64)> {
        let lo = start.max(self.first_usable());
        let hi = self.last_usable();
        let mut used: Vec<(u64, u64)> = self.partitions().map(|(_, p)| (p.first, p.last)).collect();
        used.sort();
        let mut cur = lo;
        let mut out = vec![];
        for (a, b) in used {
            if b < cur {
                continue;
            }
            if a > hi {
                break;
            }
            if a > cur {
                out.push((cur, (a - 1).min(hi)));
            }
            cur = cur.max(b + 1);
        }
        if cur <= hi {
            out.push((cur, hi));
        }
        out
    }

    /// First `align`-aligned LBA at or after `start` with room for `sectors`.
    pub fn find_space(&self, sectors: u64, start: u64, align: u64) -> Option<u64> {
        self.free_ranges(start).into_iter().find_map(|(a, b)| {
            let first = a.div_ceil(align) * align;
            (first + sectors - 1 <= b).then_some(first)
        })
    }

    pub fn add(&mut self, part: Partition, number: Option<usize>) -> Result<usize, GptError> {
        for (n, p) in self.partitions() {
            if !(part.last < p.first || part.first > p.last) {
                return err(format!("new partition overlaps partition {n}"));
            }
        }
        if part.first < self.first_usable() || part.last > self.last_usable() || part.last < part.first {
            return err("partition outside usable area");
        }
        let n = match number {
            Some(n) => n,
            None => match self.slots.iter().position(|s| s.is_none()) {
                Some(i) => i + 1,
                None => return err("partition table is full"),
            },
        };
        if self.slots[n - 1].is_some() {
            return err(format!("partition slot {n} is already used"));
        }
        self.slots[n - 1] = Some(part);
        Ok(n)
    }

    pub fn remove(&mut self, number: usize) -> Result<Partition, GptError> {
        match self.slots.get_mut(number.wrapping_sub(1)).and_then(|s| s.take()) {
            Some(p) => Ok(p),
            None => err(format!("partition {number} does not exist")),
        }
    }

    // ---- encode -----------------------------------------------------------
    fn table_bytes(&self) -> Vec<u8> {
        let mut t = Vec::with_capacity(self.slots.len() * ENTRY_SIZE);
        for s in &self.slots {
            match s {
                Some(p) => t.extend_from_slice(&p.pack()),
                None => t.extend_from_slice(&[0u8; ENTRY_SIZE]),
            }
        }
        t
    }

    fn header_bytes(&self, primary: bool, table_crc: u32) -> Vec<u8> {
        let (me, alt, table_lba) = if primary {
            (1, self.backup_lba(), 2)
        } else {
            (self.backup_lba(), 1, self.backup_lba() - TABLE_SECTORS)
        };
        let mut h = vec![0u8; SECTOR as usize];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&me.to_le_bytes());
        h[32..40].copy_from_slice(&alt.to_le_bytes());
        h[40..48].copy_from_slice(&self.first_usable().to_le_bytes());
        h[48..56].copy_from_slice(&self.last_usable().to_le_bytes());
        h[56..72].copy_from_slice(&self.disk_guid.0);
        h[72..80].copy_from_slice(&table_lba.to_le_bytes());
        h[80..84].copy_from_slice(&(self.slots.len() as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        h[88..92].copy_from_slice(&table_crc.to_le_bytes());
        let c = crc32(&h[..92]);
        h[16..20].copy_from_slice(&c.to_le_bytes());
        h
    }

    /// Write primary + backup GPT. Sector 0 (MBR) is left alone.
    pub fn write(&self, disk: &mut dyn Disk) -> std::io::Result<()> {
        let table = self.table_bytes();
        let tcrc = crc32(&table);
        let mut primary = self.header_bytes(true, tcrc);
        primary.extend_from_slice(&table);
        disk.write_at(SECTOR, &primary)?;
        let mut backup = table;
        backup.extend_from_slice(&self.header_bytes(false, tcrc));
        disk.write_at((self.backup_lba() - TABLE_SECTORS) * SECTOR, &backup)
    }

    // ---- decode -----------------------------------------------------------
    pub fn read(disk: &mut dyn Disk, total_sectors: u64) -> Result<Gpt, GptError> {
        let io = |e: std::io::Error| GptError(format!("read error: {e}"));
        let hdr = disk.read_at(SECTOR, SECTOR as usize).map_err(io)?;
        match Self::parse(disk, &hdr, total_sectors, SECTOR) {
            Ok(g) => Ok(g),
            Err(_) => {
                let hdr = disk.read_at((total_sectors - 1) * SECTOR, SECTOR as usize).map_err(io)?;
                Self::parse(disk, &hdr, total_sectors, SECTOR)
            }
        }
    }

    fn parse(disk: &mut dyn Disk, hdr: &[u8], total: u64, sector: u64) -> Result<Gpt, GptError> {
        if hdr.len() < 92 || &hdr[0..8] != b"EFI PART" {
            return err("no GPT partition table found");
        }
        let hsize = le32(hdr, 12) as usize;
        if !(92..=hdr.len()).contains(&hsize) {
            return err("bad GPT header size");
        }
        let mut check = hdr[..hsize].to_vec();
        check[16..20].fill(0);
        if crc32(&check) != le32(hdr, 16) {
            return err("GPT header checksum is wrong");
        }
        let mut dg = [0u8; 16];
        dg.copy_from_slice(&hdr[56..72]);
        let table_lba = le64(hdr, 72);
        let count = le32(hdr, 80) as usize;
        let esize = le32(hdr, 84) as usize;
        if esize != ENTRY_SIZE || count > 1024 {
            return err("unsupported GPT entry layout");
        }
        let table = disk
            .read_at(table_lba * sector, count * esize)
            .map_err(|e| GptError(format!("read error: {e}")))?;
        if crc32(&table) != le32(hdr, 88) {
            return err("GPT partition list checksum is wrong");
        }
        let mut slots: Vec<Option<Partition>> =
            table.chunks(esize).map(Partition::unpack).collect();
        if slots.len() < ENTRY_COUNT {
            slots.resize(ENTRY_COUNT, None);
        }
        Ok(Gpt { total_sectors: total, disk_guid: Guid(dg), slots })
    }

    /// Parse a GPT from the start of an in-memory image (used to look inside DMGs).
    pub fn parse_bytes(blob: &[u8], sector: u64) -> Option<Gpt> {
        let s = sector as usize;
        if blob.len() < 2 * s {
            return None;
        }
        let mut mem = MemDisk(blob.to_vec());
        Self::parse(&mut mem, &blob[s..2 * s], blob.len() as u64 / sector, sector).ok()
    }
}

/// In-memory disk (tests, and peeking into image headers).
pub struct MemDisk(pub Vec<u8>);

impl Disk for MemDisk {
    fn read_at(&mut self, off: u64, len: usize) -> std::io::Result<Vec<u8>> {
        let o = (off as usize).min(self.0.len());
        let e = (o + len).min(self.0.len());
        let mut v = self.0[o..e].to_vec();
        v.resize(len, 0);
        Ok(v)
    }
    fn write_at(&mut self, off: u64, data: &[u8]) -> std::io::Result<()> {
        let o = off as usize;
        if o + data.len() > self.0.len() {
            self.0.resize(o + data.len(), 0);
        }
        self.0[o..o + data.len()].copy_from_slice(data);
        Ok(())
    }
}

/// The single 0xEE partition entry a GPT disk carries in its MBR.
pub fn protective_mbr_entry(total_sectors: u64) -> [u8; 16] {
    let count = (total_sectors - 1).min(0xFFFF_FFFF) as u32;
    let mut e = [0u8; 16];
    e[1..4].copy_from_slice(&[0, 2, 0]);
    e[4] = 0xEE;
    e[5..8].copy_from_slice(&[254, 255, 255]);
    e[8..12].copy_from_slice(&1u32.to_le_bytes());
    e[12..16].copy_from_slice(&count.to_le_bytes());
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_text_roundtrip() {
        assert_eq!(MS_BASIC_DATA.to_string(), "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7");
        assert_eq!(&MS_BASIC_DATA.0[..4], &[0xA2, 0xA0, 0xD0, 0xEB]);
    }

    #[test]
    fn roundtrip_and_backup() {
        let mut m = MemDisk(vec![0; 64 << 20]);
        let total = (64 << 20) / SECTOR;
        let mut g = Gpt::new(total);
        g.add(Partition::new(MS_BASIC_DATA, 2048, 4095, 0, "Ventoy"), None).unwrap();
        g.add(Partition::new(APPLE_HFS, 8192, 9000, 0, "Install macOS Ünïcode"), None).unwrap();
        g.write(&mut m).unwrap();
        let r = Gpt::read(&mut m, total).unwrap();
        let names: Vec<_> = r.partitions().map(|(n, p)| (n, p.name.clone())).collect();
        assert_eq!(names, vec![(1, "Ventoy".into()), (2, "Install macOS Ünïcode".into())]);
        m.0[512..1024].fill(0); // destroy primary header -> backup is used
        let r2 = Gpt::read(&mut m, total).unwrap();
        assert_eq!(r2.get(2).unwrap().type_guid, APPLE_HFS);
    }

    #[test]
    fn overlap_and_free_space() {
        let mut g = Gpt::new(100_000);
        g.add(Partition::new(MS_BASIC_DATA, 2048, 10239, 0, ""), None).unwrap();
        assert!(g.add(Partition::new(MS_BASIC_DATA, 10000, 12000, 0, ""), None).is_err());
        assert_eq!(g.find_space(2048, 10240, 2048), Some(10240));
        assert_eq!(g.find_space(2048, 10241, 2048), Some(12288));
        assert_eq!(g.find_space(1_000_000, 0, 2048), None);
    }
}
