use super::subtitle_requests::{SubtitleRateLimiter, run_yt_dlp_subtitle_command};
use crate::urls;
use serde::Deserialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

/// Monotonic counter that keeps concurrent fetches for the same video in
/// separate scratch directories, so one task never reads or deletes another's
/// half-written subtitle file.
static NEXT_FETCH_ID: AtomicU64 = AtomicU64::new(0);

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

/// Reads and parses a locally downloaded `WebVTT` subtitle file into transcript text.
///
/// # Arguments
///
/// * `subtitle_path` - Path to a `.vtt` file downloaded alongside a video.
///
/// # Returns
///
/// `Some(transcript)` when the file is readable and yields non-empty text.
pub async fn transcript_from_vtt_file(subtitle_path: &Path) -> Option<String> {
    let vtt = tokio::fs::read_to_string(subtitle_path).await.ok()?;
    let transcript = parse_vtt(&vtt);
    (!transcript.is_empty()).then_some(transcript)
}

/// Removes inline `WebVTT` markup spans such as `<c>` and `<00:00:01.319>`.
fn strip_vtt_tags(line: &str) -> String {
    let mut stripped = String::with_capacity(line.len());
    let mut in_tag = false;
    for character in line.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => stripped.push(c),
            _ => {}
        }
    }
    stripped
}

/// Parse `WebVTT` subtitle text into clean transcript text.
fn parse_vtt(vtt: &str) -> String {
    let text = vtt
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.contains("-->")
                || trimmed == "WEBVTT"
                || trimmed.starts_with("Kind:")
                || trimmed.starts_with("Language:")
                || trimmed.starts_with("NOTE"))
        })
        .map(strip_vtt_tags)
        .collect::<Vec<_>>()
        .join("\n");

    clean_transcript(&text)
}

/// Fetch transcript for a video using yt-dlp
///
/// # Arguments
///
/// * `video_id` - `YouTube` video identifier.
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
    // Per-fetch scratch subdirectory: concurrent fetches (e.g. summary prefetch
    // and the transcript dialog for the same video) must not share files.
    let fetch_dir = unique_fetch_dir(work_dir, video_id);
    tokio::fs::create_dir_all(&fetch_dir).await?;
    let result = fetch_transcript_in(video_id, &url, &fetch_dir).await;
    let _ = tokio::fs::remove_dir_all(&fetch_dir).await;
    result
}

fn unique_fetch_dir(work_dir: &Path, video_id: &str) -> PathBuf {
    let fetch_id = NEXT_FETCH_ID.fetch_add(1, Ordering::Relaxed);
    work_dir.join(format!("{video_id}.{}.{fetch_id}", std::process::id()))
}

async fn fetch_transcript_in(
    video_id: &str,
    url: &str,
    fetch_dir: &Path,
) -> Result<String, TranscriptError> {
    let output_template = fetch_dir.join(format!("{video_id}.%(ext)s"));

    // Run yt-dlp to download auto-generated subtitles in json3 format.
    let output = run_yt_dlp_subtitle_command(SubtitleRateLimiter::global(), video_id, || {
        let mut command = super::nice_command("yt-dlp");
        command
            .arg("--cookies-from-browser")
            .arg(super::cookies_browser())
            .arg("--write-auto-sub")
            .arg("--sub-format")
            .arg("json3")
            .arg("--skip-download")
            .arg("-o")
            .arg(&output_template)
            .arg("--no-playlist")
            .arg("--no-warnings")
            .arg(url)
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
    let subtitle_path = find_subtitle_path(fetch_dir, video_id)?;

    let json_content = tokio::fs::read_to_string(&subtitle_path).await?;
    parse_json3(&json_content)
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
                    .is_some_and(|name| name.starts_with(&expected_prefix))
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
    use super::{TranscriptError, clean_transcript, find_subtitle_path, parse_json3};
    use tempfile::TempDir;

    fn create_temp_test_dir() -> TempDir {
        tempfile::tempdir().expect("temporary test directory should be created")
    }

    #[test]
    fn parse_vtt_strips_headers_timestamps_and_inline_tags() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.000 --> 00:00:02.000 align:start position:0%\nhello<00:00:01.319><c> world</c>\n\n00:00:02.000 --> 00:00:04.000\nhello world\nnext line\n";
        assert_eq!(super::parse_vtt(vtt), "hello world next line");
    }

    #[test]
    fn parse_vtt_returns_empty_for_metadata_only_input() {
        assert_eq!(super::parse_vtt("WEBVTT\nKind: captions\n"), "");
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
        let temp_dir = create_temp_test_dir();
        let video_id = "abc123";
        let wrong_file = temp_dir.path().join("abc123xyz.en.json3");
        std::fs::write(&wrong_file, "{}").expect("test subtitle file should be written");

        let error = find_subtitle_path(temp_dir.path(), video_id)
            .expect_err("non-delimited prefix should fail");

        assert!(matches!(
            error,
            TranscriptError::SubtitleFileNotFound { video_id: id } if id == video_id
        ));
    }

    #[test]
    fn find_subtitle_path_accepts_dot_delimited_prefix_match() {
        let temp_dir = create_temp_test_dir();
        let video_id = "abc123";
        let expected_file = temp_dir.path().join("abc123.en.json3");
        std::fs::write(&expected_file, "{}").expect("test subtitle file should be written");

        let found_path = find_subtitle_path(temp_dir.path(), video_id)
            .expect("dot-delimited prefix should resolve");

        assert_eq!(found_path, expected_file);
    }
}
