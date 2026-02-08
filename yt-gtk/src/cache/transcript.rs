use crate::urls;
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;
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

/// Fetch transcript for a video using yt-dlp
pub async fn fetch_transcript(video_id: &str, work_dir: &Path) -> Result<String> {
    let url = urls::watch_url(video_id);
    let output_template = work_dir.join(format!("{}.%(ext)s", video_id));

    // Run yt-dlp to download auto-generated subtitles in json3 format
    let status = Command::new("yt-dlp")
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
        .stderr(Stdio::null())
        .status()
        .await?;

    if !status.success() {
        anyhow::bail!("yt-dlp failed to fetch subtitles");
    }

    // Find the downloaded subtitle file (could be .en.json3, .en-US.json3, etc.)
    let pattern = format!("{}*.json3", video_id);
    let subtitle_path = glob::glob(work_dir.join(&pattern).to_str().unwrap_or(""))?
        .filter_map(Result::ok)
        .next()
        .ok_or_else(|| anyhow::anyhow!("No subtitle file found"))?;

    // Read and parse the JSON3 file
    let json_content = tokio::fs::read_to_string(&subtitle_path).await?;
    let transcript = parse_json3(&json_content)?;

    // Clean up the subtitle file
    let _ = tokio::fs::remove_file(&subtitle_path).await;

    Ok(transcript)
}

/// Parse JSON3 subtitle format into clean text
fn parse_json3(json: &str) -> Result<String> {
    let subtitle: Json3Subtitle = serde_json::from_str(json)?;

    let events = subtitle
        .events
        .ok_or_else(|| anyhow::anyhow!("No events in subtitle"))?;

    let mut text_parts: Vec<String> = Vec::new();

    for event in events {
        if let Some(segs) = event.segs {
            for seg in segs {
                if let Some(text) = seg.utf8 {
                    text_parts.push(text);
                }
            }
        }
    }

    // Join all text and clean it up
    let raw_text = text_parts.join("");

    // Clean up the transcript:
    // - Normalize whitespace
    // - Remove duplicate lines (auto-subs often repeat)
    let cleaned = clean_transcript(&raw_text);

    Ok(cleaned)
}

/// Clean up raw transcript text
fn clean_transcript(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut prev_line = "";

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip duplicate consecutive lines
        if trimmed != prev_line {
            lines.push(trimmed);
            prev_line = trimmed;
        }
    }

    // Join with spaces, then normalize multiple spaces
    let joined = lines.join(" ");
    let normalized: String = joined.split_whitespace().collect::<Vec<_>>().join(" ");

    normalized
}
