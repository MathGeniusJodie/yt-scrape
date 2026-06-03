use super::subtitle_requests::{run_yt_dlp_subtitle_command, SubtitleRateLimiter};
use crate::urls;
use log::warn;
use std::path::Path;
use std::process::Stdio;
use thiserror::Error;

const MIYOO_VIDEO_FILTER: &str = "scale=640:480:force_original_aspect_ratio=decrease,pad=640:480:(ow-iw)/2:(oh-ih)/2:black,fps=20";

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
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

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
/// * `subtitle_path` - Optional subtitle sidecar to burn into the output video.
/// * `output_path` - Destination path for the converted mp4.
///
/// # Errors
///
/// Returns an error when `ffmpeg` fails or returns a non-zero exit status.
pub async fn convert_to_miyoo(
    input_path: &Path,
    subtitle_path: Option<&Path>,
    output_path: &Path,
) -> Result<(), DownloadError> {
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let video_filter = miyoo_video_filter(subtitle_path);
    let output = super::nice_command("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(input_path)
        .arg("-vf")
        .arg(video_filter)
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("ultrafast")
        .arg("-crf")
        .arg("34")
        .arg("-maxrate")
        .arg("600k")
        .arg("-bufsize")
        .arg("1200k")
        .arg("-profile:v")
        .arg("baseline")
        .arg("-level")
        .arg("3.0")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("80k")
        .arg("-ac")
        .arg("2")
        .arg("-movflags")
        .arg("+faststart")
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

fn miyoo_video_filter(subtitle_path: Option<&Path>) -> String {
    match subtitle_path {
        Some(path) => format!(
            "subtitles={},{}",
            escape_filter_path(path),
            MIYOO_VIDEO_FILTER
        ),
        None => MIYOO_VIDEO_FILTER.to_string(),
    }
}

fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(|character| match character {
            '\\' | '\'' | ':' | ',' | '[' | ']' => Some('\\').into_iter().chain(Some(character)),
            _ => None.into_iter().chain(Some(character)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{escape_filter_path, miyoo_video_filter, MIYOO_VIDEO_FILTER};
    use std::path::Path;

    #[test]
    fn miyoo_video_filter_omits_subtitles_without_subtitle_path() {
        assert_eq!(miyoo_video_filter(None), MIYOO_VIDEO_FILTER);
    }

    #[test]
    fn miyoo_video_filter_burns_subtitles_before_scaling() {
        let subtitle_path = Path::new("/tmp/video title.en.vtt");

        assert_eq!(
            miyoo_video_filter(Some(subtitle_path)),
            format!("subtitles=/tmp/video title.en.vtt,{MIYOO_VIDEO_FILTER}")
        );
    }

    #[test]
    fn escape_filter_path_escapes_ffmpeg_filter_special_characters() {
        let path = Path::new("/tmp/vid: one, 'two' [en].vtt");

        assert_eq!(
            escape_filter_path(path),
            "/tmp/vid\\: one\\, \\'two\\' \\[en\\].vtt"
        );
    }
}
