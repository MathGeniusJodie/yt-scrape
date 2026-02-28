use crate::data::{Video, WatchLaterData};
use anyhow::Result;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const MAX_TITLE_LENGTH: usize = 100;
const WATCH_LATER_FILE: &str = "watch_later.json";
const VIDEOS_CACHE_FILE: &str = "videos.json";
const VIDEO_EXTENSIONS: [&str; 3] = ["mkv", "mp4", "webm"];

/// Sanitizes free-form title text into a stable filename-safe component.
fn sanitize_filename(input: &str) -> String {
    let sanitized = input
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_TITLE_LENGTH)
        .collect::<String>();

    sanitized.trim().to_string()
}

/// Manages filesystem-backed persistence for video metadata and cached assets.
pub struct Storage {
    data_dir: PathBuf,
    cache_dir: PathBuf,
    thumbnails_dir: PathBuf,
    videos_dir: PathBuf,
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

        let data_dir = project_dirs.data_dir().to_path_buf();
        let cache_dir = project_dirs.cache_dir().to_path_buf();
        let thumbnails_dir = cache_dir.join("thumbnails");
        let videos_dir = cache_dir.join("videos");
        let transcripts_work_dir = cache_dir.join("transcripts_work");

        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&thumbnails_dir)?;
        std::fs::create_dir_all(&videos_dir)?;
        std::fs::create_dir_all(&transcripts_work_dir)?;

        Ok(Self {
            data_dir,
            cache_dir,
            thumbnails_dir,
            videos_dir,
            transcripts_work_dir,
        })
    }

    /// Returns the cache directory path.
    #[allow(dead_code)]
    pub fn cache_dir(&self) -> &Path {
        self.cache_dir.as_path()
    }

    /// Returns the thumbnail cache directory path.
    #[allow(dead_code)]
    pub fn thumbnails_dir(&self) -> &Path {
        self.thumbnails_dir.as_path()
    }

    /// Returns the local videos directory path.
    #[allow(dead_code)]
    pub fn videos_dir(&self) -> &Path {
        self.videos_dir.as_path()
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
        let suffix = format!("_{video_id}");
        let entries = std::fs::read_dir(&self.videos_dir).ok()?;

        entries.filter_map(Result::ok).find_map(|entry| {
            let path = entry.path();
            let extension = path.extension().and_then(OsStr::to_str)?;
            if !VIDEO_EXTENSIONS.contains(&extension) {
                return None;
            }

            let stem = path.file_stem().and_then(OsStr::to_str)?;
            stem.ends_with(&suffix).then_some(path)
        })
    }

    /// Checks whether a local video file exists for the given video ID.
    ///
    /// # Arguments
    ///
    /// * `video_id` - YouTube video identifier.
    ///
    /// # Returns
    ///
    /// `true` when a local file exists.
    pub fn has_video(&self, video_id: &str) -> bool {
        self.find_video_path(video_id).is_some()
    }

    /// Loads watch-later IDs from disk.
    ///
    /// # Returns
    ///
    /// Saved IDs. If file loading or parsing fails, an empty set is returned.
    pub fn load_watch_later(&self) -> HashSet<String> {
        let path = self.data_dir.join(WATCH_LATER_FILE);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str::<WatchLaterData>(&content).ok())
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
    /// # Returns
    ///
    /// Cached videos, or an empty vector if cache loading/parsing fails.
    pub fn load_videos(&self) -> Vec<Video> {
        let path = self.cache_dir.join(VIDEOS_CACHE_FILE);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
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

    /// Updates a video's transcript and persists the video cache.
    ///
    /// # Arguments
    ///
    /// * `videos` - Mutable in-memory video list.
    /// * `video_id` - Target video identifier.
    /// * `transcript` - Transcript payload.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the updated cache fails.
    #[allow(dead_code)]
    pub fn save_transcript(
        &self,
        videos: &mut [Video],
        video_id: &str,
        transcript: String,
    ) -> Result<()> {
        if let Some(video) = videos.iter_mut().find(|video| video.video_id == video_id) {
            video.transcript = Some(transcript);
            self.save_videos(videos)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{sanitize_filename, MAX_TITLE_LENGTH};

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
}
