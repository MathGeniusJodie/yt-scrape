use super::subtitle_requests::{SubtitleRateLimiter, run_yt_dlp_subtitle_command};
use crate::urls;
use log::warn;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;

/// Generous upper bound for a single media download; a hung yt-dlp must not
/// leave the UI's downloading spinner (and deferred file deletion) stuck forever.
const MEDIA_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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
    /// `yt-dlp` did not finish within [`MEDIA_DOWNLOAD_TIMEOUT`].
    #[error("yt-dlp timed out after {0:?}")]
    TimedOut(Duration),
}

/// Turns an unsuccessful `yt-dlp` exit into a [`DownloadError::Failed`].
fn check_exit_status(output: &std::process::Output) -> Result<(), DownloadError> {
    if output.status.success() {
        return Ok(());
    }

    Err(DownloadError::Failed {
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
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
    let output = tokio::time::timeout(MEDIA_DOWNLOAD_TIMEOUT, media_command.output())
        .await
        .map_err(|_| DownloadError::TimedOut(MEDIA_DOWNLOAD_TIMEOUT))??;
    check_exit_status(&output)?;

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

/// Directory audio exports are written to: the platform downloads directory,
/// falling back to `~/Downloads` when the user has no XDG entry for it.
///
/// # Returns
///
/// `None` when no home directory can be determined.
pub fn downloads_dir() -> Option<PathBuf> {
    let user_dirs = directories::UserDirs::new()?;
    Some(
        user_dirs
            .download_dir()
            .map_or_else(|| user_dirs.home_dir().join("Downloads"), Path::to_path_buf),
    )
}

/// `yt-dlp` arguments extracting a video's audio as a tagged 256K MP3 named
/// after the video title.
fn audio_export_args(cookies_browser: &str, output_dir: &Path, url: &str) -> Vec<OsString> {
    let mut args = [
        "--cookies-from-browser",
        cookies_browser,
        "-x",
        "--audio-format",
        "mp3",
        "--audio-quality",
        "256K",
        "--embed-thumbnail",
        "--embed-metadata",
        "--parse-metadata",
        "%(title)s:%(meta_title)s",
        "--parse-metadata",
        "%(uploader)s:%(meta_artist)s",
        "-o",
    ]
    .map(OsString::from)
    .to_vec();
    args.push(output_dir.join("%(title)s.%(ext)s").into_os_string());
    args.push(OsString::from(url));
    args
}

/// Download a video's audio track as an MP3 tagged with its title and uploader.
///
/// # Arguments
///
/// * `video_id` - `YouTube` video identifier.
/// * `output_dir` - Directory receiving the `<title>.mp3` file.
///
/// # Errors
///
/// Returns an error when `output_dir` cannot be created, or when `yt-dlp` fails,
/// times out, or exits unsuccessfully.
pub async fn download_audio(video_id: &str, output_dir: &Path) -> Result<(), DownloadError> {
    tokio::fs::create_dir_all(output_dir).await?;

    let mut command = super::nice_command("yt-dlp");
    command
        .args(audio_export_args(
            super::cookies_browser(),
            output_dir,
            &urls::watch_url(video_id),
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(MEDIA_DOWNLOAD_TIMEOUT, command.output())
        .await
        .map_err(|_| DownloadError::TimedOut(MEDIA_DOWNLOAD_TIMEOUT))??;

    check_exit_status(&output)
}

#[cfg(test)]
mod tests {
    use super::audio_export_args;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn audio_export_args_write_a_titled_mp3_into_the_output_directory() {
        let args = audio_export_args(
            "chromium",
            Path::new("/home/user/Downloads"),
            "https://www.youtube.com/watch?v=abc",
        );

        assert_eq!(
            args,
            [
                "--cookies-from-browser",
                "chromium",
                "-x",
                "--audio-format",
                "mp3",
                "--audio-quality",
                "256K",
                "--embed-thumbnail",
                "--embed-metadata",
                "--parse-metadata",
                "%(title)s:%(meta_title)s",
                "--parse-metadata",
                "%(uploader)s:%(meta_artist)s",
                "-o",
                "/home/user/Downloads/%(title)s.%(ext)s",
                "https://www.youtube.com/watch?v=abc",
            ]
            .map(OsString::from)
        );
    }
}
