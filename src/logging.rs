use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct LogFile {
    file: File,
    path: PathBuf,
    written: u64,
}

static LOG: OnceLock<Mutex<Option<LogFile>>> = OnceLock::new();

pub fn init(path: Option<&Path>) {
    let _ = LOG.set(Mutex::new(path.and_then(open)));
}

fn open(path: &Path) -> Option<LogFile> {
    rotate(path);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    let written = file.metadata().map_or(0, |meta| meta.len());
    Some(LogFile {
        file,
        path: path.to_path_buf(),
        written,
    })
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    path.with_file_name(name)
}

fn rotate(path: &Path) {
    if fs::metadata(path).is_ok_and(|meta| meta.len() >= MAX_LOG_BYTES) {
        let old = rotated_path(path);
        let _ = fs::remove_file(&old);
        let _ = fs::rename(path, &old);
    }
}

fn rotate_now(log: &mut LogFile) {
    let old = rotated_path(&log.path);
    let _ = fs::remove_file(&old);
    let _ = fs::rename(&log.path, &old);
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&log.path) {
        log.file = file;
        log.written = 0;
    }
}

pub fn write(message: fmt::Arguments<'_>) {
    let line = format!("[{}] {message}", clock());
    eprintln!("{line}");
    if let Some(log) = LOG.get()
        && let Ok(mut log) = log.lock()
        && let Some(log) = log.as_mut()
    {
        let _ = writeln!(log.file, "{line}");
        log.written = log
            .written
            .saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX) + 1);
        if log.written >= MAX_LOG_BYTES {
            rotate_now(log);
        }
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
