use crate::urls;
use std::path::Path;
use std::process::{Command, Stdio};

/// Play a video with mpv (detached process)
/// If a local file exists, play that; otherwise stream from YouTube
pub fn play_video(video_id: &str, title: &str, local_path: Option<&Path>) -> anyhow::Result<()> {
    // Check if we have a local copy
    if let Some(path) = local_path {
        if path.exists() {
            // Play local file
            Command::new("mpv")
                .arg(format!("--title={}", title))
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            return Ok(());
        }
    }

    // Fallback: stream from YouTube
    let url = urls::watch_url(video_id);
    Command::new("mpv")
        .arg(format!("--title={}", title))
        .arg("--ytdl-format=bestvideo[height<=720]+bestaudio/best[height<=720]/best")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(())
}
