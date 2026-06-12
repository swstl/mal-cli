use std::io::Write;
use std::sync::mpsc::Sender;
use once_cell::sync::OnceCell;
use crate::app::Event;
use crate::config::Config;

static DISPATCH_TX: OnceCell<Sender<Event>> = OnceCell::new();

pub fn init(tx: Sender<Event>) {
    let _ = DISPATCH_TX.set(tx);
}

pub fn dispatch(ev: Event) {
    if let Some(tx) = DISPATCH_TX.get() {
        let _ = tx.send(ev);
    } else {
        eprintln!("[no bus] {:?}", ev);
    }
}

pub fn error<S: Into<String>>(msg: S) {
    dispatch(Event::ShowError(msg.into()));
}

// append the error to the configured log file, when logging is enabled.
// called from ScreenManager::show_error so that EVERY displayed error is
// captured, regardless of which path (send_error!, Action::ShowError, sync
// failures, ...) produced it.
// failures here are silent on purpose: logging must never itself raise an
// error (that would recurse back through the error machinery).
pub fn log_to_file(msg: &str) {
    let Some(config) = Config::try_global() else {
        return;
    };
    if !config.logging.enabled {
        return;
    }

    let path = config.logging.resolve_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(file, "[{}] {}", timestamp, msg);
    }
}
