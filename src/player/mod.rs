mod chapters;

use crate::urls;
use std::io::{self, BufRead, BufReader};
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
        .arg("--sub-auto=fuzzy")
        .arg("--sid=auto")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

fn spawn_mpv_with_stderr_logging(command: &mut Command) -> Result<(), PlayerError> {
    let mut child = command.spawn()?;
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                log::debug!("mpv: {}", line);
            }
        });
    }
    Ok(())
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
                log::debug!(
                    "No chapters metadata available for local video {}",
                    video_id
                );
            }
            command.arg(path);
            spawn_mpv_with_stderr_logging(&mut command)?;
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
    let mut command = mpv_base_command(title);
    command.arg(&url);
    spawn_mpv_with_stderr_logging(&mut command)?;

    Ok(())
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
