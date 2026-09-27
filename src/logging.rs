use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: OnceLock<Mutex<Option<File>>> = OnceLock::new();

pub fn init(path: Option<&Path>) {
    let file = path.and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());
    let _ = LOG.set(Mutex::new(file));
}

pub fn write(message: fmt::Arguments<'_>) {
    let line = format!("[{}] {message}", clock());
    eprintln!("{line}");
    if let Some(log) = LOG.get()
        && let Ok(mut log) = log.lock()
        && let Some(file) = log.as_mut()
    {
        let _ = writeln!(file, "{line}");
    }
}

fn clock() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |delta| delta.as_secs());
    format!(
        "{:02}:{:02}:{:02}Z",
        (seconds / 3600) % 24,
        (seconds / 60) % 60,
        seconds % 60
    )
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::logging::write(format_args!($($arg)*))
    };
}
