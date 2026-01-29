use crate::urls;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// Download a video using yt-dlp
pub async fn download_video(video_id: &str, output_path: &PathBuf) -> anyhow::Result<()> {
    let url = urls::watch_url(video_id);

    // Use yt-dlp to download the video
    // Format: best quality up to 720p, merge to mp4
    let status = Command::new("yt-dlp")
        .arg("--extractor-args=youtube:player_client=default,ios,-android_sdkless")
        .arg("-f")
        .arg("bestvideo[height<=720]+bestaudio/best[height<=720]")
        .arg("--merge-output-format")
        .arg("mp4")
        .arg("-o")
        .arg(output_path)
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("yt-dlp failed with exit code: {:?}", status.code())
    }
}
