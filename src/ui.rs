//! Where messages and progress go: plain terminal output, or the full-screen app.

use std::sync::mpsc::Sender;
use std::sync::OnceLock;

pub enum Event {
    Line(String),
    Progress { label: String, done: u64, total: u64 },
    ProgressEnd { label: String },
}

static SINK: OnceLock<Sender<Event>> = OnceLock::new();

/// Send everything to the full-screen app from now on.
pub fn attach(tx: Sender<Event>) {
    let _ = SINK.set(tx);
}

pub fn attached() -> bool {
    SINK.get().is_some()
}

/// Show one line of text to the user.
pub fn say(text: impl Into<String>) {
    let text = text.into();
    match SINK.get() {
        Some(tx) => {
            let _ = tx.send(Event::Line(text));
        }
        None => println!("{text}"),
    }
}

pub(crate) fn send(ev: Event) {
    if let Some(tx) = SINK.get() {
        let _ = tx.send(ev);
    }
}
