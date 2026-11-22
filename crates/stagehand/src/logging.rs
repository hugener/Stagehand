//! Writes what Stagehand and its plugins log to a file, because a background app has
//! no console to show it on.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use log::{LevelFilter, Log, Metadata, Record};
use stagehand_platform::{clock, paths};

const FILE_NAME: &str = "stagehand.log";
const PREVIOUS_FILE_NAME: &str = "stagehand.previous.log";

/// A log that has grown past this is set aside when Stagehand starts, replacing the one
/// set aside before it, so the logs never take more than about twice this much space.
const MAX_SIZE: u64 = 1024 * 1024;

struct FileLogger {
    file: Mutex<File>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} [{}] {}\n",
            clock::local_timestamp(),
            record.level(),
            record.target(),
            record.args()
        );
        // Debug builds have a console, and seeing the log there saves opening the file.
        #[cfg(debug_assertions)]
        eprint!("{line}");
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        // There is nowhere left to report a failure to write the log.
        let _ = file.write_all(line.as_bytes());
    }

    fn flush(&self) {
        let _ = self
            .file
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush();
    }
}

/// Starts logging to the log file. Until this succeeds, nothing is logged anywhere.
pub fn start() -> io::Result<()> {
    let directory = paths::log_dir()
        .ok_or_else(|| io::Error::other("the system did not say where to keep logs"))?;
    let file = open(&directory)?;
    log::set_boxed_logger(Box::new(FileLogger {
        file: Mutex::new(file),
    }))
    .map_err(io::Error::other)?;
    log::set_max_level(LevelFilter::Info);
    Ok(())
}

fn open(directory: &Path) -> io::Result<File> {
    fs::create_dir_all(directory)?;
    let path = directory.join(FILE_NAME);
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_SIZE) {
        // If the old log cannot be set aside, logging carries on in it.
        let _ = fs::rename(&path, directory.join(PREVIOUS_FILE_NAME));
    }
    OpenOptions::new().create(true).append(true).open(path)
}
