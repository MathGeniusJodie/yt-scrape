use crate::data::{Video, WatchLaterData};
use log::warn;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_TITLE_LENGTH: usize = 100;
const WATCH_LATER_FILE: &str = "watch_later.json";
const VIDEOS_CACHE_FILE: &str = "videos.json";
const VIDEO_SIDECARS_DIR: &str = "video_sidecars";
const VIDEO_SIDECAR_EXTENSION: &str = "json";
const VIDEO_EXTENSIONS: [&str; 3] = ["mkv", "mp4", "webm"];
const YOUTUBE_VIDEO_ID_LENGTH: usize = 11;

type StorageResult<T> = std::result::Result<T, StorageError>;

/// Errors produced by filesystem-backed storage operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Platform project directories could not be discovered.
    #[error("Could not determine project directories")]
    ProjectDirectoriesUnavailable,
    /// Filesystem operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization or parsing failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

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
    let id_start = stem.len().checked_sub(YOUTUBE_VIDEO_ID_LENGTH)?;
    let video_id = stem.get(id_start..)?;
    if !video_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return None;
    }
    if id_start == 0 || stem[..id_start].ends_with('_') {
        Some(video_id)
    } else {
        None
    }
}

/// Reads and deserializes a JSON file, returning `None` on any failure.
///
/// Missing files are silently ignored; read/parse errors are logged as warnings.
fn try_load_json_file<T: DeserializeOwned>(path: &Path, context: &str) -> Option<T> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!("Failed to read {} {}: {}", context, path.display(), error);
            return None;
        }
    };
    match serde_json::from_str(&content) {
        Ok(value) => Some(value),
        Err(error) => {
            warn!("Failed to parse {} {}: {}", context, path.display(), error);
            None
        }
    }
}

fn collect_cached_video_ids_from_dir(videos_dir: &Path) -> HashSet<String> {
    let entries = match std::fs::read_dir(videos_dir) {
        Ok(entries) => entries,
        Err(_) => return HashSet::new(),
    };

    entries
        .filter_map(Result::ok)
        .filter_map(|entry| cached_video_id_for_path(&entry.path()).map(str::to_string))
        .collect()
}

fn cached_video_id_for_path(path: &Path) -> Option<&str> {
    let extension = path.extension().and_then(OsStr::to_str)?;
    if !VIDEO_EXTENSIONS.contains(&extension) {
        return None;
    }

    let stem = path.file_stem().and_then(OsStr::to_str)?;
    video_id_from_stem(stem)
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
#[derive(Clone)]
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
    pub fn new() -> StorageResult<Self> {
        let project_dirs = directories::ProjectDirs::from("", "", "yt-gtk")
            .ok_or(StorageError::ProjectDirectoriesUnavailable)?;
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
    pub fn new_at(data_dir: PathBuf, cache_dir: PathBuf) -> StorageResult<Self> {
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
            (cached_video_id_for_path(&path) == Some(video_id)).then_some(path)
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

    /// Deletes all cached video files for a single video ID.
    ///
    /// This removes every matching file in the videos cache directory across supported
    /// extensions and titles.
    ///
    /// # Arguments
    ///
    /// * `video_id` - YouTube video identifier to remove from local video cache.
    ///
    /// # Returns
    ///
    /// Number of files removed.
    ///
    /// # Errors
    ///
    /// Returns an error if directory scanning or file deletion fails.
    pub fn remove_cached_video_files(&self, video_id: &str) -> StorageResult<usize> {
        let entries = std::fs::read_dir(&self.videos_dir)?;
        let mut removed_count = 0usize;

        for entry in entries {
            let path = entry?.path();
            if cached_video_id_for_path(&path) == Some(video_id) {
                std::fs::remove_file(path)?;
                removed_count += 1;
            }
        }

        Ok(removed_count)
    }

    /// Removes cached video files that are no longer in watch later.
    ///
    /// # Arguments
    ///
    /// * `watch_later` - Set of video IDs that are allowed to remain in the video cache folder.
    ///
    /// # Returns
    ///
    /// Number of cached video files removed.
    ///
    /// # Errors
    ///
    /// Returns an error if directory scanning or file deletion fails.
    pub fn prune_cached_videos_not_in_watch_later(
        &self,
        watch_later: &HashSet<String>,
    ) -> StorageResult<usize> {
        let entries = std::fs::read_dir(&self.videos_dir)?;
        let mut removed_count = 0usize;

        for entry in entries {
            let path = entry?.path();
            let Some(cached_video_id) = cached_video_id_for_path(&path) else {
                continue;
            };
            if watch_later.contains(cached_video_id) {
                continue;
            }
            std::fs::remove_file(path)?;
            removed_count += 1;
        }

        Ok(removed_count)
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

    fn apply_sidecar_metadata_to_video(&self, video: &mut Video) {
        let Some(sidecar) = self.read_video_sidecar(video.video_id()) else {
            return;
        };
        if let Some(transcript) = sidecar.transcript {
            video.set_transcript(Some(transcript));
        }
        if let Some(ai_summary) = sidecar.ai_summary {
            video.set_ai_summary(Some(ai_summary));
        }
    }

    fn write_video_sidecar(
        &self,
        video_id: &str,
        sidecar: &VideoMetadataSidecar,
    ) -> StorageResult<()> {
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
        try_load_json_file::<WatchLaterData>(&path, "watch-later file")
            .map(|data| data.video_ids.into_iter().collect())
            .unwrap_or_default()
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
    pub fn save_watch_later(&self, video_ids: &HashSet<String>) -> StorageResult<()> {
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

    /// Hydrates in-memory videos with transcript/summary metadata from sidecar files.
    ///
    /// # Arguments
    ///
    /// * `videos` - Videos to hydrate with sidecar transcript/summary data.
    pub fn hydrate_videos_from_sidecars(&self, videos: &mut [Video]) {
        for video in videos {
            self.apply_sidecar_metadata_to_video(video);
        }
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
        let mut videos: Vec<Video> = try_load_json_file(&path, "videos cache").unwrap_or_default();
        self.hydrate_videos_from_sidecars(&mut videos);
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
    pub fn save_videos(&self, videos: &[Video]) -> StorageResult<()> {
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
    ) -> StorageResult<()> {
        let mut sidecar = self.read_video_sidecar(video_id).unwrap_or_default();
        if let Some(transcript) = transcript {
            sidecar.transcript = Some(transcript.to_string());
        }
        if let Some(ai_summary) = ai_summary {
            sidecar.ai_summary = Some(ai_summary.to_string());
        }
        self.write_video_sidecar(video_id, &sidecar)
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

    fn touch(path: &Path) {
        File::create(path).expect("test file must be creatable");
    }

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
    fn video_id_from_stem_extracts_trailing_youtube_id() {
        assert_eq!(
            video_id_from_stem("title_with_underscores_C9ww_8cg_5g"),
            Some("C9ww_8cg_5g")
        );
        assert_eq!(video_id_from_stem("C9ww_8cg_5g"), Some("C9ww_8cg_5g"));
        assert_eq!(
            video_id_from_stem("trailing_abc123DEF45"),
            Some("abc123DEF45")
        );
        assert_eq!(video_id_from_stem("single_separator"), None);
        assert_eq!(video_id_from_stem("missing_suffix_"), None);
        assert_eq!(video_id_from_stem("invalid_id_prefix_abc123def$%"), None);
    }

    #[test]
    fn collect_cached_video_ids_from_dir_only_keeps_supported_video_files() {
        let temp_dir = create_unique_temp_dir();

        touch(&temp_dir.join("my_title_C9ww_8cg_5g.mkv"));
        touch(&temp_dir.join("another_title_abc123DEF45.mp4"));
        touch(&temp_dir.join("skip_me.txt"));
        touch(&temp_dir.join("missing_suffix_.webm"));

        let ids = collect_cached_video_ids_from_dir(&temp_dir);
        assert!(ids.contains("C9ww_8cg_5g"));
        assert!(ids.contains("abc123DEF45"));
        assert_eq!(ids.len(), 2);

        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }

    #[test]
    fn remove_cached_video_files_removes_all_matching_extensions() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);

        touch(&storage.videos_dir.join("title_C9ww_8cg_5g.mkv"));
        touch(&storage.videos_dir.join("title_C9ww_8cg_5g.mp4"));
        touch(&storage.videos_dir.join("other_abc123DEF45.webm"));

        let removed_count = storage
            .remove_cached_video_files("C9ww_8cg_5g")
            .expect("removal should succeed");
        assert_eq!(removed_count, 2);
        assert!(storage.find_video_path("C9ww_8cg_5g").is_none());
        assert!(storage.find_video_path("abc123DEF45").is_some());

        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }

    #[test]
    fn prune_cached_videos_not_in_watch_later_keeps_only_watch_later_files() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);

        touch(&storage.videos_dir.join("keep_C9ww_8cg_5g.mkv"));
        touch(&storage.videos_dir.join("remove_abc123DEF45.mp4"));
        touch(&storage.videos_dir.join("ignore.txt"));

        let watch_later = std::collections::HashSet::from(["C9ww_8cg_5g".to_string()]);
        let removed_count = storage
            .prune_cached_videos_not_in_watch_later(&watch_later)
            .expect("prune should succeed");

        assert_eq!(removed_count, 1);
        assert!(storage.find_video_path("C9ww_8cg_5g").is_some());
        assert!(storage.find_video_path("abc123DEF45").is_none());
        assert!(storage.videos_dir.join("ignore.txt").exists());

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

    #[test]
    fn hydrate_videos_from_sidecars_applies_metadata_for_existing_ids() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);

        storage
            .write_video_sidecar(
                "side789",
                &VideoMetadataSidecar {
                    transcript: Some("sidecar transcript".to_string()),
                    ai_summary: Some("sidecar summary".to_string()),
                },
            )
            .expect("Writing sidecar should succeed");

        let mut refreshed_videos = vec![sample_video("side789"), sample_video("none0000000")];
        storage.hydrate_videos_from_sidecars(&mut refreshed_videos);

        let hydrated = refreshed_videos
            .iter()
            .find(|video| video.video_id() == "side789")
            .expect("Hydrated video should be present");
        assert_eq!(hydrated.transcript(), Some("sidecar transcript"));
        assert_eq!(hydrated.ai_summary(), Some("sidecar summary"));

        let untouched = refreshed_videos
            .iter()
            .find(|video| video.video_id() == "none0000000")
            .expect("Untouched video should be present");
        assert_eq!(untouched.transcript(), None);
        assert_eq!(untouched.ai_summary(), None);

        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }
}
