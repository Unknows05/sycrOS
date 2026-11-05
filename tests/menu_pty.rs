//! Drives the real menu through a real terminal (Linux, macOS and Windows):
//! open it, check the choices, run "Show all disks", go back, and quit.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const UP: &str = "\x1b[A";
const ENTER: &str = "\r";

/// Screen text without colour and cursor codes.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() || n == '~' {
                        break;
                    }
                }
            }
            Some(']') => {
                for n in chars.by_ref() {
                    if n == '\x07' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

struct Screen(Arc<Mutex<Vec<u8>>>);

impl Screen {
    fn text(&self) -> String {
        plain(&self.0.lock().unwrap())
    }
    /// Wait for `want` to appear after byte position `from` of the plain text.
    fn wait_for(&self, want: &str, from: usize) -> usize {
        let start = Instant::now();
        loop {
            let t = self.text();
            if let Some(i) = t.get(from..).and_then(|rest| rest.find(want)) {
                return from + i + want.len();
            }
            assert!(start.elapsed() < Duration::from_secs(60), "never saw {want:?}. Screen:\n{t}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[test]
fn menu_opens_shows_disks_and_quits() {
    let pair = native_pty_system()
        .openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 })
        .unwrap();
    let mut child = pair.slave.spawn_command(CommandBuilder::new(env!("CARGO_BIN_EXE_sycros"))).unwrap();
    drop(pair.slave);

    let buf = Arc::new(Mutex::new(Vec::new()));
    let mut reader = pair.master.try_clone_reader().unwrap();
    let sink = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = reader.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    let screen = Screen(buf);
    let mut keys = pair.master.take_writer().unwrap();
    let mut send = |k: &str| {
        keys.write_all(k.as_bytes()).unwrap();
        keys.flush().unwrap();
        std::thread::sleep(Duration::from_millis(400));
    };

    let at = screen.wait_for("What do you want to do?", 0);
    let at = screen.wait_for("Quit", at);
    let menu = screen.text();
    assert!(menu.contains("Safely remove a stick"), "{menu}");
    assert_eq!(menu.contains("Build an image file"), !cfg!(windows), "{menu}");

    // Up wraps from the first choice to Quit; once more is "Show all disks"
    send(UP);
    send(UP);
    send(ENTER);
    let at = screen.wait_for("Only use a usb/sd disk", at);
    let at = screen.wait_for("Press Enter", at);
    send(ENTER);
    screen.wait_for("What do you want to do?", at);
    send(UP);
    send(ENTER);

    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "menu exit: {status:?}\n{}", screen.text());
            return;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("menu did not quit. Screen:\n{}", screen.text());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
