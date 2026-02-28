use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

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

struct Chapter {
    start: f64,
    end: Option<f64>,
    title: String,
}

fn secs_to_ms(seconds: f64) -> i64 {
    (seconds.max(0.0) * 1000.0).round() as i64
}

fn escape_ffmetadata(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('=', "\\=")
        .replace(';', "\\;")
        .replace('#', "\\#")
        .replace('\n', " ")
}

/// Parse a MM:SS or HH:MM:SS token, stripping common surrounding punctuation.
fn parse_time_token(raw: &str) -> Option<f64> {
    let token = raw
        .trim_matches(['[', ']', '(', ')', '{', '}'])
        .trim_end_matches(['-', '|', ',', '.']);

    let mut parts = token.split(':');
    let a: u64 = parts.next()?.parse().ok()?;
    let b: u64 = parts.next()?.parse().ok()?;

    if let Some(c_str) = parts.next() {
        // Reject anything with more than 3 components
        if parts.next().is_some() {
            return None;
        }
        let c: u64 = c_str.parse().ok()?;
        Some((a * 3600 + b * 60 + c) as f64)
    } else {
        Some((a * 60 + b) as f64)
    }
}

/// Extract chapters from timestamp lines in a video description.
fn parse_description_chapters(description: &str) -> Vec<Chapter> {
    let mut chapters: Vec<Chapter> = description
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let first = line.split_whitespace().next()?;
            let start = parse_time_token(first)?;
            let title = line[first.len()..]
                .trim_start()
                .trim_start_matches(['-', '|', ':', ' ']);
            let title = if title.is_empty() { "Chapter" } else { title };
            Some(Chapter { start, end: None, title: escape_ffmetadata(title) })
        })
        .collect();

    chapters.sort_by(|a, b| a.start.total_cmp(&b.start));
    // Deduplicate entries within 1ms of each other
    chapters.dedup_by(|a, b| (a.start - b.start).abs() < 0.001);
    chapters
}

fn build_ffmetadata(info: &InfoJson) -> Option<String> {
    let chapters: Vec<Chapter> = if !info.chapters.is_empty() {
        info.chapters
            .iter()
            .filter_map(|c| {
                let start = c.start_time?;
                let title = c
                    .title
                    .as_deref()
                    .map(escape_ffmetadata)
                    .unwrap_or_else(|| "Chapter".to_string());
                Some(Chapter { start, end: c.end_time, title })
            })
            .collect()
    } else {
        info.description
            .as_deref()
            .map(parse_description_chapters)
            .unwrap_or_default()
    };

    if chapters.is_empty() {
        return None;
    }

    let mut ffmeta = String::from(";FFMETADATA1\n");
    for (i, chapter) in chapters.iter().enumerate() {
        // Prefer the chapter's own end time, then the next chapter's start, then the
        // video duration, falling back to start + 1s so the range is always valid.
        let end = chapter
            .end
            .filter(|&e| e > chapter.start)
            .or_else(|| {
                chapters
                    .get(i + 1)
                    .map(|next| next.start)
                    .filter(|&s| s > chapter.start)
            })
            .or_else(|| info.duration.filter(|&d| d > chapter.start))
            .unwrap_or(chapter.start + 1.0);

        ffmeta.push_str("[CHAPTER]\nTIMEBASE=1/1000\n");
        ffmeta.push_str(&format!(
            "START={}\nEND={}\ntitle={}\n",
            secs_to_ms(chapter.start),
            secs_to_ms(end),
            chapter.title,
        ));
    }

    Some(ffmeta)
}

fn load_info_json(path: &Path) -> Option<InfoJson> {
    let contents = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            log::debug!("chapters: failed to read {}: {}", path.display(), e);
            return None;
        }
    };

    match serde_json::from_str(&contents) {
        Ok(info) => Some(info),
        Err(e) => {
            log::debug!("chapters: failed to parse {}: {}", path.display(), e);
            None
        }
    }
}

/// Ensure a `.chapters.ffmeta` file exists beside `local_path`, creating it from
/// the co-located `info.json` sidecar if needed. Returns `None` if no chapter data
/// is available.
pub(super) fn ensure_chapters_file(local_path: &Path) -> Option<PathBuf> {
    let chapters_path = local_path.with_extension("chapters.ffmeta");
    if chapters_path.exists() {
        return Some(chapters_path);
    }

    let info = load_info_json(&local_path.with_extension("info.json"))?;
    let ffmeta = build_ffmetadata(&info)?;
    fs::write(&chapters_path, ffmeta).ok()?;
    Some(chapters_path)
}
