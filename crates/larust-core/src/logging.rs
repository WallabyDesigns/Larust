//! `tracing_subscriber` initialization - `stdout` (the only option before
//! `Config::log_channel` existed, still the default), `file` (writes only
//! to `storage/logs/larust.log`), or `stack` (both at once, Laravel's own
//! `LOG_CHANNEL=stack` meaning) - see `Config::log_channel`'s own doc
//! comment for the exact field-level contract this reads.
//!
//! Rotation is size-based only, not Laravel's full `single`/`daily`/`stack`/
//! `slack` channel menu: `tracing-appender` (the obvious off-the-shelf
//! choice) only ever rotates on a fixed time interval
//! (hourly/daily/never), never on size, and pulling in a second crate just
//! for that one missing strategy wasn't worth it for what's genuinely a
//! small amount of logic - see [`RotatingFile`] below, which is the same
//! "shift `.1` -> `.2`, current -> `.1`, start fresh" scheme `logrotate`
//! itself uses.
//!
//! Deliberately *not* built as `tracing_subscriber::registry()` +
//! multiple `Layer`s (the more "idiomatic" way to send output to two
//! places at once): a single `fmt()` builder with one combined writer
//! keeps this the same shape `init` already was before `stack` existed,
//! and a filter only ever needs constructing once either way.

use crate::config::Config;
use crate::paths::AppPaths;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_subscriber::EnvFilter;

const LOG_FILE_NAME: &str = "larust.log";

pub(crate) fn init(config: &Config, paths: &AppPaths) {
    let filter = build_filter(config);

    match config.log_channel.as_str() {
        // `.with_ansi(false)` on both branches below - a log *file* meant
        // to be `cat`/`grep`/text-editor-read later should never carry raw
        // ANSI color escapes the way an interactive terminal's own output
        // can; confirmed directly, not assumed (a first pass without this
        // wrote the exact same escape-code-laden bytes `stdout` gets into
        // `storage/logs/larust.log`). `"stack"` accepts losing stdout's own
        // color as the trade-off for that - both targets share one
        // formatting layer (see this module's own doc comment on why),
        // so there's no way to color one output and not the other here.
        "file" => match RotatingFileWriter::open(paths.logs().join(LOG_FILE_NAME), config) {
            Ok(writer) => {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_ansi(false)
                    .with_writer(move || writer.clone())
                    .try_init();
            }
            Err(error) => {
                eprintln!(
                    "larust: couldn't open the log file ({error}) - falling back to stdout only"
                );
                let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
            }
        },
        "stack" => match RotatingFileWriter::open(paths.logs().join(LOG_FILE_NAME), config) {
            Ok(file) => {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_ansi(false)
                    .with_writer(move || TeeWriter { file: file.clone() })
                    .try_init();
            }
            Err(error) => {
                eprintln!(
                    "larust: couldn't open the log file ({error}) - falling back to stdout only"
                );
                let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
            }
        },
        // "stdout", or anything unrecognized - degrades to the always-safe
        // default rather than treating a typo'd `LOG_CHANNEL` as fatal.
        _ => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    }
}

/// Precedence, highest first: `RUST_LOG` (already won over everything
/// before `Config::log_level` existed - still does), `Config::log_level`
/// (empty means "unset" - see that field's own doc comment), then the
/// original hardcoded `app_env`-based default. `sqlx`/`tower_sessions` are
/// pinned to `warn` whenever a plain level word drives the filter (not just
/// in the legacy default branch) - the same per-crate noise the original
/// hardcoded `"debug,sqlx=warn,tower_sessions=warn"` existed to avoid,
/// still just as real when `LOG_LEVEL=debug` is what turned debug logging
/// on instead.
fn build_filter(config: &Config) -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    if !config.log_level.is_empty() {
        return EnvFilter::new(format!(
            "{},sqlx=warn,tower_sessions=warn",
            config.log_level
        ));
    }
    EnvFilter::new(if config.app_env == "local" {
        "debug,sqlx=warn,tower_sessions=warn"
    } else {
        "info"
    })
}

/// Writes every event to both `stdout` and the rotating log file -
/// `Config::log_channel == "stack"`'s own writer. Each half is best-effort
/// independently: a failure writing to one (an already-closed stdout pipe,
/// a file rotation that couldn't complete) doesn't stop the other from
/// still recording the event, and whichever result actually reflects a
/// real problem (the file half - stdout failing is rarely worth surfacing
/// as a logging error in its own right) is what `write` returns.
struct TeeWriter {
    file: RotatingFileWriter,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stdout().write_all(buf);
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stdout().flush();
        self.file.flush()
    }
}

/// Cheap to clone (an `Arc` bump) - `tracing_subscriber::fmt::MakeWriter`
/// calls `make_writer()` fresh for every event (potentially from many
/// concurrent request-handling tasks at once), so the real, single
/// `RotatingFile` lives behind a `Mutex` shared across every clone rather
/// than each clone owning its own file handle.
#[derive(Clone)]
struct RotatingFileWriter(Arc<Mutex<RotatingFile>>);

impl RotatingFileWriter {
    fn open(path: PathBuf, config: &Config) -> io::Result<Self> {
        let file = RotatingFile::open(path, config.log_max_size, config.log_keep_files)?;
        Ok(Self(Arc::new(Mutex::new(file))))
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingFileWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The actual size-tracking, rotate-on-overflow file. Never panics on a
/// rotation failure (a file still held open on Windows by something else
/// momentarily, a permissions hiccup) - `write` falls back to just
/// appending past `max_size` into the current file rather than losing the
/// event entirely or taking the app down over a logging concern.
struct RotatingFile {
    path: PathBuf,
    max_size: u64,
    keep_files: u32,
    file: File,
    current_size: u64,
}

impl RotatingFile {
    fn open(path: PathBuf, max_size: u64, keep_files: u32) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let current_size = file.metadata()?.len();
        Ok(Self {
            path,
            max_size,
            keep_files,
            file,
            current_size,
        })
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.current_size.saturating_add(buf.len() as u64) > self.max_size {
            // Best-effort: a failed rotation just means this write lands in
            // the still-too-large current file instead of a fresh one -
            // the next write gets another chance, rather than this one
            // ever being dropped or erroring out to the caller.
            let _ = rotate(&self.path, self.keep_files).and_then(|()| {
                self.file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&self.path)?;
                self.current_size = 0;
                Ok(())
            });
        }
        let written = self.file.write(buf)?;
        self.current_size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// `{path}.1`, `{path}.2`, ... - built by appending, not `Path::with_extension`
/// (which would only ever replace `larust.log`'s own `.log` extension, not
/// extend it), since `path` already carries a real extension of its own.
fn backup_path(path: &Path, generation: u32) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

/// `keep_files == 0`: no backups at all - the current file is simply
/// discarded and restarted empty. Otherwise, the classic `logrotate` shift:
/// drop whatever's oldest, rename every remaining backup up by one
/// generation (highest first, so no rename ever overwrites a backup this
/// same pass hasn't moved out of the way yet), then rename the current
/// file into the now-vacated `.1` slot. `RotatingFile::write` reopens a
/// fresh file at `path` itself afterward - this function's own job ends
/// once `path` no longer exists.
fn rotate(path: &Path, keep_files: u32) -> io::Result<()> {
    if keep_files == 0 {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }

    let _ = std::fs::remove_file(backup_path(path, keep_files));
    for generation in (1..keep_files).rev() {
        let from = backup_path(path, generation);
        if from.exists() {
            std::fs::rename(&from, backup_path(path, generation + 1))?;
        }
    }
    std::fs::rename(path, backup_path(path, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(app_env: &str, log_level: &str) -> Config {
        let mut config = Config::from_value(&serde_json::json!({})).unwrap();
        config.app_env = app_env.to_string();
        config.log_level = log_level.to_string();
        config
    }

    /// `EnvFilter::to_string()` doesn't preserve the order directives were
    /// constructed in (confirmed directly - a filter built from
    /// `"debug,sqlx=warn,tower_sessions=warn"` round-trips as
    /// `"tower_sessions=warn,sqlx=warn,debug"` instead), so every assertion
    /// below checks each expected directive is present as its own
    /// comma-separated component rather than comparing the whole string.
    fn directives(filter: &EnvFilter) -> Vec<String> {
        filter.to_string().split(',').map(str::to_string).collect()
    }

    #[test]
    fn build_filter_falls_back_to_the_legacy_local_default_with_no_log_level_set() {
        let filter = build_filter(&config_with("local", ""));
        let directives = directives(&filter);
        for expected in ["debug", "sqlx=warn", "tower_sessions=warn"] {
            assert!(
                directives.contains(&expected.to_string()),
                "expected {expected:?} in {directives:?}"
            );
        }
    }

    #[test]
    fn build_filter_falls_back_to_info_outside_local_with_no_log_level_set() {
        let filter = build_filter(&config_with("production", ""));
        assert_eq!(filter.to_string(), "info");
    }

    #[test]
    fn build_filter_uses_log_level_and_still_dampens_the_noisy_crates() {
        let filter = build_filter(&config_with("production", "trace"));
        let directives = directives(&filter);
        for expected in ["trace", "sqlx=warn", "tower_sessions=warn"] {
            assert!(
                directives.contains(&expected.to_string()),
                "expected {expected:?} in {directives:?}"
            );
        }
    }

    #[test]
    fn backup_path_appends_a_generation_suffix_without_disturbing_the_real_extension() {
        let path = PathBuf::from("/tmp/storage/logs/larust.log");
        assert_eq!(
            backup_path(&path, 1),
            PathBuf::from("/tmp/storage/logs/larust.log.1")
        );
        assert_eq!(
            backup_path(&path, 12),
            PathBuf::from("/tmp/storage/logs/larust.log.12")
        );
    }

    #[test]
    fn rotating_file_rotates_once_max_size_is_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("larust.log");
        let mut file = RotatingFile::open(path.clone(), 10, 3).unwrap();

        file.write(b"0123456789").unwrap(); // exactly at the limit, no rotation yet
        assert!(!backup_path(&path, 1).exists());

        file.write(b"more").unwrap(); // now over the limit -> rotates first
        assert!(backup_path(&path, 1).exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"more");
        assert_eq!(std::fs::read(backup_path(&path, 1)).unwrap(), b"0123456789");
    }

    #[test]
    fn rotating_file_shifts_backups_up_a_generation_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("larust.log");
        let mut file = RotatingFile::open(path.clone(), 4, 3).unwrap();

        file.write(b"aaaaa").unwrap(); // over limit immediately -> the empty starting file becomes .1, "aaaaa" lands in the fresh current file
        file.write(b"bbbbb").unwrap(); // over limit again -> "aaaaa" shifts from current into .1, "bbbbb" lands in the fresh current file

        assert_eq!(std::fs::read(backup_path(&path, 1)).unwrap(), b"aaaaa");
        assert_eq!(std::fs::read(&path).unwrap(), b"bbbbb");
    }

    #[test]
    fn rotating_file_deletes_outright_when_keep_files_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("larust.log");
        let mut file = RotatingFile::open(path.clone(), 4, 0).unwrap();

        file.write(b"aaaaa").unwrap();
        file.write(b"bbbbb").unwrap();

        assert!(!backup_path(&path, 1).exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"bbbbb");
    }

    #[test]
    fn rotating_file_never_keeps_more_backups_than_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("larust.log");
        let mut file = RotatingFile::open(path.clone(), 2, 2).unwrap();

        for chunk in [b"aa", b"bb", b"cc", b"dd"] {
            file.write(chunk).unwrap();
        }

        assert!(backup_path(&path, 1).exists());
        assert!(backup_path(&path, 2).exists());
        assert!(!backup_path(&path, 3).exists());
    }
}
