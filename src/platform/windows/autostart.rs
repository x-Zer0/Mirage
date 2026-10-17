//! Run-key autostart (DESIGN.md §11.6). The entry lives in
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` as
//! `Mirage = "<exe>" --autostart`; the settings flag only records the intent,
//! so a value removed by someone else is repaired on the next save.

use super::registry::{self, Root};
use std::io;
use std::path::Path;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Mirage";
/// Read as `--autostart` by `main` (§11.6).
pub const ARG: &str = "--autostart";

/// The command line Windows will run: `"<exe>" --autostart`.
pub fn command_line(exe: &Path) -> String {
    format!("\"{}\" {ARG}", exe.display())
}

/// Writes (or rewrites) the Run entry for the current executable.
pub fn enable() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    registry::write_string(Root::CurrentUser, RUN_KEY, VALUE_NAME, &command_line(&exe))
}

/// Removes the entry; missing is success.
pub fn disable() -> io::Result<()> {
    registry::delete_value(Root::CurrentUser, RUN_KEY, VALUE_NAME)
}

/// The value currently in the registry, if any (not necessarily ours).
pub fn read() -> io::Result<Option<String>> {
    registry::read_string(Root::CurrentUser, RUN_KEY, VALUE_NAME)
}

/// `true` when the entry exists and still names this executable.
pub fn is_current() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    read()
        .ok()
        .flatten()
        .is_some_and(|value| value.eq_ignore_ascii_case(&command_line(&exe)))
}

/// Brings the registry in line with the saved preference; returns `true` when
/// a change was made. Called at startup and after the setting is saved, so a
/// value deleted outside Mirage (or a moved executable) is repaired.
pub fn apply(wanted: bool) -> io::Result<bool> {
    if wanted {
        if is_current() {
            return Ok(false);
        }
        enable()?;
        Ok(true)
    } else {
        if read()?.is_none() {
            return Ok(false);
        }
        disable()?;
        Ok(true)
    }
}
