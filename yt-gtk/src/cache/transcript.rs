use super::subtitle_requests::{run_yt_dlp_subtitle_command, SubtitleRateLimiter};
use crate::urls;
use serde::Deserialize;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Deserialize)]
struct Json3Subtitle {
    events: Option<Vec<Json3Event>>,
}

#[derive(Debug, Deserialize)]
struct Json3Event {
    segs: Option<Vec<Json3Segment>>,
}

#[derive(Debug, Deserialize)]
struct Json3Segment {
    utf8: Option<String>,
}

/// Errors that can occur while fetching or parsing a transcript.
#[derive(Debug, Error)]
pub enum TranscriptError {
    /// A filesystem operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON parsing failed.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    /// `yt-dlp` did not successfully download subtitles.
    #[error("yt-dlp failed to fetch subtitles")]
    SubtitleFetchFailed,
    /// No subtitle artifact could be located after download.
    #[error("no subtitle file found for video {video_id}")]
    SubtitleFileNotFound { video_id: String },
    /// Subtitle payload did not contain any events.
    #[error("subtitle payload does not contain events")]
    MissingSubtitleEvents,
}

/// Fetch transcript for a video using yt-dlp
///
/// # Arguments
///
/// * `video_id` - YouTube video identifier.
/// * `work_dir` - Temporary directory where subtitle artifacts are written.
///
/// # Returns
///
/// The normalized transcript text.
///
/// # Errors
///
/// Returns [`TranscriptError`] when subtitle download, discovery, or parsing fails.
pub async fn fetch_transcript(video_id: &str, work_dir: &Path) -> Result<String, TranscriptError> {
    let url = urls::watch_url(video_id);
    let output_template = work_dir.join(format!("{}.%(ext)s", video_id));

    // Run yt-dlp to download auto-generated subtitles in json3 format.
    let output = run_yt_dlp_subtitle_command(SubtitleRateLimiter::global(), video_id, || {
        let mut command = Command::new("yt-dlp");
        command
            .arg("--write-auto-sub")
            .arg("--sub-format")
            .arg("json3")
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

    if !output.status.success() {
        return Err(TranscriptError::SubtitleFetchFailed);
    }

    // Find the downloaded subtitle file (could be .en.json3, .en-US.json3, etc.).
    let subtitle_path = find_subtitle_path(work_dir, video_id)?;

    // Read and parse the JSON3 file
    let json_content = tokio::fs::read_to_string(&subtitle_path).await?;
    let transcript = parse_json3(&json_content)?;

    // Clean up the subtitle file
    let _ = tokio::fs::remove_file(&subtitle_path).await;

    Ok(transcript)
}

fn find_subtitle_path(
    work_dir: &Path,
    video_id: &str,
) -> Result<std::path::PathBuf, TranscriptError> {
    let expected_prefix = format!("{video_id}.");

    std::fs::read_dir(work_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension().and_then(OsStr::to_str) == Some("json3")
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .map(|name| name.starts_with(&expected_prefix))
                    .unwrap_or(false)
        })
        .ok_or_else(|| TranscriptError::SubtitleFileNotFound {
            video_id: video_id.to_string(),
        })
}

/// Parse JSON3 subtitle format into clean text
fn parse_json3(json: &str) -> Result<String, TranscriptError> {
    let subtitle: Json3Subtitle = serde_json::from_str(json)?;

    let events = subtitle
        .events
        .ok_or(TranscriptError::MissingSubtitleEvents)?;

    let raw_text: String = events
        .into_iter()
        .flat_map(|event| event.segs.unwrap_or_default())
        .filter_map(|segment| segment.utf8)
        .collect();

    // Clean up the transcript:
    // - Normalize whitespace
    // - Remove duplicate lines (auto-subs often repeat)
    let cleaned = clean_transcript(&raw_text);

    Ok(cleaned)
}

/// Clean up raw transcript text
fn clean_transcript(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        // Skip duplicate consecutive lines (auto-subs repeat the previous line on each update).
        .scan("", |prev, line| {
            let emit = *prev != line;
            *prev = line;
            Some(emit.then_some(line))
        })
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{clean_transcript, find_subtitle_path, parse_json3, TranscriptError};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn create_temp_test_dir(name: &str) -> PathBuf {
        let unique_id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "yt_gtk_{name}_{}_{}",
            std::process::id(),
            unique_id
        ));
        std::fs::create_dir_all(&path).expect("temporary test directory should be created");
        path
    }

    #[test]
    fn clean_transcript_removes_duplicate_adjacent_lines() {
        let raw = "hello\nhello\n\nworld\nworld\nnext";
        assert_eq!(clean_transcript(raw), "hello world next");
    }

    #[test]
    fn parse_json3_extracts_plain_text() {
        let json = r#"{
            "events": [
                {"segs": [{"utf8": "Hello "}, {"utf8": "world"}]},
                {"segs": [{"utf8": "\nHello "}, {"utf8": "again"}]}
            ]
        }"#;
        assert_eq!(
            parse_json3(json).expect("json3 should parse"),
            "Hello world Hello again"
        );
    }

    #[test]
    fn parse_json3_rejects_missing_events() {
        let json = r#"{"foo": "bar"}"#;
        let error = parse_json3(json).expect_err("missing events should fail");
        assert!(matches!(error, TranscriptError::MissingSubtitleEvents));
    }

    #[test]
    fn find_subtitle_path_rejects_non_delimited_prefix_match() {
        let temp_dir = create_temp_test_dir("subtitle_prefix_reject");
        let video_id = "abc123";
        let wrong_file = temp_dir.join("abc123xyz.en.json3");
        std::fs::write(&wrong_file, "{}").expect("test subtitle file should be written");

        let error =
            find_subtitle_path(&temp_dir, video_id).expect_err("non-delimited prefix should fail");

        assert!(matches!(
            error,
            TranscriptError::SubtitleFileNotFound { video_id: id } if id == video_id
        ));
        std::fs::remove_dir_all(&temp_dir).expect("temporary test directory should be removed");
    }

    #[test]
    fn find_subtitle_path_accepts_dot_delimited_prefix_match() {
        let temp_dir = create_temp_test_dir("subtitle_prefix_accept");
        let video_id = "abc123";
        let expected_file = temp_dir.join("abc123.en.json3");
        std::fs::write(&expected_file, "{}").expect("test subtitle file should be written");

        let found_path =
            find_subtitle_path(&temp_dir, video_id).expect("dot-delimited prefix should resolve");

        assert_eq!(found_path, expected_file);
        std::fs::remove_dir_all(&temp_dir).expect("temporary test directory should be removed");
    }
}
