//! Runs the real full-screen app in a real terminal, with a terminal emulator
//! (`vt100`) reading exactly what a person would see.

mod common;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const UP: &str = "\x1b[A";
const DOWN: &str = "\x1b[B";
const ENTER: &str = "\r";
const ROWS: u16 = 40;
const COLS: u16 = 120;

struct App {
    screen: Arc<Mutex<(vt100::Parser, Vec<u8>)>>,
    keys: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
}

impl App {
    fn open() -> App {
        let pair = native_pty_system()
            .openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 })
            .unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_sycros"));
        cmd.env("TERM", "xterm-256color");
        cmd.env("SYCROS_NO_UPDATE_CHECK", "1"); // keep the menu the same whatever is released
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let screen = Arc::new(Mutex::new((vt100::Parser::new(NonZeroU16::new(ROWS).unwrap(), NonZeroU16::new(COLS).unwrap(), 0), Vec::new())));
        let mut reader = pair.master.try_clone_reader().unwrap();
        let sink = screen.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut s = sink.lock().unwrap();
                s.0.process(&chunk[..n]);
                s.1.extend_from_slice(&chunk[..n]);
            }
        });
        let keys = pair.master.take_writer().unwrap();
        let mut app = App { screen, keys, child, _master: pair.master };
        // Windows' terminal layer waits for the answer to "where is the cursor?"
        if cfg!(windows) {
            let start = Instant::now();
            while !app.screen.lock().unwrap().1.windows(4).any(|w| w == b"\x1b[6n") {
                assert!(start.elapsed() < Duration::from_secs(60), "no cursor question from the console");
                std::thread::sleep(Duration::from_millis(100));
            }
            app.send("\x1b[1;1R");
        }
        app
    }
    fn send(&mut self, k: &str) {
        self.keys.write_all(k.as_bytes()).unwrap();
        self.keys.flush().unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    fn text(&self) -> String {
        self.screen.lock().unwrap().0.screen().contents()
    }
    fn wait_for(&self, want: &str, secs: u64) {
        let start = Instant::now();
        loop {
            let t = self.text();
            if t.contains(want) {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(secs), "never saw {want:?}. Screen:\n{t}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// The panel must sit in the middle of the window, within one row and column.
    fn assert_centred(&self) {
        let text = self.text();
        let rows: Vec<&str> = text.split('\n').collect();
        let top = rows.iter().position(|r| r.contains('╭')).expect("panel top");
        let bottom = rows.iter().position(|r| r.contains('╰')).expect("panel bottom");
        assert!((top as i32 - (ROWS as i32 - 1 - bottom as i32)).abs() <= 1, "rows {top} {bottom}\n{text}");
        let left = rows[top].chars().position(|c| c == '╭').unwrap() as i32;
        let right = rows[top].chars().position(|c| c == '╮').unwrap() as i32;
        assert!((left - (COLS as i32 - 1 - right)).abs() <= 1, "columns {left} {right}\n{text}");
    }
    fn quit(mut self) {
        self.send("q");
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "exit {status:?}\n{}", self.text());
                return;
            }
            if start.elapsed() > Duration::from_secs(30) {
                let _ = self.child.kill();
                panic!("app did not quit. Screen:\n{}", self.text());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn temp(name: &str, data: &[u8]) -> PathBuf {
    let mut r = [0u8; 6];
    getrandom::getrandom(&mut r).unwrap();
    let dir = std::env::temp_dir().join(format!("sycros-tui-{}", r.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

#[test]
fn opens_centred_shows_disks_checks_a_file_and_quits() {
    let mut app = App::open();
    app.wait_for("Prepare a USB stick", 60);
    app.wait_for("Quit", 5);
    app.assert_centred();
    let home = app.text();
    assert!(home.contains("Safely remove a stick"), "{home}");
    assert_eq!(home.contains("Build an image file"), !cfg!(windows), "{home}");

    // Up wraps to Quit; once more is "Show all disks"
    app.send(UP);
    app.send(UP);
    app.send(ENTER);
    app.wait_for("SYSTEM disks are always refused", 60);
    app.assert_centred();
    app.wait_for("Enter back to the menu", 30);
    app.send(ENTER);
    app.wait_for("Prepare a USB stick", 10);

    // from "Show all disks" go up to "Check a file", depending on the system
    app.send(UP);
    if !cfg!(windows) {
        app.send(UP);
    }
    app.send(ENTER);
    app.wait_for("Which files?", 10);
    let file = temp("Sequoia test.iso", &common::hfs_volume("Install macOS Sequoia", 512));
    let dragged = format!("\"{}\"", file.display());
    if cfg!(windows) {
        app.send(&dragged); // conhost delivers a drag as typed keys
    } else {
        app.send(&format!("\x1b[200~{dragged}\x1b[201~")); // terminals deliver a drag as a paste
    }
    app.send(ENTER);
    app.wait_for("Install macOS Sequoia", 30);
    app.wait_for("Enter back to the menu", 30);
    app.assert_centred();
    app.send(ENTER);
    app.wait_for("Prepare a USB stick", 10);
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
    app.quit();
}

/// A whole job through the screens, on an image file: never on a real disk.
#[test]
fn build_image_and_add_macos_through_the_screens() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let dmg = temp("Tahoe.iso", &common::hfs_volume("Install macOS Tahoe", 2053));
    let img = dmg.parent().unwrap().join("stick.img");
    let mut app = App::open();
    app.wait_for("Prepare a USB stick", 60);

    // Up three times from the top: Quit, Show all disks, Build an image file
    app.send(UP);
    app.send(UP);
    app.send(UP);
    app.send(ENTER);
    app.wait_for("Image file to create", 10);
    for _ in 0.."sycros-stick.img".len() {
        app.keys.write_all(b"\x7f").unwrap();
    }
    app.send(&img.display().to_string());
    app.send(ENTER);
    app.wait_for("Size of the image", 10);
    for _ in 0..3 {
        app.keys.write_all(b"\x7f").unwrap();
    }
    app.send("600M");
    app.send(ENTER);
    app.wait_for("Space for macOS images", 10);
    for _ in 0..3 {
        app.keys.write_all(b"\x7f").unwrap();
    }
    app.send("64M");
    app.send(ENTER);
    app.wait_for("Enter back to the menu", 180);
    let done = app.text();
    assert!(done.contains("Done") && !done.contains("error"), "{done}");
    app.send(ENTER);
    app.wait_for("Prepare a USB stick", 10);

    // Back to the top, then "Add files". Up in the disk list is always "type a name".
    for _ in 0..3 {
        app.send(DOWN);
    }
    app.send(DOWN);
    app.send(ENTER);
    app.wait_for("Which stick?", 10);
    app.send(UP);
    app.send(ENTER);
    app.wait_for("Which disk?", 10);
    app.send(&img.display().to_string());
    app.send(ENTER);
    app.wait_for("Which files?", 10);
    app.send(&dmg.display().to_string());
    app.send(ENTER);
    app.wait_for("Enter back to the menu", 120);
    let added = app.text();
    assert!(added.contains("macOS slot 3") && added.contains("checking macOS"), "{added}");
    app.send(ENTER);
    app.wait_for("Prepare a USB stick", 10);
    app.quit();
    let _ = std::fs::remove_dir_all(dmg.parent().unwrap());
}
