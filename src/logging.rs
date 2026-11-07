//! Verbose file logging: every run appends to `devclean.log` next to the
//! executable, falling back to the working directory when that folder is
//! read-only (e.g. Program Files). Nothing extra prints to the console unless
//! both candidates fail — the UI already carries errors on stderr.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static INITIALISED: OnceLock<()> = OnceLock::new();

/// Rotate once the log passes this size; one previous file is kept.
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Opens/appends the shared log file and installs a logger at the highest
/// verbosity. Repeat calls are no-ops; an open failure never stops the run.
pub fn init() {
    if INITIALISED.set(()).is_err() {
        return; // already initialised earlier in this process
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = crate::paths::state_file("devclean.log") {
        candidates.push(path);
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("devclean.log"));
    }
    let Some((path, file)) = open_first(&candidates) else {
        eprintln!(
            "devclean: note: logging disabled (no writable devclean.log next to the exe or in the working directory)"
        );
        return;
    };
    install(file, &path);
}

/// First candidate that opens for appending; earlier ones are read-only or
/// missing. The chosen file is created when absent.
fn open_first(candidates: &[PathBuf]) -> Option<(PathBuf, File)> {
    candidates
        .iter()
        .find_map(|path| open_append(path).ok().map(|file| (path.clone(), file)))
}

fn open_append(path: &Path) -> std::io::Result<File> {
    rotate_if_larger_than(path, MAX_LOG_BYTES);
    OpenOptions::new().create(true).append(true).open(path)
}

/// Trace logging on a huge tree grows fast, so the previous log is kept as
/// `devclean.log.1` once the current one passes the cap. Best-effort: a locked
/// log just keeps appending instead of failing the run.
fn rotate_if_larger_than(path: &Path, max_bytes: u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= max_bytes {
        return;
    }
    let _ = std::fs::rename(path, path.with_extension("log.1"));
}

fn install(file: File, path: &Path) {
    let mut builder = simplelog::ConfigBuilder::new();
    builder
        .set_time_level(log::LevelFilter::Trace)
        .set_location_level(log::LevelFilter::Trace)
        .set_thread_level(log::LevelFilter::Trace);
    if let Err(err) = simplelog::WriteLogger::init(log::LevelFilter::Trace, builder.build(), file) {
        eprintln!(
            "devclean: note: logging disabled ({}: {err})",
            path.display()
        );
    }
}

#[cfg(test)]
fn init_in(dir: &Path) {
    if INITIALISED.set(()).is_err() {
        return;
    }
    let path = dir.join("devclean.log");
    let Ok(file) = open_append(&path) else {
        return;
    };
    install(file, &path);
}

#[cfg(test)]
mod tests {
    use super::init_in;

    #[test]
    fn creates_log_file() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        init_in(dir.path());
        log::trace!("smoke message for the log file");
        assert!(dir.path().join("devclean.log").exists());
    }

    #[test]
    fn falls_back_when_the_first_candidate_is_unwritable() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let blocked = dir.path().join("no-such-dir").join("devclean.log");
        let good = dir.path().join("devclean.log");
        let (path, _file) =
            super::open_first(&[blocked, good.clone()]).expect("fallback candidate");
        assert_eq!(path, good);
        assert!(good.exists());
    }

    #[test]
    fn oversized_log_rotates_to_dot_one() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("devclean.log");
        std::fs::write(&path, b"0123456789").expect("write log");
        super::rotate_if_larger_than(&path, 5);
        assert!(!path.exists(), "the oversized log moved away");
        let rotated = dir.path().join("devclean.log.1");
        assert_eq!(
            std::fs::read_to_string(rotated).expect("rotated log"),
            "0123456789"
        );

        std::fs::write(&path, b"0123").expect("write small log");
        super::rotate_if_larger_than(&path, 5);
        assert!(path.exists(), "a small log stays put");
    }
}
