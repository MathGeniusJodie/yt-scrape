use super::subtitle_requests::{SubtitleRateLimiter, run_yt_dlp_subtitle_command};
use crate::urls;
use log::warn;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;

/// Generous upper bound for a single video download; a hung yt-dlp must not
/// leave the UI's downloading spinner (and deferred file deletion) stuck forever.
const VIDEO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Errors that can occur while downloading a video with `yt-dlp`.
#[derive(Debug, Error)]
pub enum DownloadError {
    /// Spawning or awaiting `yt-dlp` failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// `yt-dlp` exited unsuccessfully during the media download phase.
    #[error("yt-dlp failed with exit code {exit_code:?}: {stderr}")]
    Failed {
        exit_code: Option<i32>,
        stderr: String,
    },
    /// `yt-dlp` did not finish within [`VIDEO_DOWNLOAD_TIMEOUT`].
    #[error("yt-dlp timed out after {0:?}")]
    TimedOut(Duration),
}

/// Download a video using yt-dlp
///
/// # Arguments
///
/// * `video_id` - `YouTube` video identifier.
/// * `output_path` - Destination path stem for the downloaded media file.
///
/// # Errors
///
/// Returns an error when `yt-dlp` fails or returns a non-zero exit status for the primary media
/// download phase.
pub async fn download_video(video_id: &str, output_path: &Path) -> Result<(), DownloadError> {
    let url = urls::watch_url(video_id);
    let output_template = output_path.with_extension("%(ext)s");
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Phase 1: Always download video + chapter/info metadata.
    // Keep this independent from subtitle fetching so subtitle rate limits don't fail downloads.
    let mut media_command = super::nice_command("yt-dlp");
    media_command
        .arg("--cookies-from-browser")
        .arg(super::cookies_browser())
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
        // Single-file downloads are not merged, so force the cache container invariant here too.
        .arg("--remux-video")
        .arg("mkv")
        .arg("-o")
        .arg(&output_template)
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(VIDEO_DOWNLOAD_TIMEOUT, media_command.output())
        .await
        .map_err(|_| DownloadError::TimedOut(VIDEO_DOWNLOAD_TIMEOUT))??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DownloadError::Failed {
            exit_code: output.status.code(),
            stderr: stderr.trim().to_string(),
        });
    }

    // Phase 2: Fetch subtitles as best effort. Failure here should not break local playback.
    let subs_output = run_yt_dlp_subtitle_command(SubtitleRateLimiter::global(), video_id, || {
        let mut command = super::nice_command("yt-dlp");
        command
            .arg("--cookies-from-browser")
            .arg(super::cookies_browser())
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
            .stderr(Stdio::piped());
        command
    })
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
