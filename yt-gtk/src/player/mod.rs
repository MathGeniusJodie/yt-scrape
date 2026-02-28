mod chapters;

use crate::urls;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use thiserror::Error;

use chapters::ensure_chapters_file;

/// Errors produced while launching media playback.
#[derive(Debug, Error)]
pub enum PlayerError {
    /// Failed to start the `mpv` process.
    #[error("failed to spawn mpv: {0}")]
    Spawn(#[from] io::Error),
}

fn mpv_base_command(title: &str) -> Command {
    let mut command = Command::new("mpv");
    command
        .arg(format!("--title={title}"))
        .arg(format!("--force-media-title={title}"))
        .arg("--sub-auto=all")
        .arg("--sid=auto")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Plays a video using `mpv` as a detached process.
///
/// Playback prefers a local file when available. If no local file exists, playback
/// falls back to streaming directly from YouTube.
///
/// # Arguments
///
/// * `video_id` - YouTube video identifier.
/// * `title` - Title used for mpv window metadata.
/// * `local_path` - Optional path to a local downloaded video.
///
/// # Errors
///
/// Returns [`PlayerError::Spawn`] if launching `mpv` fails.
pub fn play_video(
    video_id: &str,
    title: &str,
    local_path: Option<&Path>,
) -> Result<(), PlayerError> {
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
                log::warn!(
                    "No chapters metadata available for local video {}",
                    video_id
                );
            }
            command.arg(path).spawn()?;
            return Ok(());
        }

        log::warn!(
            "Local path does not exist for {}: {}",
            video_id,
            path.display()
        );
    }

    // Fallback: stream from YouTube
    let url = urls::watch_url(video_id);
    mpv_base_command(title).arg(&url).spawn()?;

    Ok(())
}
