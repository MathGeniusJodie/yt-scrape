//! Frogpoints: a personal time-economy that charges points for feed refreshes and
//! leisure mpv watching. Lives in this executable by design, but all periodic work
//! (directory scans, process checks, file I/O) runs on a background thread so the
//! GTK main loop is never blocked.

use log::{info, warn};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
use thiserror::Error;

pub const REFRESH_COST: i64 = 10;
const LEISURE_COST: i64 = 1;
const LEISURE_IDLE_SECONDS: u64 = 120;
const LEISURE_INTERVAL_SECONDS: u32 = 60;
const FROGPOINTS_RELATIVE_PATH: &[&str] = &["Desktop", "RemoteVault", "frogpoints.md"];
const SVG_TEMPLATE_RELATIVE_PATH: &[&str] = &["Desktop", "allfiles", "templates"];
const INKSCAPE_CACHE_RELATIVE_PATH: &[&str] = &[".cache", "inkscape"];

static LEISURE_MONITOR_STARTED: AtomicBool = AtomicBool::new(false);
/// Guards against overlapping leisure checks if one tick's scan outlives the interval.
static LEISURE_CHECK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Errors produced by frogpoints accounting.
#[derive(Debug, Error)]
pub enum FrogpointsError {
    /// `HOME` environment variable is unset.
    #[error("HOME is not set")]
    MissingHome,
    /// Reading or locking the frogpoints file failed.
    #[error("Failed to read frogpoints: {0}")]
    Read(#[source] std::io::Error),
    /// The frogpoints file did not contain a whole number.
    #[error("frogpoints.md must contain a whole number")]
    InvalidNumber(#[source] std::num::ParseIntError),
    /// Balance is too small for the requested debit.
    #[error("Need {cost} frogpoints to refresh, but only {available} remain")]
    Insufficient {
        /// Current balance.
        available: i64,
        /// Points required.
        cost: i64,
    },
    /// Writing the updated balance failed.
    #[error("Failed to save frogpoints: {0}")]
    Write(#[source] std::io::Error),
}

fn frogpoints_path() -> Result<PathBuf, FrogpointsError> {
    home_relative_path(FROGPOINTS_RELATIVE_PATH)
}

fn home_relative_path(relative: &[&str]) -> Result<PathBuf, FrogpointsError> {
    let mut path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(FrogpointsError::MissingHome)?;
    path.extend(relative);
    Ok(path)
}

/// Directories scanned for recent SVG edits that mark active (non-leisure) work.
fn svg_watch_dirs() -> Result<[PathBuf; 2], FrogpointsError> {
    Ok([
        home_relative_path(SVG_TEMPLATE_RELATIVE_PATH)?,
        home_relative_path(INKSCAPE_CACHE_RELATIVE_PATH)?,
    ])
}

/// Applies `compute` to the current balance under an exclusive lock and persists the
/// result crash-safely, so concurrent processes (e.g. a file-sync tool or a second app
/// instance) cannot interleave read-modify-write cycles, and a crash mid-write never
/// leaves `frogpoints.md` truncated or corrupt.
///
/// The lock is taken on a dedicated sibling `.lock` file rather than `path` itself,
/// because the update is performed via write-to-temp-then-rename: if the lock were on
/// `path`, a rename would silently release it out from under a concurrent waiter. The
/// new balance is written to a sibling temp file, `sync_all`'d, then atomically renamed
/// over `path`, so a crash at any point leaves either the old or the new balance intact.
fn update_frogpoints_locked(
    path: &Path,
    compute: impl FnOnce(i64) -> Result<i64, FrogpointsError>,
) -> Result<i64, FrogpointsError> {
    let lock_path = path.with_extension("md.lock");
    // The lock file's contents are irrelevant; only its inode matters.
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(FrogpointsError::Read)?;
    lock_file.lock().map_err(FrogpointsError::Read)?;

    let contents = fs::read_to_string(path).map_err(FrogpointsError::Read)?;
    let current = contents
        .trim()
        .parse::<i64>()
        .map_err(FrogpointsError::InvalidNumber)?;

    let updated = compute(current)?;

    let temp_path = path.with_extension("md.tmp");
    let mut temp_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp_path)
        .map_err(FrogpointsError::Write)?;
    temp_file
        .write_all(format!("{updated}\n").as_bytes())
        .map_err(FrogpointsError::Write)?;
    temp_file.sync_all().map_err(FrogpointsError::Write)?;
    drop(temp_file);
    fs::rename(&temp_path, path).map_err(FrogpointsError::Write)?;

    Ok(updated)
}

fn debit_frogpoints(path: &Path, cost: i64) -> Result<i64, FrogpointsError> {
    update_frogpoints_locked(path, |current| {
        if current < cost {
            Err(FrogpointsError::Insufficient {
                available: current,
                cost,
            })
        } else {
            Ok(current - cost)
        }
    })
}

/// Applies a signed delta to the balance: positive credits, negative charges.
/// Unlike [`debit_frogpoints`], the balance may go negative.
fn adjust_frogpoints(path: &Path, delta: i64) -> Result<i64, FrogpointsError> {
    update_frogpoints_locked(path, |current| Ok(current + delta))
}

/// Debits the feed-refresh cost, failing without charging when the balance is too low.
///
/// # Errors
///
/// Returns [`FrogpointsError`] when the file is missing/invalid or the balance is
/// insufficient.
pub fn debit_refresh_frogpoints() -> Result<i64, FrogpointsError> {
    let path = frogpoints_path()?;
    debit_frogpoints(&path, REFRESH_COST)
}

/// Credits the feed-refresh cost back after a refresh that failed mid-flight,
/// so users are never charged for a refresh that produced nothing.
///
/// # Errors
///
/// Returns [`FrogpointsError`] when the file is missing/invalid or cannot be written.
pub fn refund_refresh_frogpoints() -> Result<i64, FrogpointsError> {
    let path = frogpoints_path()?;
    adjust_frogpoints(&path, REFRESH_COST)
}

fn has_recent_svg_modification(
    template_dir: &Path,
    idle_duration: Duration,
) -> Result<bool, std::io::Error> {
    let cutoff = SystemTime::now()
        .checked_sub(idle_duration)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut pending_dirs = vec![template_dir.to_path_buf()];

    while let Some(dir) = pending_dirs.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending_dirs.push(entry.path());
                continue;
            }

            let is_svg = entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"));
            if is_svg && entry.metadata()?.modified()? > cutoff {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// `pgrep` works on both X11 and Wayland, unlike window-based detection.
fn mpv_is_running() -> bool {
    match Command::new("pgrep")
        .args(["-x", "mpv"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(error) => {
            warn!("Failed to query mpv processes with pgrep: {error}");
            false
        }
    }
}

fn charge_leisure_frogpoint_if_needed() {
    // Cheap process check first; skip the directory scans entirely when idle.
    if !mpv_is_running() {
        return;
    }

    let watch_dirs = match svg_watch_dirs() {
        Ok(dirs) => dirs,
        Err(error) => {
            warn!("Failed to locate SVG watch directories: {error}");
            return;
        }
    };
    let idle_duration = Duration::from_secs(LEISURE_IDLE_SECONDS);
    let recent_svg_modification = watch_dirs.iter().any(|dir| {
        match has_recent_svg_modification(dir, idle_duration) {
            Ok(recent) => recent,
            // A missing directory simply means no recent edits there.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                warn!(
                    "Failed to inspect SVG directory {}: {}",
                    dir.display(),
                    error
                );
                false
            }
        }
    });

    if recent_svg_modification {
        info!("Recent SVG modification detected; no leisure mpv minute charged");
        return;
    }

    let path = match frogpoints_path() {
        Ok(path) => path,
        Err(error) => {
            warn!("Failed to locate frogpoints file: {error}");
            return;
        }
    };

    match adjust_frogpoints(&path, -LEISURE_COST) {
        Ok(remaining) => info!("Leisure mpv minute charged; {remaining} frogpoints remaining"),
        Err(error) => warn!("Failed to charge leisure frogpoint: {error}"),
    }
}

/// Starts the once-per-minute leisure monitor. Each tick runs on a detached
/// background thread; the GLib timeout only dispatches it.
pub fn start_leisure_monitor() {
    if LEISURE_MONITOR_STARTED
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    gtk::glib::timeout_add_seconds_local(LEISURE_INTERVAL_SECONDS, || {
        if LEISURE_CHECK_IN_FLIGHT
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            std::thread::spawn(|| {
                charge_leisure_frogpoint_if_needed();
                LEISURE_CHECK_IN_FLIGHT.store(false, Ordering::Release);
            });
        }
        gtk::glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::{
        FrogpointsError, REFRESH_COST, adjust_frogpoints, debit_frogpoints,
        has_recent_svg_modification,
    };
    use tempfile::NamedTempFile;

    fn temporary_frogpoints_file() -> NamedTempFile {
        NamedTempFile::new().expect("temporary frogpoints file must be creatable")
    }

    #[test]
    fn debit_frogpoints_subtracts_cost_and_returns_remaining_balance() {
        let file = temporary_frogpoints_file();
        let path = file.path();
        std::fs::write(path, "18").expect("write test frogpoints file");

        let remaining = debit_frogpoints(path, REFRESH_COST).expect("debit enough frogpoints");

        assert_eq!(remaining, 8);
        assert_eq!(
            std::fs::read_to_string(path).expect("read updated frogpoints file"),
            "8\n"
        );
    }

    #[test]
    fn debit_frogpoints_blocks_when_balance_is_too_small() {
        let file = temporary_frogpoints_file();
        let path = file.path();
        std::fs::write(path, "9").expect("write test frogpoints file");

        let error = debit_frogpoints(path, REFRESH_COST).expect_err("block insufficient balance");

        assert!(matches!(
            error,
            FrogpointsError::Insufficient {
                available: 9,
                cost: REFRESH_COST
            }
        ));
        assert_eq!(
            std::fs::read_to_string(path).expect("read unchanged frogpoints file"),
            "9"
        );
    }

    #[test]
    fn adjust_frogpoints_with_positive_delta_credits_the_balance() {
        let file = temporary_frogpoints_file();
        let path = file.path();
        std::fs::write(path, "3").expect("write test frogpoints file");

        let remaining = adjust_frogpoints(path, REFRESH_COST).expect("credit frogpoints");

        assert_eq!(remaining, 13);
        assert_eq!(
            std::fs::read_to_string(path).expect("read updated frogpoints file"),
            "13\n"
        );
    }

    #[test]
    fn adjust_frogpoints_allows_negative_balances() {
        let file = temporary_frogpoints_file();
        let path = file.path();
        std::fs::write(path, "0").expect("write test frogpoints file");

        let remaining = adjust_frogpoints(path, -1).expect("adjust frogpoints");

        assert_eq!(remaining, -1);
        assert_eq!(
            std::fs::read_to_string(path).expect("read updated frogpoints file"),
            "-1\n"
        );
    }

    #[test]
    fn adjust_frogpoints_writes_shorter_balance_without_stale_bytes() {
        let file = temporary_frogpoints_file();
        let path = file.path();
        std::fs::write(path, "1000").expect("write test frogpoints file");

        let remaining = adjust_frogpoints(path, -995).expect("adjust frogpoints");

        assert_eq!(remaining, 5);
        assert_eq!(
            std::fs::read_to_string(path).expect("read updated frogpoints file"),
            "5\n"
        );
    }

    #[test]
    fn repeated_updates_with_lock_sibling_do_not_corrupt_the_balance() {
        let file = temporary_frogpoints_file();
        let path = file.path();
        std::fs::write(path, "100").expect("write test frogpoints file");

        for _ in 0..5 {
            adjust_frogpoints(path, -1).expect("adjust frogpoints");
        }

        assert_eq!(
            std::fs::read_to_string(path).expect("read updated frogpoints file"),
            "95\n"
        );

        let lock_path = path.with_extension("md.lock");
        let _ = std::fs::remove_file(lock_path);
        let temp_path = path.with_extension("md.tmp");
        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn has_recent_svg_modification_ignores_non_svg_files() {
        let dir = tempfile::tempdir().expect("test directory must be creatable");
        std::fs::write(dir.path().join("not-svg.txt"), "fresh").expect("write non-svg file");

        let has_recent_svg =
            has_recent_svg_modification(dir.path(), std::time::Duration::from_secs(120))
                .expect("scan temp directory");

        assert!(!has_recent_svg);
    }

    #[test]
    fn has_recent_svg_modification_finds_nested_svg_files_case_insensitively() {
        let dir = tempfile::tempdir().expect("test directory must be creatable");
        let nested_dir = dir.path().join("nested");
        std::fs::create_dir_all(&nested_dir).expect("create nested test directory");
        std::fs::write(nested_dir.join("work.SVG"), "<svg />").expect("write svg file");

        let has_recent_svg =
            has_recent_svg_modification(dir.path(), std::time::Duration::from_secs(120))
                .expect("scan temp directory");

        assert!(has_recent_svg);
    }
}
