use super::subtitle_requests::{run_yt_dlp_subtitle_command, SubtitleRateLimiter};
use crate::urls;
use log::warn;
use std::path::Path;
use std::process::Stdio;
use thiserror::Error;

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
}

/// Download a video using yt-dlp
///
/// # Arguments
///
/// * `video_id` - YouTube video identifier.
/// * `output_path` - Destination path stem for the downloaded media file.
///
/// # Errors
///
/// Returns an error when `yt-dlp` fails or returns a non-zero exit status for the primary media
/// download phase.
pub async fn download_video(video_id: &str, output_path: &Path) -> Result<(), DownloadError> {
    let url = urls::watch_url(video_id);
    let output_template = output_path.with_extension("%(ext)s");

    // Phase 1: Always download video + chapter/info metadata.
    // Keep this independent from subtitle fetching so subtitle rate limits don't fail downloads.
    let output = super::nice_command("yt-dlp")
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
        .stderr(Stdio::piped())
        .output()
        .await?;

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

/// Convert a downloaded video to the miyoo-compatible format using ffmpeg.
///
/// # Arguments
///
/// * `input_path` - Path to the source video file.
/// * `output_path` - Destination path for the converted mp4.
///
/// # Errors
///
/// Returns an error when `ffmpeg` fails or returns a non-zero exit status.
pub async fn convert_to_miyoo(input_path: &Path, output_path: &Path) -> Result<(), DownloadError> {
    let output = super::nice_command("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(input_path)
        .arg("-vf")
        .arg("scale=750:560:force_original_aspect_ratio=decrease,pad=750:560:(ow-iw)/2:(oh-ih)/2:black,fps=24")
        .arg("-c:v").arg("libx264")
        .arg("-preset").arg("veryfast")
        .arg("-crf").arg("30")
        .arg("-maxrate").arg("800k")
        .arg("-bufsize").arg("1600k")
        .arg("-profile:v").arg("main")
        .arg("-level").arg("3.1")
        .arg("-pix_fmt").arg("yuv420p")
        .arg("-c:a").arg("aac")
        .arg("-b:a").arg("96k")
        .arg("-ac").arg("2")
        .arg("-movflags").arg("+faststart")
        .arg(output_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DownloadError::Failed {
            exit_code: output.status.code(),
            stderr: stderr.trim().to_string(),
        });
    }

    Ok(())
}
