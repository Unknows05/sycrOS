//! Raw byte access to a whole disk or an image file.
//!
//! Real disks (/dev/sdX, /dev/rdiskN, \\.\PhysicalDriveN) only accept reads and
//! writes in whole sectors at sector boundaries. `Target` hides that: any offset
//! or length works; it reads, patches and writes the surrounding sectors when needed.

use crate::gpt::Disk;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

pub const SECTOR: u64 = 512;
const CHUNK: usize = 4 << 20;

pub struct Target {
    pub path: String,
    pub size: u64,
    pub is_image: bool,
    file: File,
}

impl Target {
    pub fn open(path: &str, size: u64, is_image: bool, writable: bool) -> io::Result<Target> {
        let file = OpenOptions::new().read(true).write(writable).open(path)?;
        Ok(Target { path: path.into(), size, is_image, file })
    }

    pub fn sectors(&self) -> u64 {
        self.size / SECTOR
    }

    fn pread(&mut self, off: u64, n: usize) -> io::Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            let k = self.file.read(&mut buf[got..])?;
            if k == 0 {
                break; // past the end: rest stays zero
            }
            got += k;
        }
        Ok(buf)
    }

    fn pwrite(&mut self, off: u64, data: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(off))?;
        self.file.write_all(data)
    }

    pub fn zero(&mut self, mut off: u64, mut n: u64) -> io::Result<()> {
        let z = vec![0u8; CHUNK.min(n as usize)];
        while n > 0 {
            let k = (n as usize).min(z.len());
            self.write_at(off, &z[..k])?;
            off += k as u64;
            n -= k as u64;
        }
        Ok(())
    }

    /// Copy `length` bytes of a file into the disk at `dst_off`, with a progress line.
    pub fn copy_from_file(&mut self, src: &str, src_off: u64, length: u64, dst_off: u64, label: &str) -> io::Result<()> {
        let mut f = File::open(src)?;
        f.seek(SeekFrom::Start(src_off))?;
        let mut done = 0u64;
        let mut buf = vec![0u8; CHUNK];
        let mut p = Progress::new(label, length);
        while done < length {
            let want = ((length - done) as usize).min(CHUNK);
            let chunk = &mut buf[..want];
            let mut got = 0;
            while got < want {
                let k = f.read(&mut chunk[got..])?;
                if k == 0 {
                    break;
                }
                got += k;
            }
            chunk[got..].fill(0); // source ended early: pad with zeros
            self.write_at(dst_off + done, chunk)?;
            done += want as u64;
            p.update(done);
        }
        p.finish();
        Ok(())
    }

    /// Read back what was written and compare it with the source file.
    /// Run after `copy_from_file`, so a bad stick or broken write is caught.
    pub fn verify_from_file(&mut self, src: &str, src_off: u64, length: u64, dst_off: u64, label: &str) -> io::Result<()> {
        self.flush();
        forget_cached(&self.file); // read from the stick, not from the computer's memory
        let mut f = File::open(src)?;
        f.seek(SeekFrom::Start(src_off))?;
        let mut done = 0u64;
        let mut buf = vec![0u8; CHUNK];
        let mut p = Progress::new(label, length);
        while done < length {
            let want = ((length - done) as usize).min(CHUNK);
            read_full(&mut f, &mut buf[..want])?;
            if self.read_at(dst_off + done, want)? != buf[..want] {
                return Err(check_failed(done));
            }
            done += want as u64;
            p.update(done);
        }
        p.finish();
        Ok(())
    }

    pub fn flush(&mut self) {
        let _ = self.file.sync_all(); // some raw devices refuse fsync; writes are unbuffered there
    }
}

impl Disk for Target {
    fn read_at(&mut self, off: u64, n: usize) -> io::Result<Vec<u8>> {
        if self.is_image {
            return self.pread(off, n);
        }
        let a = off - off % SECTOR;
        let b = (off + n as u64).div_ceil(SECTOR) * SECTOR;
        let blob = self.pread(a, (b - a) as usize)?;
        let s = (off - a) as usize;
        Ok(blob[s..s + n].to_vec())
    }

    fn write_at(&mut self, off: u64, data: &[u8]) -> io::Result<()> {
        if off + data.len() as u64 > self.size {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "write goes past the end of the disk"));
        }
        if self.is_image || (off % SECTOR == 0 && data.len() as u64 % SECTOR == 0) {
            return self.pwrite(off, data);
        }
        let a = off - off % SECTOR;
        let b = (off + data.len() as u64).div_ceil(SECTOR) * SECTOR;
        let mut blob = self.pread(a, (b - a) as usize)?;
        let s = (off - a) as usize;
        blob[s..s + data.len()].copy_from_slice(data);
        self.pwrite(a, &blob)
    }
}

/// Fill `buf` from a file; bytes past the end of the file stay zero.
fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<()> {
    let mut got = 0;
    while got < buf.len() {
        let k = f.read(&mut buf[got..])?;
        if k == 0 {
            break;
        }
        got += k;
    }
    buf[got..].fill(0);
    Ok(())
}

fn check_failed(at: u64) -> io::Error {
    io::Error::other(format!(
        "Check after writing failed at {}: the stick did not keep the data correctly. \
         Try again; if it fails again, try another stick.",
        human(at)
    ))
}

/// Linux keeps recently written data in memory. Drop it, so that reading back
/// really reads the stick. (macOS raw disks and Windows disks are not cached this way.)
pub fn forget_cached(file: &File) {
    let _ = file.sync_all();
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = file;
}

/// Compare two files byte by byte (used after copying an ISO to the stick).
pub fn verify_files(src: &Path, dst: &Path, label: &str) -> io::Result<()> {
    let length = std::fs::metadata(src)?.len();
    if std::fs::metadata(dst)?.len() != length {
        return Err(check_failed(0));
    }
    let (mut a, mut b) = (File::open(src)?, File::open(dst)?);
    forget_cached(&b);
    let (mut x, mut y) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let mut done = 0u64;
    let mut p = Progress::new(label, length);
    while done < length {
        let want = ((length - done) as usize).min(CHUNK);
        read_full(&mut a, &mut x[..want])?;
        read_full(&mut b, &mut y[..want])?;
        if x[..want] != y[..want] {
            return Err(check_failed(done));
        }
        done += want as u64;
        p.update(done);
    }
    p.finish();
    Ok(())
}

/// "1.5 GB" style sizes.
pub fn human(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if v < 1024.0 || unit == "TB" {
            return if unit == "B" { format!("{n} B") } else { format!("{v:.1} {unit}") };
        }
        v /= 1024.0;
    }
    unreachable!()
}

/// One-line progress display on stderr.
pub struct Progress {
    label: String,
    total: u64,
    start: Instant,
    last: Option<Instant>,
}

impl Progress {
    pub fn new(label: &str, total: u64) -> Self {
        Progress { label: label.into(), total, start: Instant::now(), last: None }
    }

    pub fn update(&mut self, done: u64) {
        let now = Instant::now();
        if let Some(l) = self.last {
            if now.duration_since(l).as_millis() < 500 && done < self.total {
                return;
            }
        }
        self.last = Some(now);
        if crate::ui::attached() {
            crate::ui::send(crate::ui::Event::Progress { label: self.label.clone(), done, total: self.total });
            return;
        }
        let pct = if self.total == 0 { 100.0 } else { 100.0 * done as f64 / self.total as f64 };
        let secs = now.duration_since(self.start).as_secs_f64().max(0.001);
        eprint!(
            "\r  {}: {:5.1}%  {} / {}  ({}/s)   ",
            self.label,
            pct,
            human(done),
            human(self.total),
            human((done as f64 / secs) as u64)
        );
    }

    pub fn finish(&mut self) {
        if crate::ui::attached() {
            crate::ui::send(crate::ui::Event::ProgressEnd { label: self.label.clone() });
            return;
        }
        self.last = None;
        self.update(self.total);
        eprintln!();
    }
}
