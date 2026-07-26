mod chapters;

use crate::urls;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

use chapters::ensure_chapters_file;

/// mpv exits this quickly only when playback failed to start (dead URL, missing
/// file, network error) — a normal viewing session always outlives this window.
/// Fallback only, used when the IPC socket never connected.
const IMMEDIATE_FAILURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
/// Observed playback position (via IPC) required to count a session as watched.
const WATCHED_MIN_PLAYBACK_SECONDS: f64 = 10.0;
/// How long to keep trying to connect to mpv's IPC socket after spawn.
const IPC_CONNECT_WINDOW: std::time::Duration = std::time::Duration::from_secs(15);
const IPC_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Distinguishes concurrent mpv sessions' IPC socket paths.
static NEXT_PLAYER_ID: AtomicU64 = AtomicU64::new(0);

/// yt-dlp format selector capping streamed video at 720p. The muxed-stream
/// fallback is capped too, so no branch can silently pull a larger rendition.
const YTDL_FORMAT_MAX_720P: &str = "bv*[height<=720]+ba/b[height<=720]";

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
    /// mpv ran long enough to count as watched.
    Watched,
    /// mpv exited almost immediately; the video was never really played.
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

/// Returns `true` if `line` is an mpv stderr line reporting that playback never
/// started (dead URL, missing file, unrecognized format, ...).
fn is_fatal_open_failure(line: &str) -> bool {
    let lower = line.to_lowercase();
    [
        "failed to open",
        "errors when loading file",
        "failed to recognize file format",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Parses a `get_property playback-time` IPC response line into seconds.
fn playback_time_from_ipc_line(line: &str) -> Option<f64> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    (value.get("error")?.as_str()? == "success")
        .then(|| value.get("data")?.as_f64())
        .flatten()
}

/// Polls mpv's IPC socket for `playback-time` until the socket closes,
/// returning the highest position observed. `None` when the socket never
/// connected (mpv died instantly or IPC is unavailable).
fn observe_max_playback_time(socket_path: &Path) -> Option<f64> {
    use std::os::unix::net::UnixStream;

    let connect_deadline = std::time::Instant::now() + IPC_CONNECT_WINDOW;
    let stream = loop {
        match UnixStream::connect(socket_path) {
            Ok(stream) => break stream,
            Err(_) if std::time::Instant::now() < connect_deadline => {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(_) => return None,
        }
    };

    let mut writer = stream.try_clone().ok()?;
    let mut reader = BufReader::new(stream);
    let mut max_playback_time = 0.0f64;
    loop {
        if writer
            .write_all(b"{\"command\":[\"get_property\",\"playback-time\"]}\n")
            .is_err()
        {
            break; // mpv exited; the socket is gone.
        }
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                // EOF or socket error: mpv exited.
                Ok(0) | Err(_) => return Some(max_playback_time),
                Ok(_) => {
                    if let Some(playback_time) = playback_time_from_ipc_line(&line) {
                        max_playback_time = max_playback_time.max(playback_time);
                        break;
                    }
                    // Unrelated event line (pause, seek, ...): keep reading.
                }
            }
        }
        std::thread::sleep(IPC_POLL_INTERVAL);
    }
    Some(max_playback_time)
}

fn spawn_mpv_watched(
    command: &mut Command,
) -> Result<async_channel::Receiver<PlaybackEnd>, PlayerError> {
    let socket_path = std::env::temp_dir().join(format!(
        "yt-gtk-mpv-{}-{}.sock",
        std::process::id(),
        NEXT_PLAYER_ID.fetch_add(1, Ordering::Relaxed)
    ));
    command.arg(format!("--input-ipc-server={}", socket_path.display()));

    let mut child = command.spawn()?;
    let (end_tx, end_rx) = async_channel::bounded(1);
    let stderr = child.stderr.take();

    // Fallback signals for when the IPC socket never comes up.
    let fatal_line_seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fatal_line_seen_logger = fatal_line_seen.clone();
    std::thread::spawn(move || {
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if is_fatal_open_failure(&line) {
                    fatal_line_seen_logger.store(true, Ordering::Relaxed);
                }
                log::debug!("mpv: {line}");
            }
        }
    });

    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let observed = observe_max_playback_time(&socket_path);
        let _ = child.wait();
        let _ = std::fs::remove_file(&socket_path);

        // Prefer the position mpv itself reported; fall back to the old
        // exit-timing heuristic only when IPC was unavailable.
        let end = match observed {
            Some(max_playback_time) if max_playback_time >= WATCHED_MIN_PLAYBACK_SECONDS => {
                PlaybackEnd::Watched
            }
            Some(_) => PlaybackEnd::FailedImmediately,
            None => {
                if fatal_line_seen.load(Ordering::Relaxed)
                    || started.elapsed() <= IMMEDIATE_FAILURE_WINDOW
                {
                    PlaybackEnd::FailedImmediately
                } else {
                    PlaybackEnd::Watched
                }
            }
        };
        let _ = end_tx.send_blocking(end);
    });
    Ok(end_rx)
}

/// Builds the mpv command for streaming `url`, resolution-capped at 720p.
fn stream_command(url: &str, title: &str) -> Command {
    let mut command = mpv_base_command(title);
    command
        .arg(format!("--ytdl-format={YTDL_FORMAT_MAX_720P}"))
        .arg(url);
    command
}

/// Plays a video using `mpv` as a detached process.
///
/// Playback prefers a local file when available. If no local file exists, playback
/// falls back to streaming directly from `YouTube` at no more than 720p.
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
    let mut command = stream_command(&urls::watch_url(video_id), title);
    spawn_mpv_watched(&mut command)
}

#[cfg(test)]
mod tests {
    use super::{is_fatal_open_failure, mpv_base_command, stream_command};

    #[test]
    fn is_fatal_open_failure_detects_known_failure_lines() {
        assert!(is_fatal_open_failure("Failed to open ."));
        assert!(is_fatal_open_failure(
            "[ffmpeg/demuxer] Errors when loading file"
        ));
        assert!(is_fatal_open_failure("Failed to recognize file format."));
        // Case-insensitive substring match.
        assert!(is_fatal_open_failure("FAILED TO OPEN /some/path"));
    }

    #[test]
    fn is_fatal_open_failure_ignores_ordinary_log_lines() {
        assert!(!is_fatal_open_failure("AO: [pipewire] 48000Hz stereo"));
        assert!(!is_fatal_open_failure("Video--vo=gpu (opengl)"));
        assert!(!is_fatal_open_failure("Cache fill: 12.34% (1234 bytes)"));
    }

    #[test]
    fn playback_time_parses_successful_property_response() {
        assert_eq!(
            super::playback_time_from_ipc_line(r#"{"data":42.5,"error":"success"}"#),
            Some(42.5)
        );
    }

    #[test]
    fn playback_time_ignores_events_and_errors() {
        assert_eq!(
            super::playback_time_from_ipc_line(r#"{"event":"pause"}"#),
            None
        );
        assert_eq!(
            super::playback_time_from_ipc_line(r#"{"error":"property unavailable"}"#),
            None
        );
        assert_eq!(super::playback_time_from_ipc_line("not json"), None);
    }

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

    #[test]
    fn stream_command_caps_resolution_at_720p() {
        let command = stream_command("https://www.youtube.com/watch?v=abc", "Example Title");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert!(args.contains(&"--ytdl-format=bv*[height<=720]+ba/b[height<=720]".to_string()));
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://www.youtube.com/watch?v=abc")
        );
    }
}
