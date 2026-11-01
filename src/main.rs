//! sycros command line. With no arguments it opens the interactive menu (menu.rs).

mod menu;

use clap::{Parser, Subcommand};
use std::io::{self, BufRead, Write};
use std::path::Path;
use sycros::library::{self, OsKind};
use sycros::rawdisk::human;
use sycros::{engine, hosts, layout, macimg, Result};

const EXAMPLES: &str = "\
examples:
  sycros disks                                  show USB disks
  sycros init /dev/sdb --mac-space 32G          prepare a USB stick (ERASES IT)
  sycros add /dev/sdb Win11.iso ubuntu.iso      copy ISOs (sorted into Windows/Linux/Tools)
  sycros add /dev/sdb macOS_Sonoma.dmg.img      write a macOS slot (Mac boot menu)
  sycros status /dev/sdb --files
  sycros remove /dev/sdb --slot 3               delete a macOS slot
  sycros inspect Tahoe.iso                      what sycrOS sees inside a file
  sycros init stick.img --size 16G --mac-space 8G   build an image to flash later

disk names:  Linux /dev/sdb   macOS /dev/disk4   Windows 2 (disk number)";

#[derive(Parser)]
#[command(name = "sycros", version = concat!(env!("CARGO_PKG_VERSION"), " (Ventoy 1.1.18 boot engine)"),
          about = "sycrOS - one USB for Windows, Linux and macOS installers.", after_help = EXAMPLES)]
struct Cli {
    /// No command = interactive menu
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// List disks
    Disks,
    /// Download / check the Ventoy boot files
    Engine {
        /// Download again
        #[arg(long)]
        refresh: bool,
        /// Use a local ventoy-*-linux.tar.gz (offline)
        #[arg(long)]
        tarball: Option<String>,
    },
    /// Prepare a disk or image (ERASES it)
    Init {
        disk: String,
        /// Space kept for macOS slots, e.g. 32G (default 0 = no macOS)
        #[arg(long, value_parser = parse_size, default_value = "0")]
        mac_space: u64,
        /// Create a new image file of this size, e.g. 16G
        #[arg(long, value_parser = parse_size)]
        size: Option<u64>,
        /// Name of the ISO partition (1-11 characters)
        #[arg(long, default_value = "sycrOS")]
        label: String,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
        /// Allow an internal (non-USB) disk
        #[arg(long)]
        internal: bool,
    },
    /// Add ISO/WIM/IMG/VHD files or macOS DMG/ISO images
    Add {
        disk: String,
        #[arg(required = true)]
        files: Vec<String>,
        /// Force the OS folder (or mac = macOS slot)
        #[arg(long = "os", value_enum)]
        os_kind: Option<OsKind>,
        /// Name for the macOS slot (default: volume name)
        #[arg(long)]
        name: Option<String>,
        /// Folder for temporary files when unpacking .dmg
        #[arg(long)]
        workdir: Option<String>,
        /// Allow an internal (non-USB) disk
        #[arg(long)]
        internal: bool,
    },
    /// Remove a macOS slot or a file
    Remove {
        disk: String,
        /// macOS slot partition number (see status)
        #[arg(long)]
        slot: Option<usize>,
        /// File path on the disk, e.g. Linux/ubuntu.iso
        #[arg(long)]
        file: Option<String>,
        /// Allow an internal (non-USB) disk
        #[arg(long)]
        internal: bool,
    },
    /// Show what is on a sycrOS disk
    Status {
        disk: String,
        /// Also list ISO files (mounts the disk)
        #[arg(long)]
        files: bool,
    },
    /// Show how sycrOS would use a file (writes nothing)
    Inspect {
        #[arg(required = true)]
        files: Vec<String>,
        #[arg(long)]
        workdir: Option<String>,
    },
}

pub(crate) fn parse_size(text: &str) -> std::result::Result<u64, String> {
    let t = text.trim().to_uppercase();
    let t = t.trim_end_matches("IB").trim_end_matches('B');
    let (num, unit) = match t.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => (&t[..i], &t[i..]),
        None => (t, "G"), // plain number = GB
    };
    let mult: u64 = match unit {
        "K" => 1 << 10,
        "M" => 1 << 20,
        "G" => 1 << 30,
        "T" => 1 << 40,
        _ => return Err(format!("bad size: {text} (use e.g. 32G or 500M)")),
    };
    let v: f64 = num.trim().parse().map_err(|_| format!("bad size: {text} (use e.g. 32G or 500M)"))?;
    Ok((v * mult as f64) as u64)
}

/// Safety checks first (wrong disk?), then admin rights.
fn need_admin(disk: &str, internal: bool) -> Result<()> {
    if layout::is_image_target(disk) {
        return Ok(());
    }
    layout::check_disk(disk, true, internal)?;
    if hosts::is_admin() {
        return Ok(());
    }
    let how = if hosts::IS_WIN { "run the terminal as Administrator" } else { "use sudo" };
    Err(format!("Writing to a disk needs admin rights - {how}.").into())
}

fn confirm(text: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    println!("{text}");
    print!("Type YES to continue: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    if line.trim() != "YES" {
        return Err("Cancelled.".into());
    }
    Ok(())
}

pub(crate) fn run_cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Disks => {
            println!("{:<16}{:>10}  {:<10} MODEL", "DISK", "SIZE", "TYPE");
            for d in hosts::list_disks()? {
                let kind = if d.system { "SYSTEM" } else if d.removable { "usb/sd" } else { "internal" };
                println!("{:<16}{:>10}  {:<10} {}", d.id, human(d.size), kind, d.model);
            }
            println!("\nOnly use a usb/sd disk. sycrOS refuses SYSTEM disks.");
        }

        Cmd::Engine { refresh, tarball } => {
            let e = engine::get_engine(refresh, tarball.as_deref(), false)?;
            println!("Ventoy {} boot files: {}", e.version(), e.folder.display());
        }

        Cmd::Init { disk, mac_space, size, label, yes, internal } => {
            if label.is_empty() || label.chars().count() > 11 {
                return Err("--label must be 1 to 11 characters (exFAT limit)".into());
            }
            need_admin(&disk, internal)?;
            let eng = engine::get_engine(false, None, false)?;
            if size.is_some() && Path::new(&disk).exists() {
                confirm(&format!("{disk} exists and will be replaced."), yes)?;
            }
            let mut d = layout::Disk::open(&disk, size, true, internal)?;
            if let Some(info) = &d.info {
                confirm(
                    &format!("\nALL DATA on {} ({}, {}) will be ERASED.", d.name, human(d.raw.size), info.model),
                    yes,
                )?;
            }
            layout::init(&mut d, &eng, mac_space, &label, true)?;
            let root = layout::refresh_menus(&mut d)?;
            println!("\nDone. {} is a sycrOS disk.", d.name);
            if let Some(r) = root {
                println!("ISO files go in: {}  (folders Windows / Linux / Tools)", r.display());
            }
            println!("Next: sycros add <disk> <file.iso|file.dmg> ...");
        }

        Cmd::Add { disk, files, os_kind, name, workdir, internal } => {
            need_admin(&disk, internal)?;
            for f in &files {
                if !Path::new(f).is_file() {
                    return Err(format!("{f}: file not found").into());
                }
            }
            let mut d = layout::Disk::open(&disk, None, true, internal)?;
            layout::check_layout(&d.read_gpt()?)?;
            let mut isos = vec![];
            let mut changed_mac = false;
            for f in &files {
                let kind = os_kind.unwrap_or_else(|| if macimg::looks_like_mac(f) { OsKind::Mac } else { library::classify(f) });
                if kind == OsKind::Mac {
                    layout::add_mac(&mut d, f, name.as_deref(), workdir.as_deref(), true)?;
                    changed_mac = true;
                } else {
                    isos.push((f.clone(), kind));
                }
            }
            if !isos.is_empty() || changed_mac {
                let root = layout::refresh_menus(&mut d)?;
                if !isos.is_empty() {
                    let root = root.ok_or(
                        "Could not mount the ISO partition. Plug the disk in and run 'add' again, \
                         or copy the files into the Windows/Linux/Tools folders yourself.",
                    )?;
                    for (f, kind) in isos {
                        let dest = library::copy_file(&f, &root, kind)?;
                        let size = std::fs::metadata(&dest)?.len();
                        println!(
                            "  {} -> {}/  ({})",
                            Path::new(&f).file_name().unwrap_or_default().to_string_lossy(),
                            kind.folder(),
                            human(size)
                        );
                    }
                }
            }
            println!("Done.");
        }

        Cmd::Remove { disk, slot, file, internal } => {
            need_admin(&disk, internal)?;
            if slot.is_none() && file.is_none() {
                return Err("say what to remove: --slot N or --file Folder/name.iso".into());
            }
            let mut d = layout::Disk::open(&disk, None, true, internal)?;
            if let Some(n) = slot {
                layout::remove_mac(&mut d, n, true)?;
                layout::refresh_menus(&mut d)?;
            }
            if let Some(f) = file {
                if f.split(['/', '\\']).any(|part| part == "..") {
                    return Err("file path must stay inside the disk".into());
                }
                let root = d.data_folder().ok_or("Could not mount the ISO partition.")?;
                let p = root.join(&f);
                if !p.is_file() {
                    return Err(format!("{f} not found on the disk (see: sycros status --files)").into());
                }
                std::fs::remove_file(&p)?;
                println!("Removed {f}");
            }
        }

        Cmd::Status { disk, files } => {
            let mut d = layout::Disk::open(&disk, None, false, true)?;
            layout::status(&mut d)?;
            if files {
                if let Some(root) = d.data_folder() {
                    println!("\nfiles:");
                    for (folder, name, size) in library::list_files(&root) {
                        println!("  {folder:<8} {name}  ({})", human(size));
                    }
                }
            }
        }

        Cmd::Inspect { files, workdir } => {
            for f in files {
                println!("{f}:");
                if macimg::looks_like_mac(&f) {
                    let v = macimg::find_volume(&f, workdir.as_deref())?;
                    println!(
                        "  macOS image -> macOS slot. {} volume '{}', {}, type: {}",
                        v.fs.label(),
                        v.label,
                        human(v.length),
                        v.kind().label()
                    );
                    let more = if v.kind() == macimg::Kind::Recovery { "" } else { " + Apple silicon (if it is a full installer)" };
                    println!("  boots on: Intel Macs{more}");
                } else {
                    let k = library::classify(&f);
                    let lbl = library::iso_label(&f);
                    let extra = if lbl.is_empty() { String::new() } else { format!("  (label: {lbl})") };
                    println!("  {:?} image -> {}/ folder{extra}", k, k.folder());
                }
            }
        }
    }
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Some(cmd) => run_cmd(cmd),
        None => menu::run(),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
