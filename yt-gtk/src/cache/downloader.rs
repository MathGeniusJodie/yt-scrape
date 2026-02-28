use crate::urls;
use log::warn;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Download a video using yt-dlp
///
/// # Arguments
///
/// * `video_id` - YouTube video identifier.
/// * `output_path` - Destination path stem for the downloaded media file.
///
/// # Errors
///
/// Returns an error when `yt-dlp` fails or returns a non-zero exit status for the
/// primary media download phase.
pub async fn download_video(video_id: &str, output_path: &Path) -> anyhow::Result<()> {
    let url = urls::watch_url(video_id);
    let output_template = output_path.with_extension("%(ext)s");

    // Phase 1: Always download video + chapter/info metadata.
    // Keep this independent from subtitle fetching so subtitle rate limits don't fail downloads.
    let output = Command::new("yt-dlp")
        .arg("-f")
        .arg("bestvideo[height<=720]+bestaudio/best[height<=720]")
        .arg("--add-metadata")
        .arg("--embed-chapters")
        .arg("--write-info-json")
        .arg("--embed-info-json")
        .arg("--write-thumbnail")
        .arg("--embed-thumbnail")
        .arg("--merge-output-format")
        .arg("mkv")
        .arg("-o")
        .arg(&output_template)
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "yt-dlp failed with exit code {:?}: {}",
            output.status.code(),
            stderr.trim()
        );
    }

    // Phase 2: Fetch subtitles as best effort. Failure here should not break local playback.
    let subs_output = Command::new("yt-dlp")
        .arg("--write-subs")
        .arg("--write-auto-subs")
        .arg("--sub-langs")
        .arg("en.*,en,-live_chat")
        .arg("--convert-subs")
        .arg("vtt")
        .arg("--skip-download")
        .arg("-o")
        .arg(&output_template)
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !subs_output.status.success() {
        let stderr = String::from_utf8_lossy(&subs_output.stderr);
        warn!(
            "Subtitle download failed for {} (continuing): {}",
            video_id,
            stderr.trim()
        );
    }

    Ok(())
}
