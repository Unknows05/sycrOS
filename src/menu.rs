//! Interactive terminal menu: `sycros` with no arguments.
//! Arrow keys + Enter to choose, Esc to go back. Every action reuses the
//! normal commands (`run_cmd`), so menu and typed commands behave the same.

use crate::{parse_size, run_cmd, Cmd};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};
use std::path::Path;
use sycros::library;
use sycros::rawdisk::human;
use sycros::{hosts, layout, Result};

type Action = fn() -> Result<()>;

fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

pub fn run() -> Result<()> {
    println!();
    println!("  sycrOS {}  -  one USB for Windows, Linux and macOS", env!("CARGO_PKG_VERSION"));
    println!("  (boot engine: Ventoy 1.1.18)   arrows + Enter to choose, Esc to go back");
    if !hosts::is_admin() {
        let how = if hosts::IS_WIN { "open the terminal as Administrator" } else { "run: sudo sycros" };
        println!("\n  Note: changing a USB stick needs admin rights - {how}.");
        println!("  Without it you can still check files{}.", if hosts::IS_WIN { "" } else { " and build image files" });
    }
    let mut actions: Vec<(&str, Action)> = vec![
        ("Prepare a USB stick (erases it)", prepare_stick),
        ("Add files to a stick (ISO, DMG, ...)", add_files),
        ("Show what is on a stick", show_stick),
        ("Remove a macOS section", remove_slot),
        ("Remove a file", remove_file),
        ("Safely remove a stick", eject_stick),
        ("Check a file (writes nothing)", check_files),
    ];
    if !hosts::IS_WIN {
        actions.push(("Build an image file instead of a stick", build_image)); // not possible on Windows
    }
    actions.push(("Show all disks", || run_cmd(Cmd::Disks)));
    let labels: Vec<&str> = actions.iter().map(|(label, _)| *label).chain(["Quit"]).collect();
    loop {
        println!();
        let Some(choice) = Select::with_theme(&theme())
            .with_prompt("What do you want to do?")
            .items(&labels)
            .default(0)
            .interact_opt()?
        else {
            return Ok(());
        };
        let Some((_, action)) = actions.get(choice) else { return Ok(()) };
        let result = action();
        match result {
            Ok(()) => {}
            Err(e) if e.to_string() == "back" => continue,
            Err(e) => println!("\n  Problem: {e}"),
        }
        let _ = Input::<String>::with_theme(&theme())
            .with_prompt("Press Enter to go back to the menu")
            .allow_empty(true)
            .interact_text();
    }
}

fn back() -> sycros::Error {
    "back".into()
}

/// Let the user pick a USB stick (or type a disk name / image path).
fn pick_disk(prompt: &str, writable: bool) -> Result<(String, bool)> {
    loop {
        let disks: Vec<_> = hosts::list_disks()?.into_iter().filter(|d| !d.system).collect();
        let usb: Vec<_> = disks.iter().filter(|d| d.removable).collect();
        let mut items: Vec<String> =
            usb.iter().map(|d| format!("{:<14} {:>9}   {}", d.id, human(d.size), d.model)).collect();
        if usb.is_empty() {
            println!("\n  No USB stick found. Plug one in, then choose 'Look again'.");
        }
        items.push("Look again".into());
        items.push("Type a disk name or an image file path ...".into());
        let Some(i) = Select::with_theme(&theme()).with_prompt(prompt).items(&items).default(0).interact_opt()? else {
            return Err(back());
        };
        if i < usb.len() {
            return Ok((usb[i].id.clone(), false));
        }
        if i == usb.len() {
            continue;
        }
        let typed: String = Input::with_theme(&theme())
            .with_prompt("Disk name (e.g. /dev/sdb, /dev/disk4, 2) or image file")
            .interact_text()?;
        let typed = clean_path(&typed);
        if layout::is_image_target(&typed) {
            return Ok((typed, false));
        }
        // a typed non-USB disk: say so clearly and ask once more
        let internal = match layout::check_disk(&typed, writable, false) {
            Ok(_) => false,
            Err(e) if e.to_string().contains("internal disk") => {
                println!("\n  {e}");
                let sure = Confirm::with_theme(&theme())
                    .with_prompt("This is NOT a USB stick. Use it anyway?")
                    .default(false)
                    .interact()?;
                if !sure {
                    return Err(back());
                }
                true
            }
            Err(e) => return Err(e),
        };
        return Ok((typed, internal));
    }
}

fn ask_size(prompt: &str, default: &str) -> Result<u64> {
    loop {
        let t: String = Input::with_theme(&theme()).with_prompt(prompt).default(default.into()).interact_text()?;
        match parse_size(&t) {
            Ok(v) => return Ok(v),
            Err(e) => println!("  {e}"),
        }
    }
}

const MAC_SPACE_HELP: &str = "Space for macOS images (0 = no macOS)";

fn prepare_stick() -> Result<()> {
    let (disk, internal) = pick_disk("Which USB stick? (it will be ERASED)", true)?;
    let mac_space = ask_mac_space()?;
    run_cmd(Cmd::Init { disk, mac_space, size: None, label: "sycrOS".into(), yes: false, internal })
}

/// Ask for the macOS space. 0 is allowed, but only after a clear warning,
/// because the size can only be changed later by erasing the stick again.
fn ask_mac_space() -> Result<u64> {
    println!("\n  The macOS space is fixed now. To change it later you must erase the stick again.");
    println!("  Examples: one full installer 18G, two installers 34G, Recovery images about 2G each.");
    loop {
        let v = ask_size(MAC_SPACE_HELP, "34G")?;
        if v > 0 {
            return Ok(v);
        }
        let sure = Confirm::with_theme(&theme())
            .with_prompt("No macOS space: you can NOT add macOS images to this stick later. Continue without macOS?")
            .default(false)
            .interact()?;
        if sure {
            return Ok(0);
        }
    }
}

fn build_image() -> Result<()> {
    let path: String = Input::with_theme(&theme())
        .with_prompt("Image file to create")
        .default("sycros-stick.img".into())
        .interact_text()?;
    let size = ask_size("Size of the image (same as or smaller than your USB stick)", "64G")?;
    let mac_space = ask_mac_space()?;
    run_cmd(Cmd::Init { disk: clean_path(&path), mac_space, size: Some(size), label: "sycrOS".into(), yes: false, internal: false })
}

fn ask_files(prompt: &str) -> Result<Vec<String>> {
    loop {
        let t: String = Input::with_theme(&theme()).with_prompt(prompt).allow_empty(true).interact_text()?;
        let files = split_paths(&t);
        if files.is_empty() {
            return Err(back());
        }
        let missing: Vec<_> = files.iter().filter(|f| !Path::new(f).is_file()).collect();
        if missing.is_empty() {
            return Ok(files);
        }
        for m in missing {
            println!("  not found: {m}");
        }
    }
}

const FILES_HELP: &str = "Drag files here or type their paths (space between files, Enter when done)";

fn add_files() -> Result<()> {
    let (disk, internal) = pick_disk("Which stick?", true)?;
    let files = ask_files(FILES_HELP)?;
    run_cmd(Cmd::Add { disk, files, os_kind: None, name: None, workdir: None, internal })
}

fn show_stick() -> Result<()> {
    let (disk, _) = pick_disk("Which stick?", false)?;
    run_cmd(Cmd::Status { disk, files: true })
}

fn eject_stick() -> Result<()> {
    let (disk, internal) = pick_disk("Which stick?", true)?;
    run_cmd(Cmd::Eject { disk, internal })
}

fn check_files() -> Result<()> {
    let files = ask_files(FILES_HELP)?;
    run_cmd(Cmd::Inspect { files, workdir: None })
}

fn remove_slot() -> Result<()> {
    let (disk, internal) = pick_disk("Which stick?", true)?;
    let slots = {
        let mut d = layout::Disk::open(&disk, None, false, true)?;
        let g = d.read_gpt()?;
        layout::check_layout(&g)?;
        layout::mac_slots(&g)
    };
    if slots.is_empty() {
        println!("\n  This stick has no macOS sections.");
        return Ok(());
    }
    let items: Vec<String> = slots.iter().map(|(n, p)| format!("{n}  {}  ({})", p.name, human(p.size()))).collect();
    let Some(i) = Select::with_theme(&theme()).with_prompt("Remove which macOS section?").items(&items).interact_opt()? else {
        return Err(back());
    };
    let sure = Confirm::with_theme(&theme()).with_prompt(format!("Remove '{}'?", slots[i].1.name)).default(false).interact()?;
    if !sure {
        return Err(back());
    }
    run_cmd(Cmd::Remove { disk, slot: Some(slots[i].0), file: None, internal })
}

fn remove_file() -> Result<()> {
    let (disk, internal) = pick_disk("Which stick?", true)?;
    let files = {
        let mut d = layout::Disk::open(&disk, None, false, true)?;
        let root = d.data_folder().ok_or("Could not open the ISO partition.")?;
        library::list_files(&root)
    };
    if files.is_empty() {
        println!("\n  No ISO files on this stick.");
        return Ok(());
    }
    let items: Vec<String> = files.iter().map(|(f, n, s)| format!("{f:<8} {n}  ({})", human(*s))).collect();
    let Some(i) = Select::with_theme(&theme()).with_prompt("Remove which file?").items(&items).interact_opt()? else {
        return Err(back());
    };
    let (folder, name, _) = &files[i];
    let rel = if folder == "/" { name.clone() } else { format!("{folder}/{name}") };
    let sure = Confirm::with_theme(&theme()).with_prompt(format!("Delete {rel}?")).default(false).interact()?;
    if !sure {
        return Err(back());
    }
    run_cmd(Cmd::Remove { disk, slot: None, file: Some(rel), internal })
}

/// Strip quotes a terminal adds when you drag a file in.
fn clean_path(s: &str) -> String {
    split_paths(s).into_iter().next().unwrap_or_default()
}

/// Split "a.iso 'my file.dmg' "x y.iso" b\ c.iso" into paths.
/// Handles the quoting that terminals add when files are dragged in.
pub(crate) fn split_paths(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut chars = s.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            // backslash-escape on Unix ("my\ file.iso"); on Windows backslash is a path separator
            (None, '\\') if !cfg!(windows) => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .map(|p| p.strip_prefix("file://").map(String::from).unwrap_or(p))
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::split_paths;

    #[test]
    fn dragged_paths() {
        assert_eq!(split_paths("a.iso  b.iso"), vec!["a.iso", "b.iso"]);
        assert_eq!(split_paths("'/home/x/my file.dmg' \"/tmp/y z.iso\""), vec!["/home/x/my file.dmg", "/tmp/y z.iso"]);
        if !cfg!(windows) {
            assert_eq!(split_paths("/a/my\\ file.iso"), vec!["/a/my file.iso"]);
        }
        assert_eq!(split_paths("file:///tmp/a.iso"), vec!["/tmp/a.iso"]);
        assert!(split_paths("   ").is_empty());
    }
}
