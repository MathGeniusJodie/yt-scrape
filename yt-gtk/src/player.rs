use crate::urls;
use std::path::Path;
use std::process::{Command, Stdio};
use std::{fs, io::Write, path::PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct InfoJson {
    #[serde(default)]
    chapters: Vec<InfoChapter>,
    duration: Option<f64>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InfoChapter {
    start_time: Option<f64>,
    end_time: Option<f64>,
    title: Option<String>,
}

fn secs_to_ms(seconds: f64) -> i64 {
    (seconds.max(0.0) * 1000.0).round() as i64
}

fn log_chapter_debug(message: &str) {
    let mut log_paths = vec![PathBuf::from("/tmp/yt-gtk-chapters.log")];
    if let Ok(home) = std::env::var("HOME") {
        log_paths.push(PathBuf::from(home).join(".cache/yt-gtk/yt-gtk-chapters.log"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        log_paths.push(cwd.join("yt-gtk-chapters.log"));
    }

    for path in log_paths {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{}", message);
            return;
        }
    }

    eprintln!("yt-gtk chapter debug: {}", message);
}

fn escape_ffmetadata(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('=', "\\=")
        .replace(';', "\\;")
        .replace('#', "\\#")
        .replace('\n', " ")
}

fn parse_time_token(raw: &str) -> Option<f64> {
    let token = raw
        .trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | '{' | '}'))
        .trim_end_matches(|c: char| matches!(c, '-' | '|' | ',' | '.'));
    let parts: Vec<&str> = token.split(':').collect();
    if parts.len() != 2 && parts.len() != 3 {
        return None;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }

    let nums: Vec<u64> = parts.iter().filter_map(|p| p.parse::<u64>().ok()).collect();
    if nums.len() != parts.len() {
        return None;
    }

    let seconds = if nums.len() == 2 {
        nums[0] * 60 + nums[1]
    } else {
        nums[0] * 3600 + nums[1] * 60 + nums[2]
    };

    Some(seconds as f64)
}

fn parse_description_chapters(description: &str) -> Vec<(f64, String)> {
    let mut parsed = Vec::new();

    for line in description.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let mut tokens = line.split_whitespace();
        let first = match tokens.next() {
            Some(token) => token,
            None => continue,
        };
        let start = match parse_time_token(first) {
            Some(value) => value,
            None => continue,
        };

        let mut title = line[first.len()..].trim_start();
        title = title.trim_start_matches(|c: char| matches!(c, '-' | '|' | ':' | ' '));
        if title.is_empty() {
            title = "Chapter";
        }

        parsed.push((start, escape_ffmetadata(title)));
    }

    parsed.sort_by(|a, b| a.0.total_cmp(&b.0));
    parsed.dedup_by(|a, b| (a.0 - b.0).abs() < 0.001);
    parsed
}

fn build_ffmetadata(info: &InfoJson) -> Option<String> {
    let mut starts = Vec::new();
    let mut titles = Vec::new();
    let mut ends = Vec::new();

    if !info.chapters.is_empty() {
        for chapter in &info.chapters {
            let Some(start) = chapter.start_time else {
                continue;
            };
            starts.push(start);
            titles.push(
                chapter
                    .title
                    .as_deref()
                    .map(escape_ffmetadata)
                    .unwrap_or_else(|| "Chapter".to_string()),
            );
            ends.push(chapter.end_time);
        }
    } else if let Some(description) = &info.description {
        let description_chapters = parse_description_chapters(description);
        for (start, title) in description_chapters {
            starts.push(start);
            titles.push(title);
            ends.push(None);
        }
    }

    if starts.is_empty() {
        return None;
    }

    let mut ffmeta = String::from(";FFMETADATA1\n");
    for idx in 0..starts.len() {
        let start = starts[idx];
        let mut end = ends[idx]
            .filter(|end| *end > start)
            .or_else(|| starts.get(idx + 1).copied().filter(|next| *next > start))
            .or_else(|| info.duration.filter(|duration| *duration > start))
            .unwrap_or(start + 1.0);

        if end <= start {
            end = start + 1.0;
        }

        ffmeta.push_str("[CHAPTER]\n");
        ffmeta.push_str("TIMEBASE=1/1000\n");
        ffmeta.push_str(&format!("START={}\n", secs_to_ms(start)));
        ffmeta.push_str(&format!("END={}\n", secs_to_ms(end)));
        ffmeta.push_str(&format!("title={}\n", titles[idx]));
    }

    Some(ffmeta)
}

fn load_info_json(path: &Path) -> Option<InfoJson> {
    let info_json = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            log_chapter_debug(&format!(
                "load_info_json: failed to read {}: {}",
                path.display(),
                err
            ));
            return None;
        }
    };

    match serde_json::from_str(&info_json) {
        Ok(info) => Some(info),
        Err(err) => {
            log_chapter_debug(&format!(
                "load_info_json: failed to parse {}: {}",
                path.display(),
                err
            ));
            None
        }
    }
}

fn fetch_info_json(video_id: &str) -> Option<InfoJson> {
    let url = urls::watch_url(video_id);
    let output = match Command::new("yt-dlp")
        .arg("--dump-single-json")
        .arg("--skip-download")
        .arg("--extractor-retries")
        .arg("3")
        .arg("--retries")
        .arg("3")
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            log_chapter_debug(&format!(
                "fetch_info_json: failed to run yt-dlp for {}: {}",
                video_id, err
            ));
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log_chapter_debug(&format!(
            "fetch_info_json: yt-dlp failed for {} with {:?}: {}",
            video_id,
            output.status.code(),
            stderr.trim()
        ));
        return None;
    }

    match serde_json::from_slice(&output.stdout) {
        Ok(info) => Some(info),
        Err(err) => {
            log_chapter_debug(&format!(
                "fetch_info_json: failed to parse yt-dlp json for {}: {}",
                video_id, err
            ));
            None
        }
    }
}

fn ensure_chapters_file(local_path: &Path, video_id: &str) -> Option<PathBuf> {
    let chapters_path = local_path.with_extension("chapters.ffmeta");
    log_chapter_debug(&format!(
        "ensure_chapters_file: video={} local={} chapters={}",
        video_id,
        local_path.display(),
        chapters_path.display()
    ));
    if chapters_path.exists() {
        log_chapter_debug(&format!(
            "ensure_chapters_file: chapters file already exists for {}",
            video_id
        ));
        return Some(chapters_path);
    }

    let info_json_path = local_path.with_extension("info.json");
    let info = load_info_json(&info_json_path).or_else(|| fetch_info_json(video_id))?;
    log_chapter_debug(&format!(
        "ensure_chapters_file: resolved info for {} with {} chapters",
        video_id,
        info.chapters.len()
    ));
    let ffmeta = build_ffmetadata(&info)?;

    if let Err(err) = fs::write(&chapters_path, ffmeta) {
        log_chapter_debug(&format!(
            "ensure_chapters_file: failed to write {}: {}",
            chapters_path.display(),
            err
        ));
        return None;
    }

    log_chapter_debug(&format!(
        "ensure_chapters_file: wrote {}",
        chapters_path.display()
    ));
    Some(chapters_path)
}

/// Play a video with mpv (detached process)
/// If a local file exists, play that; otherwise stream from YouTube
pub fn play_video(video_id: &str, title: &str, local_path: Option<&Path>) -> anyhow::Result<()> {
    log_chapter_debug(&format!(
        "play_video: id={} title={} local_path={}",
        video_id,
        title,
        local_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<none>".to_string())
    ));

    // Check if we have a local copy
    if let Some(path) = local_path {
        if path.exists() {
            // Play local file
            let mut command = Command::new("mpv");
            command
                .arg(format!("--title={}", title))
                .arg(format!("--force-media-title={}", title))
                .arg("--sub-auto=all")
                .arg("--sid=auto");
            let chapters_file = ensure_chapters_file(path, video_id);
            if let Some(ref chapters_file) = chapters_file {
                eprintln!(
                    "Using chapters file for {}: {}",
                    video_id,
                    chapters_file.display()
                );
                command.arg(format!("--chapters-file={}", chapters_file.display()));
            } else {
                eprintln!(
                    "No chapters metadata available for local video {}",
                    video_id
                );
            }
            command
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            return Ok(());
        }

        log_chapter_debug(&format!(
            "play_video: local path does not exist for {}: {}",
            video_id,
            path.display()
        ));
    }

    // Fallback: stream from YouTube
    log_chapter_debug(&format!("play_video: streaming fallback for {}", video_id));
    let url = urls::watch_url(video_id);
    Command::new("mpv")
        .arg(format!("--title={}", title))
        .arg(format!("--force-media-title={}", title))
        .arg("--sub-auto=all")
        .arg("--sid=auto")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(())
}
