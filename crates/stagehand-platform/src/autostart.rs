//! Starting Stagehand when the user signs in.
//!
//! On Windows this is the per-user `Run` registry key, or the package's startup task when
//! Stagehand runs from an installed package: Windows does not honour `Run` entries
//! written by packaged apps, and the user manages a startup task in Windows' own settings.

use std::io;
use std::path::Path;

use windows_registry::CURRENT_USER;

use crate::package;

/// Passed on the command line when the system starts Stagehand at sign-in, so that it
/// comes up in the background without opening the settings window.
pub const BACKGROUND_ARG: &str = "--background";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Stagehand";

pub fn is_enabled() -> bool {
    if package::is_packaged() {
        package::startup_task_enabled()
    } else {
        RunEntry::CURRENT_USER.exists()
    }
}

/// Switches starting at sign-in on or off. Switching it on registers the executable
/// that is running now, so calling it again after the app has moved repairs the entry.
pub fn set_enabled(enabled: bool) -> io::Result<()> {
    if package::is_packaged() {
        return package::set_startup_task_enabled(enabled);
    }
    if enabled {
        let executable = std::env::current_exe()?;
        RunEntry::CURRENT_USER.write(&command_line(&executable))
    } else {
        RunEntry::CURRENT_USER.remove()
    }
}

fn command_line(executable: &Path) -> String {
    format!("\"{}\" {BACKGROUND_ARG}", executable.display())
}

struct RunEntry {
    key: &'static str,
    name: &'static str,
}

impl RunEntry {
    const CURRENT_USER: Self = Self {
        key: RUN_KEY,
        name: VALUE_NAME,
    };

    fn exists(&self) -> bool {
        CURRENT_USER
            .open(self.key)
            .and_then(|key| key.get_string(self.name))
            .is_ok()
    }

    fn write(&self, command: &str) -> io::Result<()> {
        CURRENT_USER
            .create(self.key)
            .and_then(|key| key.set_string(self.name, command))
            .map_err(io::Error::from)
    }

    fn remove(&self) -> io::Result<()> {
        if !self.exists() {
            return Ok(());
        }
        CURRENT_USER
            .create(self.key)
            .and_then(|key| key.remove_value(self.name))
            .map_err(io::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A key of our own, so the tests never touch the user's real startup entries.
    const TEST_KEY: &str = r"Software\Stagehand.Tests\Autostart";

    #[test]
    fn command_line_quotes_the_path_and_asks_for_a_background_start() {
        let command = command_line(Path::new(r"C:\Program Files\Stagehand\stagehand.exe"));

        assert_eq!(
            command,
            r#""C:\Program Files\Stagehand\stagehand.exe" --background"#
        );
    }

    #[test]
    fn entry_can_be_written_read_and_removed() {
        let entry = RunEntry {
            key: TEST_KEY,
            name: "RoundTrip",
        };
        entry.remove().unwrap();
        assert!(!entry.exists());

        entry.write("\"stagehand.exe\" --background").unwrap();
        assert!(entry.exists());
        let stored = CURRENT_USER
            .open(TEST_KEY)
            .and_then(|key| key.get_string("RoundTrip"))
            .unwrap();
        assert_eq!(stored, "\"stagehand.exe\" --background");

        entry.remove().unwrap();
        assert!(!entry.exists());
        // Removing an entry that is already gone is not an error.
        entry.remove().unwrap();

        let _ = CURRENT_USER.remove_tree(r"Software\Stagehand.Tests");
    }
}
