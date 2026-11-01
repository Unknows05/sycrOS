//! Unpack Apple .dmg (UDIF) disk images to a plain raw image.
//!
//! A compressed .dmg is a list of chunks (zlib, bzip2, LZMA, LZFSE, ADC, raw, zero).
//! Each chunk is written into a sparse output file, so a 15 GB installer does
//! not need 15 GB of RAM. Everything is pure Rust, including Apple's LZFSE.

use crate::rawdisk::Progress;
use crate::Result;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

const ZERO: u32 = 0x0000_0000;
const RAW: u32 = 0x0000_0001;
const IGNORE: u32 = 0x0000_0002;
const ADC: u32 = 0x8000_0004;
const ZLIB: u32 = 0x8000_0005;
const BZIP2: u32 = 0x8000_0006;
const LZFSE: u32 = 0x8000_0007;
const LZMA: u32 = 0x8000_0008;
const COMMENT: u32 = 0x7FFF_FFFE;
const END: u32 = 0xFFFF_FFFF;

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}

/// True if the file ends with a UDIF "koly" trailer.
pub fn is_udif(path: &str) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let Ok(size) = f.seek(SeekFrom::End(0)) else { return false };
    if size < 512 || f.seek(SeekFrom::Start(size - 512)).is_err() {
        return false;
    }
    let mut sig = [0u8; 4];
    f.read_exact(&mut sig).is_ok() && &sig == b"koly"
}

struct Chunk {
    kind: u32,
    sector: u64,
    count: u64,
    offset: u64,
    length: u64,
}

fn read_blocks(f: &mut File) -> Result<(u64, u64, Vec<Chunk>)> {
    let size = f.seek(SeekFrom::End(0))?;
    f.seek(SeekFrom::Start(size - 512))?;
    let mut k = [0u8; 512];
    f.read_exact(&mut k)?;
    if &k[0..4] != b"koly" {
        return Err("not a UDIF .dmg (no 'koly' trailer)".into());
    }
    let data_fork = be64(&k, 24);
    let (xml_off, xml_len) = (be64(&k, 216), be64(&k, 224));
    let sectors = be64(&k, 492);

    f.seek(SeekFrom::Start(xml_off))?;
    let mut xml = vec![0u8; xml_len as usize];
    f.read_exact(&mut xml)?;
    let pl = plist::Value::from_reader(std::io::Cursor::new(xml)).map_err(|e| format!("dmg block table: {e}"))?;
    let entries = pl
        .as_dictionary()
        .and_then(|d| d.get("resource-fork"))
        .and_then(|v| v.as_dictionary())
        .and_then(|d| d.get("blkx"))
        .and_then(|v| v.as_array())
        .ok_or("dmg has no block table")?;

    let mut chunks = vec![];
    for e in entries {
        let mish = e
            .as_dictionary()
            .and_then(|d| d.get("Data"))
            .and_then(|v| v.as_data())
            .ok_or("bad block table entry")?;
        if mish.len() < 204 || &mish[0..4] != b"mish" {
            return Err("bad block table entry".into());
        }
        let first = be64(mish, 8);
        let data_off = be64(mish, 24);
        let n = be32(mish, 200) as usize;
        for i in 0..n {
            let c = &mish[204 + 40 * i..244 + 40 * i];
            chunks.push(Chunk {
                kind: be32(c, 0),
                sector: first + be64(c, 8),
                count: be64(c, 16),
                offset: data_fork + data_off + be64(c, 24),
                length: be64(c, 32),
            });
        }
    }
    Ok((sectors, data_fork, chunks))
}

/// Apple Data Compression (old DMGs).
pub fn adc_decompress(src: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(src.len() * 2);
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        if b & 0x80 != 0 {
            let len = (b & 0x7F) as usize + 1;
            let end = (i + 1 + len).min(src.len());
            out.extend_from_slice(&src[i + 1..end]);
            i = end;
            continue;
        }
        let (len, dist) = if b & 0x40 != 0 {
            if i + 2 >= src.len() {
                break;
            }
            let d = ((src[i + 1] as usize) << 8 | src[i + 2] as usize) + 1;
            i += 3;
            ((b & 0x3F) as usize + 4, d)
        } else {
            if i + 1 >= src.len() {
                break;
            }
            let d = (((b & 0x03) as usize) << 8 | src[i + 1] as usize) + 1;
            i += 2;
            (((b & 0x3C) >> 2) as usize + 3, d)
        };
        if dist > out.len() {
            break; // corrupt data
        }
        let start = out.len() - dist;
        for k in 0..len {
            let byte = out[start + k];
            out.push(byte);
        }
    }
    out
}

fn decode(kind: u32, data: &[u8], expect: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expect);
    match kind {
        ZLIB => {
            flate2::read::ZlibDecoder::new(data).read_to_end(&mut out)?;
        }
        BZIP2 => {
            bzip2_rs::DecoderReader::new(data).read_to_end(&mut out)?;
        }
        LZMA => {
            // Apple uses xz framing; fall back to plain .lzma
            if lzma_rs::xz_decompress(&mut std::io::Cursor::new(data), &mut out).is_err() {
                out.clear();
                lzma_rs::lzma_decompress(&mut std::io::Cursor::new(data), &mut out)
                    .map_err(|e| format!("LZMA chunk: {e:?}"))?;
            }
        }
        LZFSE => {
            lzfse_rust::decode_bytes(data, &mut out).map_err(|e| format!("LZFSE chunk: {e:?}"))?;
        }
        ADC => out = adc_decompress(data),
        other => return Err(format!("unknown dmg chunk type 0x{other:08x}").into()),
    }
    Ok(out)
}

/// Unpack `src` .dmg to raw `dst`. Returns the raw size in bytes.
pub fn convert(src: &str, dst: &str) -> Result<u64> {
    let mut f = File::open(src)?;
    let (sectors, _fork, chunks) = read_blocks(&mut f)?;
    let total = if sectors > 0 {
        sectors * 512
    } else {
        chunks.iter().map(|c| (c.sector + c.count) * 512).max().unwrap_or(0)
    };
    let mut out = File::create(dst)?;
    out.set_len(total)?; // sparse: zero areas cost nothing
    let mut p = Progress::new("unpacking dmg", total);
    let mut buf = vec![];
    for c in &chunks {
        if matches!(c.kind, ZERO | IGNORE | COMMENT | END) || c.count == 0 {
            continue;
        }
        let want = (c.count * 512) as usize;
        f.seek(SeekFrom::Start(c.offset))?;
        buf.resize(c.length as usize, 0);
        f.read_exact(&mut buf)?;
        let raw = if c.kind == RAW { buf.clone() } else { decode(c.kind, &buf, want)? };
        out.seek(SeekFrom::Start(c.sector * 512))?;
        out.write_all(&raw[..raw.len().min(want)])?;
        p.update((c.sector + c.count) * 512);
    }
    p.finish();
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adc() {
        // literal "abc" then copy 4 bytes from distance 3 -> "abcabca"
        let mut src = vec![0x82];
        src.extend_from_slice(b"abc");
        src.extend_from_slice(&[0x04, 0x02]);
        assert_eq!(adc_decompress(&src), b"abcabca");
    }
}
