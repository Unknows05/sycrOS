//! The full-screen app: `sycros` with no arguments.
//!
//! One centred panel, redrawn only when something changes. The spinner runs only
//! while work is happening. Every action reuses the normal commands (`run_cmd`), on a
//! background thread; their messages and progress arrive through `sycros::ui`.

use crate::menu::split_paths;
use crate::{parse_size, run_cmd, Cmd};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::Frame;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sycros::hosts::{self, DiskInfo};
use sycros::rawdisk::human;
use sycros::{layout, library, ui, update, Result};

const MIN_W: u16 = 70;
const MIN_H: u16 = 24;
const PANEL_W: u16 = 76;
const SPINNER: &[&str] = if cfg!(windows) { &["|", "/", "-", "\\"] } else { &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"] };
const TICK: &str = if cfg!(windows) { "√" } else { "✓" };
const PART_COLORS: [Color; 5] = [Color::Blue, Color::Magenta, Color::Green, Color::Yellow, Color::Cyan];

/// The full-screen app needs a real terminal of a reasonable size.
pub fn usable() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
        && crossterm::terminal::size().map(|(w, h)| w >= MIN_W && h >= MIN_H).unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Prepare,
    Add,
    Show,
    RemoveSection,
    RemoveFile,
    Eject,
    Check,
    BuildImage,
    Disks,
    Update,
    Quit,
}

fn home_items(latest: &Option<String>) -> Vec<(String, Action)> {
    let mut items: Vec<(String, Action)> = [
        ("Prepare a USB stick (erases it)", Action::Prepare),
        ("Add files to a stick (ISO, DMG, ...)", Action::Add),
        ("Show what is on a stick", Action::Show),
        ("Remove a macOS section", Action::RemoveSection),
        ("Remove a file", Action::RemoveFile),
        ("Safely remove a stick", Action::Eject),
        ("Check a file (writes nothing)", Action::Check),
    ]
    .into_iter()
    .map(|(label, action)| (label.to_string(), action))
    .collect();
    if !hosts::IS_WIN {
        items.push(("Build an image file instead of a stick".into(), Action::BuildImage));
    }
    items.push(("Show all disks".into(), Action::Disks));
    if let Some(v) = latest {
        items.push((format!("Update sycrOS to {v}"), Action::Update));
    }
    items.push(("Quit".into(), Action::Quit));
    items
}

#[derive(Clone, Copy, PartialEq)]
enum Ask {
    Disk,
    MacSpace,
    ImagePath,
    ImageSize,
    Files,
}

#[derive(Clone, Copy, PartialEq)]
enum Sure {
    UseThisDisk,
    NoMacSpace,
    Erase,
    Overwrite,
    RemoveItem,
}

enum Screen {
    Home,
    PickDisk { sel: usize },
    Ask { kind: Ask, value: String, error: String },
    Sure { kind: Sure, title: String, lines: Vec<String>, words: String, typed: String },
    PickItem { title: String, items: Vec<(String, String)>, sel: usize },
    Work(Work),
}

#[derive(Default)]
struct Work {
    title: String,
    steps: Vec<String>,
    label: String,
    done: u64,
    total: u64,
    started: Option<Instant>,
    result: Option<std::result::Result<(), String>>,
    /// Lines scrolled up from the end of the messages.
    scroll: usize,
    stop_asked: bool,
}

/// What the user has chosen so far in the current action.
struct Flow {
    action: Action,
    disk: Option<String>,
    internal: bool,
    mac_space: Option<u64>,
    no_mac_ok: bool,
    erase_ok: bool,
    path: Option<String>,
    size: Option<u64>,
    files: Vec<String>,
    item: Option<String>,
    item_ok: bool,
}

impl Flow {
    fn new(action: Action) -> Flow {
        Flow {
            action,
            disk: None,
            internal: false,
            mac_space: None,
            no_mac_ok: false,
            erase_ok: false,
            path: None,
            size: None,
            files: vec![],
            item: None,
            item_ok: false,
        }
    }
}

struct App {
    disks: Vec<DiskInfo>,
    disks_read: bool,
    admin: bool,
    home_sel: usize,
    screen: Screen,
    flow: Flow,
    frame: usize,
    /// A newer released version, if the check at start found one.
    latest: Option<String>,
    /// A command to start; `run` picks it up.
    pending: Option<Cmd>,
    quit: bool,
}

fn offered(disks: &[DiskInfo]) -> Vec<&DiskInfo> {
    disks.iter().filter(|d| !d.system && d.removable && hosts::holds_system(&d.parts).is_none()).collect()
}

fn not_offered(disks: &[DiskInfo]) -> Vec<&DiskInfo> {
    disks.iter().filter(|d| !d.system && d.removable && hosts::holds_system(&d.parts).is_some()).collect()
}

impl App {
    fn new(admin: bool) -> App {
        App {
            disks: vec![],
            disks_read: false,
            admin,
            home_sel: 0,
            screen: Screen::Home,
            flow: Flow::new(Action::Quit),
            frame: 0,
            latest: None,
            pending: None,
            quit: false,
        }
    }

    fn working(&self) -> bool {
        matches!(&self.screen, Screen::Work(w) if w.result.is_none())
    }

    fn problem(&mut self, title: &str, text: String) {
        self.screen = Screen::Work(Work { title: title.into(), result: Some(Err(text)), ..Default::default() });
    }

    fn start(&mut self, title: &str, cmd: Cmd) {
        self.screen = Screen::Work(Work { title: title.into(), started: Some(Instant::now()), ..Default::default() });
        self.pending = Some(cmd);
    }

    fn ask(&mut self, kind: Ask, value: &str) {
        self.screen = Screen::Ask { kind, value: value.into(), error: String::new() };
    }

    fn sure(&mut self, kind: Sure, title: &str, lines: Vec<String>, words: String) {
        self.screen = Screen::Sure { kind, title: title.into(), lines, words, typed: String::new() };
    }

    /// Make sure a disk is chosen. False: another screen must be answered first.
    fn have_disk(&mut self) -> bool {
        if self.flow.disk.is_none() {
            // Always ask, even with one stick plugged in: never assume which disk is meant.
            self.screen = Screen::PickDisk { sel: 0 };
            return false;
        }
        let disk = self.flow.disk.as_deref().unwrap_or("");
        if !layout::is_image_target(disk) && !self.admin {
            let how = if hosts::IS_WIN { "open sycrOS as Administrator" } else { "quit and run: sudo sycros" };
            self.problem("Admin rights needed", format!("Changing a USB stick needs admin rights: {how}."));
            return false;
        }
        true
    }

    /// Move the current action on to its next question, or start it.
    fn step(&mut self) {
        match self.flow.action {
            Action::Prepare => {
                if !self.have_disk() {
                    return;
                }
                let Some(mac_space) = self.flow.mac_space else {
                    return self.ask(Ask::MacSpace, "34G");
                };
                if mac_space == 0 && !self.flow.no_mac_ok {
                    return self.sure(
                        Sure::NoMacSpace,
                        "No macOS space?",
                        vec!["You can NOT add macOS images to this stick later without erasing it again.".into()],
                        "NO MACOS".into(),
                    );
                }
                let disk = self.flow.disk.clone().unwrap_or_default();
                if !self.flow.erase_ok {
                    let lines = match self.disks.iter().find(|d| d.id == disk) {
                        Some(d) => vec![
                            format!("Disk:      {}", d.id),
                            format!("Model:     {}", d.model),
                            format!("Size:      {}", human(d.size)),
                            format!("Now on it: {}", hosts::describe(&d.parts)),
                            String::new(),
                            "ALL DATA on this disk will be ERASED.".into(),
                        ],
                        None => vec![format!("ALL DATA on {disk} will be ERASED.")],
                    };
                    return self.sure(Sure::Erase, "Is this the right disk?", lines, hosts::erase_words(&disk));
                }
                let cmd = Cmd::Init { disk, mac_space, size: None, label: "sycrOS".into(), yes: true, internal: self.flow.internal };
                self.start("Preparing the stick", cmd);
            }
            Action::BuildImage => {
                let Some(path) = self.flow.path.clone() else {
                    return self.ask(Ask::ImagePath, "sycros-stick.img");
                };
                let Some(size) = self.flow.size else {
                    return self.ask(Ask::ImageSize, "64G");
                };
                let Some(mac_space) = self.flow.mac_space else {
                    return self.ask(Ask::MacSpace, "34G");
                };
                if mac_space == 0 && !self.flow.no_mac_ok {
                    return self.sure(
                        Sure::NoMacSpace,
                        "No macOS space?",
                        vec!["You can NOT add macOS images to this image later.".into()],
                        "NO MACOS".into(),
                    );
                }
                if Path::new(&path).exists() && !self.flow.erase_ok {
                    return self.sure(
                        Sure::Overwrite,
                        "This file already exists",
                        vec![format!("{path} will be OVERWRITTEN.")],
                        hosts::erase_words("file"),
                    );
                }
                let cmd = Cmd::Init { disk: path, mac_space, size: Some(size), label: "sycrOS".into(), yes: true, internal: false };
                self.start("Building the image", cmd);
            }
            Action::Add => {
                if !self.have_disk() {
                    return;
                }
                if self.flow.files.is_empty() {
                    return self.ask(Ask::Files, "");
                }
                let cmd = Cmd::Add {
                    disk: self.flow.disk.clone().unwrap_or_default(),
                    files: self.flow.files.clone(),
                    os_kind: None,
                    name: None,
                    workdir: None,
                    internal: self.flow.internal,
                };
                self.start("Adding files", cmd);
            }
            Action::Show => {
                if self.have_disk() {
                    let disk = self.flow.disk.clone().unwrap_or_default();
                    self.start("What is on the stick", Cmd::Status { disk, files: true });
                }
            }
            Action::Eject => {
                if self.have_disk() {
                    let cmd = Cmd::Eject { disk: self.flow.disk.clone().unwrap_or_default(), internal: self.flow.internal };
                    self.start("Safely removing the stick", cmd);
                }
            }
            Action::Check => {
                if self.flow.files.is_empty() {
                    return self.ask(Ask::Files, "");
                }
                self.start("Checking files", Cmd::Inspect { files: self.flow.files.clone(), workdir: None });
            }
            Action::Disks => self.start("All disks", Cmd::Disks),
            Action::Update => self.start("Updating sycrOS", Cmd::Update),
            Action::RemoveSection | Action::RemoveFile => self.step_remove(),
            Action::Quit => self.quit = true,
        }
    }

    fn step_remove(&mut self) {
        if !self.have_disk() {
            return;
        }
        let disk = self.flow.disk.clone().unwrap_or_default();
        let section = self.flow.action == Action::RemoveSection;
        let Some(item) = self.flow.item.clone() else {
            return match removable_items(&disk, section) {
                Ok(items) if items.is_empty() => {
                    let what = if section { "macOS sections" } else { "ISO files" };
                    self.problem("Nothing to remove", format!("This stick has no {what}."));
                }
                Ok(items) => {
                    let title = if section { "Remove which macOS section?" } else { "Remove which file?" };
                    self.screen = Screen::PickItem { title: title.into(), items, sel: 0 };
                }
                Err(e) => self.problem("Could not read the stick", e.to_string()),
            };
        };
        if !self.flow.item_ok {
            return self.sure(Sure::RemoveItem, "Remove this?", vec![item_name(&item)], "REMOVE".into());
        }
        let internal = self.flow.internal;
        let cmd = if section {
            Cmd::Remove { disk, slot: item.split(':').next().and_then(|n| n.parse().ok()), file: None, internal }
        } else {
            Cmd::Remove { disk, slot: None, file: Some(item), internal }
        };
        self.start("Removing", cmd);
    }

    fn submit_ask(&mut self, kind: Ask, value: String) {
        let set_error = |app: &mut App, error: String| {
            if let Screen::Ask { error: e, .. } = &mut app.screen {
                *e = error;
            }
        };
        match kind {
            Ask::Disk => {
                let typed = split_paths(&value).into_iter().next().unwrap_or_default();
                if layout::is_image_target(&typed) {
                    self.flow.disk = Some(typed);
                    return self.step();
                }
                match layout::check_disk(&typed, self.flow.action != Action::Show, false) {
                    Ok(_) => self.flow.disk = Some(typed),
                    Err(e) if e.to_string().contains("--internal") => {
                        let words = format!("USE {}", typed.rsplit('/').next().unwrap_or(&typed));
                        self.flow.disk = Some(typed);
                        return self.sure(Sure::UseThisDisk, "This does not look like a USB stick", vec![e.to_string()], words);
                    }
                    Err(e) => return set_error(self, e.to_string()),
                }
            }
            Ask::MacSpace => match parse_size(&value) {
                Ok(v) => self.flow.mac_space = Some(v),
                Err(e) => return set_error(self, e),
            },
            Ask::ImageSize => match parse_size(&value) {
                Ok(v) => self.flow.size = Some(v),
                Err(e) => return set_error(self, e),
            },
            Ask::ImagePath => {
                let path = split_paths(&value).into_iter().next().unwrap_or_default();
                if path.is_empty() {
                    return set_error(self, "Type a file name.".into());
                }
                self.flow.path = Some(path);
            }
            Ask::Files => {
                let files = split_paths(&value);
                if files.is_empty() {
                    return set_error(self, "Drag a file here, or type its path.".into());
                }
                if let Some(m) = files.iter().find(|f| !Path::new(f).is_file()) {
                    return set_error(self, format!("Not found: {m}"));
                }
                self.flow.files = files;
            }
        }
        self.step();
    }

    fn sure_ok(&mut self, kind: Sure) {
        match kind {
            Sure::UseThisDisk => self.flow.internal = true,
            Sure::NoMacSpace => self.flow.no_mac_ok = true,
            Sure::Erase | Sure::Overwrite => self.flow.erase_ok = true,
            Sure::RemoveItem => self.flow.item_ok = true,
        }
        self.step();
    }

    fn key(&mut self, code: KeyCode, ctrl: bool) {
        if ctrl && matches!(code, KeyCode::Char('c')) && !self.working() {
            self.quit = true;
            return;
        }
        match &mut self.screen {
            Screen::Home => {
                let items = home_items(&self.latest);
                match code {
                    KeyCode::Up => self.home_sel = (self.home_sel + items.len() - 1) % items.len(),
                    KeyCode::Down => self.home_sel = (self.home_sel + 1) % items.len(),
                    KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                    KeyCode::Enter => {
                        self.flow = Flow::new(items[self.home_sel].1);
                        self.step();
                    }
                    _ => {}
                }
            }
            Screen::PickDisk { sel } => {
                let count = offered(&self.disks).len() + 1; // + "type a name"
                *sel = (*sel).min(count - 1);
                match code {
                    KeyCode::Up => *sel = (*sel + count - 1) % count,
                    KeyCode::Down => *sel = (*sel + 1) % count,
                    KeyCode::Esc => self.screen = Screen::Home,
                    KeyCode::Enter => {
                        let chosen = offered(&self.disks).get(*sel).map(|d| d.id.clone());
                        match chosen {
                            Some(id) => {
                                self.flow.disk = Some(id);
                                self.step();
                            }
                            None => self.ask(Ask::Disk, ""),
                        }
                    }
                    _ => {}
                }
            }
            Screen::Ask { kind, value, .. } => match code {
                KeyCode::Esc => self.screen = Screen::Home,
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) if !ctrl => value.push(c),
                KeyCode::Enter => {
                    let (kind, value) = (*kind, value.clone());
                    self.submit_ask(kind, value);
                }
                _ => {}
            },
            Screen::Sure { kind, words, typed, .. } => match code {
                KeyCode::Esc => self.screen = Screen::Home,
                KeyCode::Backspace => {
                    typed.pop();
                }
                KeyCode::Char(c) if !ctrl => typed.push(c),
                KeyCode::Enter if typed.trim() == words.as_str() => {
                    let kind = *kind;
                    self.sure_ok(kind);
                }
                _ => {}
            },
            Screen::PickItem { items, sel, .. } => match code {
                KeyCode::Up => *sel = (*sel + items.len() - 1) % items.len(),
                KeyCode::Down => *sel = (*sel + 1) % items.len(),
                KeyCode::Esc => self.screen = Screen::Home,
                KeyCode::Enter => {
                    self.flow.item = Some(items[*sel].1.clone());
                    self.step();
                }
                _ => {}
            },
            Screen::Work(w) if w.result.is_some() => match code {
                KeyCode::Enter | KeyCode::Esc => self.screen = Screen::Home,
                KeyCode::Up => w.scroll += 1,
                KeyCode::Down => w.scroll = w.scroll.saturating_sub(1),
                _ => {}
            },
            Screen::Work(_) => {}
        }
    }

    fn paste(&mut self, text: &str) {
        match &mut self.screen {
            Screen::Ask { value, .. } => value.push_str(text),
            Screen::Sure { typed, .. } => typed.push_str(text),
            _ => {}
        }
    }

    fn on_ui(&mut self, ev: ui::Event) {
        let Screen::Work(w) = &mut self.screen else { return };
        match ev {
            ui::Event::Line(text) => {
                for line in text.split('\n') {
                    if !line.trim().is_empty() {
                        w.steps.push(line.trim_end().to_string());
                    }
                }
            }
            ui::Event::Progress { label, done, total } => {
                if w.label != label {
                    w.label = label;
                    w.started = Some(Instant::now());
                }
                w.done = done;
                w.total = total;
            }
            ui::Event::ProgressEnd { label } => {
                w.steps.push(format!("{TICK} {label}"));
                w.label.clear();
            }
        }
    }
}

/// `3:Install macOS Sequoia` -> the slot number + name; file items are plain paths.
fn item_name(item: &str) -> String {
    match item.split_once(':') {
        Some((n, name)) if n.parse::<usize>().is_ok() => name.to_string(),
        _ => item.to_string(),
    }
}

fn removable_items(disk: &str, section: bool) -> Result<Vec<(String, String)>> {
    let mut d = layout::Disk::open(disk, None, false, true)?;
    if section {
        let g = d.read_gpt()?;
        layout::check_layout(&g)?;
        return Ok(layout::mac_slots(&g)
            .into_iter()
            .map(|(n, p)| (format!("{}  ({})", p.name, human(p.size())), format!("{n}:{}", p.name)))
            .collect());
    }
    let root = d.data_folder().ok_or("Could not open the ISO partition.")?;
    Ok(library::list_files(&root)
        .into_iter()
        .map(|(folder, name, size)| {
            let rel = if folder == "/" { name.clone() } else { format!("{folder}/{name}") };
            (format!("{folder:<8} {name}  ({})", human(size)), rel)
        })
        .collect())
}

// ---------------------------------------------------------------------------
// drawing
// ---------------------------------------------------------------------------
/// Word-wrap plain text to `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = vec![];
    let mut line = String::new();
    for word in text.split(' ') {
        let mut word = word.to_string();
        while word.chars().count() > width {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            out.push(word.chars().take(width).collect());
            word = word.chars().skip(width).collect();
        }
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    out.push(line);
    out
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}
fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}
fn danger() -> Style {
    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
}

/// A choice list; the block as a whole sits in the middle of the panel.
fn list_lines<'a>(items: &[String], sel: usize, width: usize) -> Vec<Line<'a>> {
    let widest = items.iter().map(|i| i.chars().count()).max().unwrap_or(0) + 2;
    let pad = " ".repeat(width.saturating_sub(widest) / 2);
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let text: String = item.chars().take(width.saturating_sub(pad.len() + 2)).collect();
            if i == sel {
                Line::from(vec![Span::raw(pad.clone()), Span::styled(format!("› {text}"), Style::new().add_modifier(Modifier::REVERSED))])
            } else {
                Line::from(format!("{pad}  {text}"))
            }
        })
        .collect()
}

/// A picture of the stick: one coloured block per part, free space shaded.
fn stick_lines<'a>(d: &DiskInfo, width: usize) -> Vec<Line<'a>> {
    let mut lines = vec![Line::styled(
        format!("{}  {}  {}", d.model, human(d.size), d.id.rsplit('/').next().unwrap_or(&d.id)),
        bold(),
    )
    .centered()];
    let used: u64 = d.parts.iter().map(|p| p.size).sum();
    let free = d.size.saturating_sub(used);
    let cell = |size: u64| ((size as u128 * width as u128 / d.size.max(1) as u128) as usize).max(1);
    let mut bar = vec![];
    let mut legend: Vec<Vec<Span>> = vec![vec![]];
    let mut row_len = 0;
    let mut add_legend = |legend: &mut Vec<Vec<Span<'a>>>, text: String, style: Style| {
        let len = text.chars().count() + 4;
        if row_len + len > width && row_len > 0 {
            legend.push(vec![]);
            row_len = 0;
        }
        row_len += len;
        let row = legend.last_mut().expect("legend has a row");
        row.push(Span::styled("■ ", style));
        row.push(Span::raw(format!("{text}  ")));
    };
    for (i, p) in d.parts.iter().enumerate() {
        let style = Style::new().fg(PART_COLORS[i % PART_COLORS.len()]);
        bar.push(Span::styled("█".repeat(cell(p.size)), style));
        let name = if p.label == "VTOYEFI" || p.name == "sycrOS-boot" {
            "boot".to_string()
        } else if p.label.is_empty() {
            format!("part {}", i + 1)
        } else {
            p.label.replace("Install macOS ", "")
        };
        add_legend(&mut legend, format!("{name} {}", human(p.size)), style);
    }
    if free > d.size / 200 {
        bar.push(Span::styled("░".repeat(cell(free)), dim()));
        add_legend(&mut legend, format!("free {}", human(free)), dim());
    }
    lines.push(Line::from(bar).centered());
    lines.extend(legend.into_iter().map(|spans| Line::from(spans).centered()));
    lines
}

/// Title, body and key hints of the current screen.
fn view<'a>(app: &App, width: usize, height: usize) -> (String, Vec<Line<'a>>, &'static str) {
    match &app.screen {
        Screen::Home => {
            let mut body = vec![];
            let sticks = offered(&app.disks);
            match sticks.first() {
                Some(d) => {
                    body.extend(stick_lines(d, width));
                    if sticks.len() > 1 {
                        body.push(Line::styled(format!("+ {} more", sticks.len() - 1), dim()).centered());
                    }
                }
                None if !app.disks_read => body.push(Line::styled("Looking for USB sticks ...", dim()).centered()),
                None => body.push(Line::styled("No USB stick found. Plug one in: it will appear here.", dim()).centered()),
            }
            if !app.admin {
                let how = if hosts::IS_WIN { "open as Administrator" } else { "run: sudo sycros" };
                body.push(Line::styled(format!("To change a stick, {how}"), Style::new().fg(Color::Yellow)).centered());
            }
            if let Some(v) = &app.latest {
                body.push(Line::styled(format!("New version {v} is available"), Style::new().fg(Color::Yellow)).centered());
            }
            body.push(Line::raw(""));
            let items: Vec<String> = home_items(&app.latest).iter().map(|(label, _)| label.clone()).collect();
            body.extend(list_lines(&items, app.home_sel, width));
            (format!(" sycrOS {} ", env!("CARGO_PKG_VERSION")), body, " ↑↓ choose   Enter select   q quit ")
        }
        Screen::PickDisk { sel } => {
            let sticks = offered(&app.disks);
            let mut items: Vec<String> = sticks
                .iter()
                .map(|d| format!("{:<9} {:>9}  {:<18} {}", d.id.rsplit('/').next().unwrap_or(&d.id), human(d.size), d.model, hosts::describe(&d.parts)))
                .collect();
            items.push("Type a disk name or image file ...".into());
            let mut body = vec![];
            if sticks.is_empty() {
                body.push(Line::styled("No USB stick found. Plug one in: it will appear here.", dim()).centered());
                body.push(Line::raw(""));
            }
            body.extend(list_lines(&items, *sel, width));
            let hidden = not_offered(&app.disks);
            if !hidden.is_empty() {
                body.push(Line::raw(""));
                body.push(Line::styled("Not offered, these look like a computer's disk:", dim()).centered());
                for d in hidden {
                    body.push(Line::styled(format!("{} {} {}: {}", d.id, human(d.size), d.model, hosts::describe(&d.parts)), dim()).centered());
                }
            }
            (" Which stick? ".to_string(), body, " ↑↓ choose   Enter select   Esc back ")
        }
        Screen::Ask { kind, value, error } => {
            let (title, help): (&str, Vec<&str>) = match kind {
                Ask::Disk => ("Which disk?", vec!["Linux /dev/sdb, Mac /dev/disk4, Windows 2, or an image file."]),
                Ask::MacSpace => (
                    "Space for macOS images",
                    vec!["One full installer 18G, two installers 34G, Recovery about 2G.", "This is fixed now. Changing it later erases the stick. 0 = no macOS."],
                ),
                Ask::ImagePath => ("Image file to create", vec!["Name or path of the new file."]),
                Ask::ImageSize => ("Size of the image", vec!["Same as or smaller than your USB stick."]),
                Ask::Files => ("Which files?", vec!["Drag files into this window, or type their paths.", "Put a space between files."]),
            };
            let mut body: Vec<Line> = help.iter().map(|h| Line::raw(h.to_string()).centered()).collect();
            body.push(Line::raw(""));
            // long dragged paths: show the end, where the file name is
            let room = width.saturating_sub(4);
            let count = value.chars().count();
            let shown: String = value.chars().skip(count.saturating_sub(room)).collect();
            body.push(Line::from(vec![Span::styled("> ", bold()), Span::raw(shown), Span::styled("_", dim())]));
            if !error.is_empty() {
                body.push(Line::raw(""));
                body.extend(wrap(error, width).into_iter().map(|l| Line::styled(l, danger())));
            }
            (format!(" {title} "), body, " Enter continue   Esc back ")
        }
        Screen::Sure { title, lines, words, typed, .. } => {
            let mut body: Vec<Line> = vec![];
            for l in lines {
                let style = if l.contains("ERASED") || l.contains("OVERWRITTEN") || l.contains("NOT") { danger() } else { Style::new() };
                body.extend(wrap(l, width).into_iter().map(|w| Line::styled(w, style)));
            }
            body.push(Line::raw(""));
            body.push(Line::from(vec![Span::raw("To continue, type:  "), Span::styled(words.clone(), bold())]));
            body.push(Line::raw(""));
            let good = typed.trim() == words.as_str();
            body.push(Line::from(vec![
                Span::styled("> ", bold()),
                Span::raw(typed.clone()),
                Span::styled("_", dim()),
                Span::styled(if good { format!("   {TICK} press Enter") } else { String::new() }, Style::new().fg(Color::Green)),
            ]));
            (format!(" {title} "), body, " Enter continue   Esc cancel ")
        }
        Screen::PickItem { title, items, sel } => {
            let names: Vec<String> = items.iter().map(|(label, _)| label.clone()).collect();
            (format!(" {title} "), list_lines(&names, *sel, width), " ↑↓ choose   Enter select   Esc back ")
        }
        Screen::Work(w) => view_work(w, app.frame, width, height),
    }
}

fn view_work<'a>(w: &Work, frame: usize, width: usize, height: usize) -> (String, Vec<Line<'a>>, &'static str) {
    let mut body = vec![];
    let mut all: Vec<Line> = vec![];
    for s in &w.steps {
        let style = if s.starts_with(TICK) { Style::new().fg(Color::Green) } else { Style::new() };
        all.extend(wrap(s, width).into_iter().map(|l| Line::styled(l, style)));
    }
    let mut tail = vec![];
    match &w.result {
        None if !w.label.is_empty() => {
            let pct = if w.total == 0 { 0 } else { (w.done as u128 * 100 / w.total as u128) as usize };
            let elapsed = w.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
            let left = if w.done > 0 && elapsed > 2.0 {
                let secs = elapsed * (w.total.saturating_sub(w.done)) as f64 / w.done as f64;
                if secs >= 90.0 { format!("  about {} min left", (secs / 60.0).ceil() as u64) } else { format!("  about {} s left", secs.ceil() as u64) }
            } else {
                String::new()
            };
            let bar_w = width.saturating_sub(8);
            let full = bar_w * pct.min(100) / 100;
            tail.push(Line::from(vec![Span::raw(SPINNER[frame % SPINNER.len()].to_string()), Span::raw(format!(" {}", w.label))]));
            tail.push(Line::from(vec![
                Span::styled("█".repeat(full), Style::new().fg(Color::Blue)),
                Span::styled("░".repeat(bar_w - full), dim()),
                Span::raw(format!(" {pct:>3}%")),
            ]));
            tail.push(Line::styled(format!("{} / {}{left}", human(w.done), human(w.total)), dim()));
        }
        None => tail.push(Line::from(format!("{} Working ...", SPINNER[frame % SPINNER.len()]))),
        Some(Ok(())) => {
            tail.push(Line::raw(""));
            tail.push(Line::styled(format!("{TICK} Done"), Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)));
        }
        Some(Err(e)) => {
            tail.push(Line::raw(""));
            tail.extend(wrap(e, width).into_iter().map(|l| Line::styled(l, danger())));
        }
    }
    if w.stop_asked {
        tail.push(Line::styled("Press Ctrl+C again to stop. The stick may be left unfinished.", danger()));
    }
    let room = height.saturating_sub(tail.len());
    let last_start = all.len().saturating_sub(room);
    let start = last_start.saturating_sub(w.scroll);
    body.extend(all.into_iter().skip(start).take(room));
    body.extend(tail);
    let hint = if w.result.is_some() { " Enter back to the menu   ↑↓ scroll " } else { " Please wait. Do not unplug the stick. " };
    (format!(" {} ", w.title), body, hint)
}

/// Panel rectangle: centred both ways in the window.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width < 50 || area.height < 16 {
        f.render_widget(Paragraph::new("Please make this window bigger.").centered(), centred(area, 40, 1));
        return;
    }
    let panel_w = PANEL_W.min(area.width);
    let inner_w = panel_w.saturating_sub(4) as usize;
    let inner_h = area.height.saturating_sub(4) as usize;
    let (title, mut lines, hint) = view(app, inner_w, inner_h);
    lines.truncate(inner_h);
    let panel = centred(area, panel_w, lines.len() as u16 + 4);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::styled(title, bold()).centered())
        .title_bottom(Line::styled(hint, dim()).centered())
        .padding(ratatui::widgets::Padding::new(1, 1, 1, 1));
    f.render_widget(Paragraph::new(lines).block(block), panel);
}

// ---------------------------------------------------------------------------
// the loop
// ---------------------------------------------------------------------------
enum Msg {
    Latest(String),
    Disks(Vec<DiskInfo>),
    Finished(std::result::Result<(), String>),
}

pub fn run() -> Result<()> {
    let (ui_tx, ui_rx) = channel();
    ui::attach(ui_tx);
    let admin = hosts::is_admin();

    let (tx, rx) = channel();
    let paused = Arc::new(AtomicBool::new(false));
    let refresh = Arc::new(AtomicBool::new(true));
    {
        // Notice sticks being plugged in and out. Paused during work.
        let (tx, paused, refresh) = (tx.clone(), paused.clone(), refresh.clone());
        std::thread::spawn(move || {
            let every = if hosts::IS_WIN { 50 } else { 20 }; // tenths of a second
            let mut wait = 0;
            loop {
                if refresh.swap(false, Ordering::Relaxed) || (wait >= every && !paused.load(Ordering::Relaxed)) {
                    wait = 0;
                    if let Ok(disks) = hosts::list_disks() {
                        if tx.send(Msg::Disks(disks)).is_err() {
                            return;
                        }
                    }
                }
                wait += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }

    // One small check for a newer version, in the background. If it fails, nothing is shown.
    if std::env::var_os("SYCROS_NO_UPDATE_CHECK").is_none() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Ok(Some(v)) = update::newer_version() {
                let _ = tx.send(Msg::Latest(v));
            }
        });
    }

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let result = run_loop(&mut terminal, App::new(admin), &rx, &ui_rx, &tx, &paused, &refresh);
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    mut app: App,
    rx: &Receiver<Msg>,
    ui_rx: &Receiver<ui::Event>,
    tx: &std::sync::mpsc::Sender<Msg>,
    paused: &Arc<AtomicBool>,
    refresh: &Arc<AtomicBool>,
) -> Result<()> {
    let mut dirty = true;
    while !app.quit {
        if let Some(cmd) = app.pending.take() {
            paused.store(true, Ordering::Relaxed);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(Msg::Finished(run_cmd(cmd).map_err(|e| e.to_string())));
            });
            dirty = true;
        }
        while let Ok(ev) = ui_rx.try_recv() {
            app.on_ui(ev);
            dirty = true;
        }
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Latest(v) => {
                    app.latest = Some(v);
                    dirty = true;
                }
                Msg::Disks(disks) => {
                    if !app.disks_read || disks != app.disks {
                        app.disks = disks;
                        app.disks_read = true;
                        dirty = true;
                    }
                }
                Msg::Finished(result) => {
                    if app.flow.action == Action::Update && result.is_ok() {
                        app.latest = None;
                        let items = home_items(&None).len();
                        app.home_sel = app.home_sel.min(items - 1);
                    }
                    // collect the last messages sent before the command ended
                    while let Ok(ev) = ui_rx.try_recv() {
                        app.on_ui(ev);
                    }
                    if let Screen::Work(w) = &mut app.screen {
                        w.result = Some(result);
                        w.stop_asked = false;
                    }
                    paused.store(false, Ordering::Relaxed);
                    refresh.store(true, Ordering::Relaxed);
                    dirty = true;
                }
            }
        }
        // The spinner only moves while work is running; otherwise nothing is redrawn.
        if app.working() {
            app.frame = app.frame.wrapping_add(1);
            dirty = true;
        }
        if dirty {
            terminal.draw(|f| draw(f, &app))?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(if app.working() { 120 } else { 200 }))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                    if ctrl && matches!(k.code, KeyCode::Char('c')) && app.working() {
                        if let Screen::Work(w) = &mut app.screen {
                            if w.stop_asked {
                                let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
                                ratatui::restore();
                                std::process::exit(130);
                            }
                            w.stop_asked = true;
                        }
                    } else {
                        app.key(k.code, ctrl);
                    }
                    dirty = true;
                }
                Event::Paste(text) => {
                    app.paste(&text);
                    dirty = true;
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use sycros::hosts::DiskPart;

    fn stick() -> DiskInfo {
        let part = |label: &str, size: u64| DiskPart { label: label.into(), fs: String::new(), kind: String::new(), name: label.into(), size };
        DiskInfo {
            id: "/dev/sdb".into(),
            path: "/dev/sdb".into(),
            size: 57 << 30,
            model: "SanDisk 3.2Gen1".into(),
            removable: true,
            system: false,
            parts: vec![part("sycrOS", 23 << 30), part("VTOYEFI", 32 << 20), part("Install macOS Sequoia", 16 << 30)],
        }
    }

    fn windows_usb_ssd() -> DiskInfo {
        let mut d = stick();
        d.id = "/dev/sdc".into();
        d.model = "Samsung T7".into();
        d.parts = vec![DiskPart { label: "C".into(), fs: "ntfs".into(), kind: "e3c9e316-0b5c-4db8-817d-f92df00215ae".into(), name: String::new(), size: 1 << 40 }];
        d
    }

    fn screen(app: &App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    }

    fn app() -> App {
        let mut a = App::new(true);
        a.disks = vec![stick()];
        a.disks_read = true;
        a
    }

    /// The panel's blank margins must match on both sides, within one column/row.
    #[test]
    fn panel_is_centred_in_any_window() {
        for (w, h) in [(80, 24), (120, 40), (101, 33), (70, 22), (200, 60)] {
            let rows = screen(&app(), w, h);
            let top = rows.iter().position(|r| r.contains('╭')).expect("panel top");
            let bottom = rows.iter().position(|r| r.contains('╰')).expect("panel bottom");
            assert!((top as i32 - (h as i32 - 1 - bottom as i32)).abs() <= 1, "{w}x{h}: {top} {bottom}");
            let left = rows[top].chars().position(|c| c == '╭').unwrap();
            let right = rows[top].chars().count() - 1 - rows[top].chars().position(|c| c == '╮').unwrap();
            assert!((left as i32 - right as i32).abs() <= 1, "{w}x{h}: {left} {right}");
        }
    }

    #[test]
    fn home_shows_stick_and_all_choices() {
        let text = screen(&app(), 80, 24).join("\n");
        for want in ["SanDisk 3.2Gen1", "Sequoia", "boot", "Prepare a USB stick", "Safely remove a stick", "Quit"] {
            assert!(text.contains(want), "{want}\n{text}");
        }
        println!("{text}");
    }

    #[test]
    fn usb_ssd_with_windows_is_not_offered() {
        let mut a = app();
        a.disks.push(windows_usb_ssd());
        assert_eq!(offered(&a.disks).len(), 1);
        a.flow = Flow::new(Action::Prepare);
        a.step();
        assert!(matches!(a.screen, Screen::PickDisk { .. }));
        let text = screen(&a, 90, 30).join("\n");
        assert!(text.contains("Not offered"), "{text}");
        assert!(text.contains("Samsung T7"), "{text}");
    }

    /// Erasing: it always asks which stick, shows what is on it, and only the
    /// exact words let it continue.
    #[test]
    fn erase_needs_the_exact_words() {
        let mut a = app();
        a.flow = Flow::new(Action::Prepare);
        a.step();
        assert!(matches!(a.screen, Screen::PickDisk { .. }), "asks even with one stick");
        a.key(KeyCode::Enter, false);
        assert!(matches!(a.screen, Screen::Ask { kind: Ask::MacSpace, .. }));
        a.key(KeyCode::Enter, false); // default 34G
        assert!(matches!(a.screen, Screen::Sure { kind: Sure::Erase, .. }));
        let text = screen(&a, 80, 24).join("\n");
        assert!(text.contains("sycrOS stick") && text.contains("ERASE sdb"), "{text}");
        for c in "ERASE sda".chars() {
            a.key(KeyCode::Char(c), false);
        }
        a.key(KeyCode::Enter, false);
        assert!(matches!(a.screen, Screen::Sure { .. }), "wrong disk name must not continue");
        a.key(KeyCode::Backspace, false);
        a.key(KeyCode::Char('b'), false);
        a.key(KeyCode::Enter, false);
        assert!(matches!(a.screen, Screen::Work(_)));
        assert!(matches!(a.pending, Some(Cmd::Init { .. })));
    }

    #[test]
    fn update_choice_appears_only_when_a_new_version_exists() {
        let mut a = app();
        assert!(!screen(&a, 80, 30).join("\n").contains("Update sycrOS"));
        a.latest = Some("9.9.9".into());
        let text = screen(&a, 80, 30).join("\n");
        assert!(text.contains("New version 9.9.9") && text.contains("Update sycrOS to 9.9.9"), "{text}");
        // and everything still fits in the smallest window the app opens in
        assert!(screen(&a, MIN_W, MIN_H).join("\n").contains("Quit"));
    }

    #[test]
    fn dragged_paste_lands_in_the_box_and_long_paths_show_their_end() {
        let mut a = app();
        a.flow = Flow::new(Action::Check);
        a.step();
        a.paste("/a/very/long/folder/name/that/goes/on/and/on/and/on/and/on/and/on/and/on/and/on/macOS_Sequoia.dmg.img");
        let text = screen(&a, 80, 24).join("\n");
        assert!(text.contains("macOS_Sequoia.dmg.img"), "{text}");
    }

    #[test]
    fn progress_shows_bar_and_time_left() {
        let mut a = app();
        a.screen = Screen::Work(Work { title: "Adding files".into(), started: Some(Instant::now()), ..Default::default() });
        a.on_ui(ui::Event::Line("found HFS+ volume".into()));
        a.on_ui(ui::Event::Progress { label: "writing macOS".into(), done: 8 << 30, total: 16 << 30 });
        let text = screen(&a, 80, 24).join("\n");
        assert!(text.contains("writing macOS") && text.contains("50%") && text.contains("8.0 GB / 16.0 GB"), "{text}");
        a.on_ui(ui::Event::ProgressEnd { label: "writing macOS".into() });
        assert!(screen(&a, 80, 24).join("\n").contains(&format!("{TICK} writing macOS")));
    }
}
