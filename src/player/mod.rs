mod chapters;

use crate::urls;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use thiserror::Error;

use chapters::ensure_chapters_file;

/// mpv exits this quickly only when playback failed to start (dead URL, missing
/// file, network error) — a normal viewing session always outlives this window.
const IMMEDIATE_FAILURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// Errors produced while launching media playback.
#[derive(Debug, Error)]
pub enum PlayerError {
    /// Failed to start the `mpv` process.
    #[error("failed to spawn mpv: {0}")]
    Spawn(#[from] io::Error),
}

/// How an mpv playback session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackEnd {
    /// mpv ran long enough to count as watched, or exited cleanly.
    Watched,
    /// mpv failed almost immediately; the video was never really played.
    FailedImmediately,
}

fn mpv_base_command(title: &str) -> Command {
    let mut command = Command::new("mpv");
    command
        .arg(format!("--title={title}"))
        .arg(format!("--force-media-title={title}"))
        .arg("--sub-auto=fuzzy")
        .arg("--sid=auto")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

fn spawn_mpv_watched(
    command: &mut Command,
) -> Result<async_channel::Receiver<PlaybackEnd>, PlayerError> {
    let mut child = command.spawn()?;
    let (end_tx, end_rx) = async_channel::bounded(1);
    let stderr = child.stderr.take();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                log::debug!("mpv: {line}");
            }
        }
        let exit_ok = child.wait().is_ok_and(|status| status.success());
        // A user quitting mpv (even via signal) after real playback still counts
        // as watched; only an immediate non-zero exit means playback failed.
        let end = if exit_ok || started.elapsed() > IMMEDIATE_FAILURE_WINDOW {
            PlaybackEnd::Watched
        } else {
            PlaybackEnd::FailedImmediately
        };
        let _ = end_tx.send_blocking(end);
    });
    Ok(end_rx)
}

/// Plays a video using `mpv` as a detached process.
///
/// Playback prefers a local file when available. If no local file exists, playback
/// falls back to streaming directly from `YouTube`.
///
/// # Arguments
///
/// * `video_id` - `YouTube` video identifier.
/// * `title` - Title used for mpv window metadata.
/// * `local_path` - Optional path to a local downloaded video.
///
/// # Returns
///
/// A receiver that yields one [`PlaybackEnd`] when the mpv process exits.
///
/// # Errors
///
/// Returns [`PlayerError::Spawn`] if launching `mpv` fails.
pub fn play_video(
    video_id: &str,
    title: &str,
    local_path: Option<&Path>,
) -> Result<async_channel::Receiver<PlaybackEnd>, PlayerError> {
    if let Some(path) = local_path {
        if path.exists() {
            let mut command = mpv_base_command(title);
            let chapters_file = ensure_chapters_file(path);
            if let Some(ref chapters_file) = chapters_file {
                log::info!(
                    "Using chapters file for {}: {}",
                    video_id,
                    chapters_file.display()
                );
                command.arg(format!("--chapters-file={}", chapters_file.display()));
            } else {
                log::debug!("No chapters metadata available for local video {video_id}");
            }
            command.arg(path);
            return spawn_mpv_watched(&mut command);
        }

        log::warn!(
            "Local path does not exist for {}: {}",
            video_id,
            path.display()
        );
    }

    // Fallback: stream from YouTube
    let url = urls::watch_url(video_id);
    let mut command = mpv_base_command(title);
    command.arg(&url);
    spawn_mpv_watched(&mut command)
}

#[cfg(test)]
mod tests {
    use super::mpv_base_command;

    #[test]
    fn mpv_base_command_sets_expected_flags() {
        let command = mpv_base_command("Example Title");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert!(args.contains(&"--title=Example Title".to_string()));
        assert!(args.contains(&"--force-media-title=Example Title".to_string()));
        assert!(args.contains(&"--sub-auto=fuzzy".to_string()));
        assert!(args.contains(&"--sid=auto".to_string()));
    }
}
