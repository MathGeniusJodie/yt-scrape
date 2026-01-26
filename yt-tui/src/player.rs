use crate::urls;
use std::process::{Command, Stdio};

/// Play a video with mpv (detached process)
pub fn play_video(video_id: &str) -> anyhow::Result<()> {
    let url = urls::watch_url(video_id);

    // Spawn mpv as detached process so it survives after we exit
    Command::new("mpv")
        .arg(&url)
        .arg("--ytdl-format=best[height<=720]")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(())
}
