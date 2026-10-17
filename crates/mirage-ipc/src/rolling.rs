//! Bounded, date-stamped log files (DESIGN.md §18): one file per day, capped
//! at 10 MiB, and files older than 7 days are deleted on every roll.
//!
//! `tracing-appender`'s own rolling appender has no per-file size cap, so the
//! same writer is used for the application log and for the core's stdout and
//! stderr, which are the two logs §18 bounds by these numbers.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// §18: single file cap.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// §18: files older than this are removed.
pub const KEEP_DAYS: u64 = 7;
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// Writes to `<stem>-<date>.log`, rolling to `-<date>.<n>.log` when the
/// current file would pass the cap. A roll also prunes old files.
pub struct RollingWriter {
    stem: PathBuf,
    max_bytes: u64,
    state: Mutex<State>,
}

struct State {
    file: File,
    path: PathBuf,
    /// Day number the current file belongs to.
    day: u64,
    /// Index within the day; 0 is unsuffixed.
    seq: u32,
    written: u64,
}

impl RollingWriter {
    /// Writes to the current segment, rolling first when the date changed or
    /// the cap would be passed. Shared by `Write` and by the core-log pump.
    pub fn append(&self, data: &[u8]) -> io::Result<()> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            // A panicking writer must not take the whole logging system down;
            // the file handle itself is still usable.
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.day != today() || state.written + data.len() as u64 > self.max_bytes {
            self.roll(&mut state)?;
        }
        state.file.write_all(data)?;
        state.written += data.len() as u64;
        Ok(())
    }
    /// Opens (or creates) today's file and prunes old ones.
    pub fn open(stem: PathBuf, max_bytes: u64) -> io::Result<Self> {
        let day = today();
        let (file, path, written) = open_segment(&stem, day, max_bytes)?;
        prune(&stem, day);
        Ok(Self {
            stem,
            max_bytes,
            state: Mutex::new(State {
                file,
                path,
                day,
                seq: 0,
                written,
            }),
        })
    }

    /// Current file path, e.g. for a tail read after an early core exit.
    #[allow(dead_code)]
    pub fn current_path(&self) -> PathBuf {
        match self.state.lock() {
            Ok(state) => state.path.clone(),
            Err(poisoned) => poisoned.into_inner().path.clone(),
        }
    }

    /// Moves to the next segment, then prunes expired days.
    fn roll(&self, state: &mut State) -> io::Result<()> {
        let _ = state.file.flush();
        let day = today();
        let (day, seq) = if day == state.day {
            (state.day, state.seq + 1)
        } else {
            (day, 0)
        };
        let (file, path, written) = open_segment(&self.stem, day, self.max_bytes)?;
        state.file = file;
        state.path = path;
        state.day = day;
        state.seq = seq;
        state.written = written;
        prune(&self.stem, day);
        Ok(())
    }
}

impl Write for RollingWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.append(data)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.state.lock() {
            Ok(mut state) => state.file.flush(),
            Err(poisoned) => poisoned.into_inner().file.flush(),
        }
    }
}

/// Opens `<stem>-<date>.log` (seq 0) or `<stem>-<date>.<seq>.log`, appending.
/// Falls back to a fresh segment when the existing one already exceeds the cap.
fn open_segment(stem: &Path, day: u64, max_bytes: u64) -> io::Result<(File, PathBuf, u64)> {
    let mut seq = 0u32;
    loop {
        let path = segment_path(stem, day, seq);
        let existing = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if existing < max_bytes || seq >= MAX_SEGMENTS {
            let file = open_append(&path)?;
            return Ok((file, path, existing));
        }
        seq += 1;
    }
}

/// Enough room for a very chatty day without unbounded names.
const MAX_SEGMENTS: u32 = 99;

fn open_append(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn segment_path(stem: &Path, day: u64, seq: u32) -> PathBuf {
    let base = stem.with_extension("");
    let name = base
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "log".into());
    let date = date_text(day);
    let file = if seq == 0 {
        format!("{name}-{date}.log")
    } else {
        format!("{name}-{date}.{seq}.log")
    };
    base.with_file_name(file)
}

/// Newest existing segment for `stem`, so a tail read still finds the log
/// after the file rolled. Returns `stem` itself when nothing matches yet.
pub fn newest_segment(stem: &Path) -> PathBuf {
    let Some(dir) = stem.parent() else {
        return stem.to_path_buf();
    };
    let base = stem
        .with_extension("")
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Ok(entries) = fs::read_dir(dir) else {
        return stem.to_path_buf();
    };
    let mut best: Option<(u64, u32, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((day, seq)) = parse_segment(&base, &name) else {
            continue;
        };
        if best.as_ref().is_none_or(|(d, s, _)| (day, seq) > (*d, *s)) {
            best = Some((day, seq, entry.path()));
        }
    }
    best.map(|(_, _, path)| path)
        .unwrap_or_else(|| stem.to_path_buf())
}

/// `mirage-2026-10-16.log` / `mirage-2026-10-16.2.log` → `(day, seq)`.
fn parse_segment(base: &str, name: &str) -> Option<(u64, u32)> {
    let rest = name.strip_prefix(base)?.strip_prefix('-')?;
    let rest = rest.strip_suffix(".log")?;
    let (date, seq) = match rest.split_once('.') {
        Some((date, seq)) => (date, seq.parse().ok()?),
        None => (rest, 0),
    };
    Some((parse_day(date)?, seq))
}

/// Date of a file written by the previous `tracing-appender` scheme,
/// `mirage.2026-10-15.log` (a dot, not a dash, before the date). Those files
/// are never appended to again, but they must still expire like the new ones.
fn parse_legacy(base: &str, name: &str) -> Option<u64> {
    let rest = name.strip_prefix(base)?.strip_prefix('.')?;
    let date = rest.strip_suffix(".log")?;
    parse_day(date)
}

/// `mirage-2026-10-16.log` / `mirage-2026-10-16.2.log` → day number.
fn parse_day(date: &str) -> Option<u64> {
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let date = chrono::NaiveDate::from_ymd_opt(year as i32, month, day)?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp() as u64 / SECS_PER_DAY)
}

/// Removes segments of `stem` whose date is older than `KEEP_DAYS`.
fn prune(stem: &Path, today_day: u64) {
    let Some(dir) = stem.parent() else { return };
    let base = stem
        .with_extension("")
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let day = match parse_segment(&base, &name) {
            Some((day, _)) => day,
            // Files from the pre-M6 scheme expire by the same rule.
            None => match parse_legacy(&base, &name) {
                Some(day) => day,
                None => continue,
            },
        };
        if today_day.saturating_sub(day) < KEEP_DAYS {
            continue;
        }
        if let Err(e) = fs::remove_file(entry.path()) {
            tracing::debug!(error = %e, file = %name, "old log not removed");
        }
    }
}

/// Days since the Unix epoch, local time, so the file name matches the clock
/// the user sees.
fn today() -> u64 {
    let now = chrono::Local::now().date_naive();
    (now.and_hms_opt(0, 0, 0)
        .map(|d| d.and_utc().timestamp())
        .unwrap_or_default()
        .max(0) as u64)
        / SECS_PER_DAY
}

fn date_text(day: u64) -> String {
    let secs = (day * SECS_PER_DAY) as i64;
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".into())
}
