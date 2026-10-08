// Tiny logger: writes to <data>/mixdeck.log and keeps the last lines in memory so the
// Settings page can show them (a GUI app has no console to look at).
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;

struct Logger {
    file: Option<PathBuf>,
    recent: Mutex<VecDeque<String>>,
}

static LOGGER: OnceCell<Logger> = OnceCell::new();

pub fn init(data_dir: &std::path::Path) {
    let file = data_dir.join("mixdeck.log");
    // Keep the log from growing forever: start fresh when it passes 2 MB.
    if std::fs::metadata(&file).map(|m| m.len() > 2_000_000).unwrap_or(false) {
        let _ = std::fs::rename(&file, data_dir.join("mixdeck.old.log"));
    }
    let _ = LOGGER.set(Logger { file: Some(file), recent: Mutex::new(VecDeque::new()) });
}

pub fn log(msg: impl AsRef<str>) {
    let line = format!("{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), msg.as_ref());
    eprintln!("{line}");
    if let Some(l) = LOGGER.get() {
        {
            let mut r = l.recent.lock();
            r.push_back(line.clone());
            while r.len() > 300 { r.pop_front(); }
        }
        if let Some(f) = &l.file {
            if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
                let _ = writeln!(fh, "{line}");
            }
        }
    }
}

pub fn recent() -> Vec<String> {
    LOGGER.get().map(|l| l.recent.lock().iter().cloned().collect()).unwrap_or_default()
}

#[macro_export]
macro_rules! logf {
    ($($t:tt)*) => { $crate::logger::log(format!($($t)*)) };
}
