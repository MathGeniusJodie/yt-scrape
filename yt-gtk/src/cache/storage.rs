use crate::data::{Video, WatchLaterData};
use anyhow::Result;
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const MAX_TITLE_LENGTH: usize = 100;
const WATCH_LATER_FILE: &str = "watch_later.json";
const VIDEOS_CACHE_FILE: &str = "videos.json";
const VIDEO_SIDECARS_DIR: &str = "video_sidecars";
const VIDEO_SIDECAR_EXTENSION: &str = "json";
const VIDEO_EXTENSIONS: [&str; 3] = ["mkv", "mp4", "webm"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VideoMetadataSidecar {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ai_summary: Option<String>,
}

impl VideoMetadataSidecar {
    fn is_empty(&self) -> bool {
        self.transcript.is_none() && self.ai_summary.is_none()
    }
}

fn video_id_from_stem(stem: &str) -> Option<&str> {
    // Split from the right so underscores in titles do not affect ID extraction.
    let (_, video_id) = stem.rsplit_once('_')?;
    (!video_id.is_empty()).then_some(video_id)
}

fn collect_cached_video_ids_from_dir(videos_dir: &Path) -> HashSet<String> {
    let entries = match std::fs::read_dir(videos_dir) {
        Ok(entries) => entries,
        Err(_) => return HashSet::new(),
    };

    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let extension = path.extension().and_then(OsStr::to_str)?;
            if !VIDEO_EXTENSIONS.contains(&extension) {
                return None;
            }

            let stem = path.file_stem().and_then(OsStr::to_str)?;
            video_id_from_stem(stem).map(str::to_string)
        })
        .collect()
}

/// Sanitizes free-form title text into a stable filename-safe component.
fn sanitize_filename(input: &str) -> String {
    // Map invalid characters first so the trim operates on the final character set.
    // We trim_end after take() because truncation can land on a trailing space.
    let mut s: String = input
        .trim()
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(MAX_TITLE_LENGTH)
        .collect();
    s.truncate(s.trim_end().len());
    s
}

/// Manages filesystem-backed persistence for video metadata and cached assets.
pub struct Storage {
    data_dir: PathBuf,
    cache_dir: PathBuf,
    thumbnails_dir: PathBuf,
    videos_dir: PathBuf,
    video_sidecars_dir: PathBuf,
    transcripts_work_dir: PathBuf,
}

impl Storage {
    /// Creates storage directories under OS-specific application data/cache paths.
    ///
    /// # Returns
    ///
    /// A fully initialized [`Storage`] value.
    ///
    /// # Errors
    ///
    /// Returns an error if project directories cannot be discovered or if directory creation
    /// fails.
    pub fn new() -> Result<Self> {
        let project_dirs = directories::ProjectDirs::from("", "", "yt-gtk")
            .ok_or_else(|| anyhow::anyhow!("Could not determine project directories"))?;
        Self::new_at(
            project_dirs.data_dir().to_path_buf(),
            project_dirs.cache_dir().to_path_buf(),
        )
    }

    /// Creates storage rooted at explicit data/cache directories.
    ///
    /// # Arguments
    ///
    /// * `data_dir` - Directory used for persisted user data.
    /// * `cache_dir` - Directory used for cache artifacts.
    ///
    /// # Returns
    ///
    /// A fully initialized [`Storage`] value.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation fails.
    pub fn new_at(data_dir: PathBuf, cache_dir: PathBuf) -> Result<Self> {
        let thumbnails_dir = cache_dir.join("thumbnails");
        let videos_dir = cache_dir.join("videos");
        let video_sidecars_dir = cache_dir.join(VIDEO_SIDECARS_DIR);
        let transcripts_work_dir = cache_dir.join("transcripts_work");

        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&thumbnails_dir)?;
        std::fs::create_dir_all(&videos_dir)?;
        std::fs::create_dir_all(&video_sidecars_dir)?;
        std::fs::create_dir_all(&transcripts_work_dir)?;

        Ok(Self {
            data_dir,
            cache_dir,
            thumbnails_dir,
            videos_dir,
            video_sidecars_dir,
            transcripts_work_dir,
        })
    }

    /// Returns the transcript extraction work directory path.
    pub fn transcripts_work_dir(&self) -> &Path {
        self.transcripts_work_dir.as_path()
    }

    /// Builds the cache path for a video's thumbnail image.
    ///
    /// # Arguments
    ///
    /// * `video_id` - YouTube video identifier.
    ///
    /// # Returns
    ///
    /// Destination thumbnail path in the cache directory.
    pub fn thumbnail_path(&self, video_id: &str) -> PathBuf {
        self.thumbnails_dir.join(format!("{video_id}.jpg"))
    }

    /// Builds the target path for a downloaded video file.
    ///
    /// # Arguments
    ///
    /// * `video_id` - YouTube video identifier.
    /// * `title` - Raw video title used for filename generation.
    ///
    /// # Returns
    ///
    /// Destination video path in the local videos directory.
    pub fn video_path(&self, video_id: &str, title: &str) -> PathBuf {
        let sanitized_title = sanitize_filename(title);
        self.videos_dir
            .join(format!("{sanitized_title}_{video_id}.mkv"))
    }

    /// Finds an existing local video file for a given video ID.
    ///
    /// This lookup accepts any supported extension in [`VIDEO_EXTENSIONS`], regardless of title.
    ///
    /// # Arguments
    ///
    /// * `video_id` - YouTube video identifier.
    ///
    /// # Returns
    ///
    /// Matched local file path, if present.
    pub fn find_video_path(&self, video_id: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(&self.videos_dir).ok()?;

        entries.filter_map(Result::ok).find_map(|entry| {
            let path = entry.path();
            let extension = path.extension().and_then(OsStr::to_str)?;
            if !VIDEO_EXTENSIONS.contains(&extension) {
                return None;
            }

            let stem = path.file_stem().and_then(OsStr::to_str)?;
            (video_id_from_stem(stem) == Some(video_id)).then_some(path)
        })
    }

    /// Scans the local videos directory and returns all discovered downloaded video IDs.
    ///
    /// # Returns
    ///
    /// A set of video IDs extracted from cached video filenames.
    pub fn cached_video_ids(&self) -> HashSet<String> {
        collect_cached_video_ids_from_dir(&self.videos_dir)
    }

    fn video_sidecar_path(&self, video_id: &str) -> PathBuf {
        self.video_sidecars_dir
            .join(format!("{video_id}.{VIDEO_SIDECAR_EXTENSION}"))
    }

    fn read_video_sidecar(&self, video_id: &str) -> Option<VideoMetadataSidecar> {
        let path = self.video_sidecar_path(video_id);
        let contents = std::fs::read_to_string(&path).ok()?;
        match serde_json::from_str::<VideoMetadataSidecar>(&contents) {
            Ok(sidecar) => Some(sidecar),
            Err(parse_error) => {
                warn!(
                    "Failed to parse video sidecar {}: {}",
                    path.display(),
                    parse_error
                );
                None
            }
        }
    }

    fn write_video_sidecar(&self, video_id: &str, sidecar: &VideoMetadataSidecar) -> Result<()> {
        let path = self.video_sidecar_path(video_id);
        if sidecar.is_empty() {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            return Ok(());
        }

        let json = serde_json::to_string_pretty(sidecar)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Loads watch-later IDs from disk.
    ///
    /// # Returns
    ///
    /// Saved IDs. If file loading or parsing fails, an empty set is returned.
    pub fn load_watch_later(&self) -> HashSet<String> {
        let path = self.data_dir.join(WATCH_LATER_FILE);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return HashSet::new();
            }
            Err(error) => {
                warn!(
                    "Failed to read watch-later file {}: {}",
                    path.display(),
                    error
                );
                return HashSet::new();
            }
        };

        match serde_json::from_str::<WatchLaterData>(&content) {
            Ok(data) => data.video_ids.into_iter().collect(),
            Err(error) => {
                warn!(
                    "Failed to parse watch-later file {}: {}",
                    path.display(),
                    error
                );
                HashSet::new()
            }
        }
    }

    /// Saves watch-later IDs to disk using deterministic ordering.
    ///
    /// # Arguments
    ///
    /// * `video_ids` - Current set of watch-later IDs.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or file writes fail.
    pub fn save_watch_later(&self, video_ids: &HashSet<String>) -> Result<()> {
        let path = self.data_dir.join(WATCH_LATER_FILE);
        let mut sorted_video_ids = video_ids.iter().cloned().collect::<Vec<_>>();
        sorted_video_ids.sort_unstable();

        let data = WatchLaterData {
            video_ids: sorted_video_ids,
        };
        let json = serde_json::to_string_pretty(&data)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Loads cached video metadata from disk.
    ///
    /// Metadata from per-video sidecar files is merged into each loaded video.
    ///
    /// # Returns
    ///
    /// Cached videos, or an empty vector if cache loading/parsing fails.
    pub fn load_videos(&self) -> Vec<Video> {
        let path = self.cache_dir.join(VIDEOS_CACHE_FILE);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                warn!("Failed to read videos cache {}: {}", path.display(), error);
                return Vec::new();
            }
        };

        let mut videos: Vec<Video> = match serde_json::from_str(&content) {
            Ok(videos) => videos,
            Err(error) => {
                warn!("Failed to parse videos cache {}: {}", path.display(), error);
                return Vec::new();
            }
        };

        for video in &mut videos {
            if let Some(sidecar) = self.read_video_sidecar(video.video_id()) {
                if let Some(transcript) = sidecar.transcript {
                    video.set_transcript(Some(transcript));
                }
                if let Some(ai_summary) = sidecar.ai_summary {
                    video.set_ai_summary(Some(ai_summary));
                }
            }
        }

        videos
    }

    /// Persists video metadata to cache.
    ///
    /// # Arguments
    ///
    /// * `videos` - Video list to serialize.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or file writes fail.
    pub fn save_videos(&self, videos: &[Video]) -> Result<()> {
        let path = self.cache_dir.join(VIDEOS_CACHE_FILE);
        let json = serde_json::to_string_pretty(videos)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Persists transcript and/or AI summary in a per-video sidecar file.
    ///
    /// # Arguments
    ///
    /// * `video_id` - YouTube video identifier.
    /// * `transcript` - Optional transcript text to write.
    /// * `ai_summary` - Optional summary text to write.
    ///
    /// # Errors
    ///
    /// Returns an error when sidecar loading, serialization, or file writes fail.
    pub fn save_video_metadata(
        &self,
        video_id: &str,
        transcript: Option<&str>,
        ai_summary: Option<&str>,
    ) -> Result<()> {
        let mut sidecar = self.read_video_sidecar(video_id).unwrap_or_default();
        if let Some(transcript) = transcript {
            sidecar.transcript = Some(transcript.to_string());
        }
        if let Some(ai_summary) = ai_summary {
            sidecar.ai_summary = Some(ai_summary.to_string());
        }
        self.write_video_sidecar(video_id, &sidecar)
    }

    /// Persists transcript text in a per-video sidecar file.
    ///
    /// # Arguments
    ///
    /// * `video_id` - YouTube video identifier.
    /// * `transcript` - Normalized transcript text to cache.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the sidecar is persisted.
    ///
    /// # Errors
    ///
    /// Returns an error when sidecar loading, serialization, or file writes fail.
    pub fn save_video_transcript(&self, video_id: &str, transcript: &str) -> Result<()> {
        self.save_video_metadata(video_id, Some(transcript), None)
    }

    /// Persists AI summary text in a per-video sidecar file.
    ///
    /// # Arguments
    ///
    /// * `video_id` - YouTube video identifier.
    /// * `ai_summary` - Summary text to cache.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the sidecar is persisted.
    ///
    /// # Errors
    ///
    /// Returns an error when sidecar loading, serialization, or file writes fail.
    pub fn save_video_ai_summary(&self, video_id: &str, ai_summary: &str) -> Result<()> {
        self.save_video_metadata(video_id, None, Some(ai_summary))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_cached_video_ids_from_dir, sanitize_filename, video_id_from_stem, Storage,
        VideoMetadataSidecar, MAX_TITLE_LENGTH, VIDEOS_CACHE_FILE,
    };
    use crate::data::Video;
    use chrono::{DateTime, Utc};
    use std::fs::File;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_unique_temp_dir() -> std::path::PathBuf {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System clock should be after UNIX epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "yt_gtk_storage_tests_{}_{}",
            std::process::id(),
            unique_suffix
        ));
        std::fs::create_dir_all(&path).expect("Failed to create temp directory");
        path
    }

    fn create_test_storage(root: &Path) -> Storage {
        let data_dir = root.join("data");
        let cache_dir = root.join("cache");
        Storage::new_at(data_dir, cache_dir).expect("test storage should initialize")
    }

    fn sample_video(video_id: &str) -> Video {
        let published =
            DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z").expect("timestamp should parse");
        Video::new(
            video_id.to_string(),
            "UC123".to_string(),
            "Channel".to_string(),
            format!("Title {video_id}"),
            published.with_timezone(&Utc),
            "https://example.com/thumb.jpg".to_string(),
            Some(120),
        )
    }

    #[test]
    fn sanitize_filename_replaces_invalid_characters() {
        let input = r#"title:/\:*?"<>|"#;
        let output = sanitize_filename(input);
        assert_eq!(output, "title__________");
    }

    #[test]
    fn sanitize_filename_trims_and_limits_length() {
        let output = sanitize_filename(&"  very long title ".repeat(12));
        assert!(output.len() <= MAX_TITLE_LENGTH);
        assert!(!output.starts_with(' '));
        assert!(!output.ends_with(' '));
    }

    #[test]
    fn video_id_from_stem_extracts_suffix_after_last_underscore() {
        assert_eq!(
            video_id_from_stem("title_with_underscores_abc123"),
            Some("abc123")
        );
        assert_eq!(video_id_from_stem("_abc123"), Some("abc123"));
        assert_eq!(video_id_from_stem("single_separator"), Some("separator"));
        assert_eq!(video_id_from_stem("missing_suffix_"), None);
    }

    #[test]
    fn collect_cached_video_ids_from_dir_only_keeps_supported_video_files() {
        let temp_dir = create_unique_temp_dir();

        File::create(temp_dir.join("my_title_abc123.mkv")).expect("Failed to create mkv file");
        File::create(temp_dir.join("another_title_xyz789.mp4")).expect("Failed to create mp4 file");
        File::create(temp_dir.join("skip_me.txt")).expect("Failed to create text file");
        File::create(temp_dir.join("missing_suffix_.webm")).expect("Failed to create webm file");

        let ids = collect_cached_video_ids_from_dir(&temp_dir);
        assert!(ids.contains("abc123"));
        assert!(ids.contains("xyz789"));
        assert_eq!(ids.len(), 2);

        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }

    #[test]
    fn saving_sidecar_fields_does_not_rewrite_videos_cache_file() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);
        let video = sample_video("abc123");
        storage
            .save_videos(&[video])
            .expect("Saving baseline videos cache should succeed");

        let videos_path = storage.cache_dir.join(VIDEOS_CACHE_FILE);
        let videos_before =
            std::fs::read_to_string(&videos_path).expect("Baseline videos cache should exist");

        storage
            .save_video_metadata("abc123", Some("Transcript body"), Some("Summary body"))
            .expect("Saving sidecar metadata should succeed");

        let videos_after =
            std::fs::read_to_string(&videos_path).expect("Videos cache should remain readable");
        assert_eq!(videos_before, videos_after);

        let sidecar = storage
            .read_video_sidecar("abc123")
            .expect("Expected sidecar to be written");
        assert_eq!(sidecar.transcript.as_deref(), Some("Transcript body"));
        assert_eq!(sidecar.ai_summary.as_deref(), Some("Summary body"));

        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }

    #[test]
    fn load_videos_applies_sidecar_fields() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);

        storage
            .save_videos(&[sample_video("mixed456")])
            .expect("Writing videos cache should succeed");

        storage
            .write_video_sidecar(
                "mixed456",
                &VideoMetadataSidecar {
                    transcript: Some("sidecar transcript".to_string()),
                    ai_summary: Some("sidecar summary".to_string()),
                },
            )
            .expect("Writing existing sidecar should succeed");

        let loaded = storage.load_videos();
        assert_eq!(loaded.len(), 1);

        let mixed_loaded = loaded
            .iter()
            .find(|video| video.video_id() == "mixed456")
            .expect("Video should be present");
        assert_eq!(mixed_loaded.transcript(), Some("sidecar transcript"));
        assert_eq!(mixed_loaded.ai_summary(), Some("sidecar summary"));

        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }
}
