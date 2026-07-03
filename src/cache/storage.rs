use crate::data::{NewVideo, Video, WatchLaterData};
use crate::urls;
use chrono::{DateTime, NaiveDate, Utc};
use log::warn;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_TITLE_LENGTH: usize = 100;
const WATCH_LATER_FILE: &str = "watch_later.json";
const VIDEOS_CACHE_FILE: &str = "videos.json";
const FEED_VIDEO_IDS_CACHE_FILE: &str = "feed_video_ids.json";
const THUMBNAILS_DIR: &str = "thumbnails";
const VIDEOS_DIR: &str = "videos";
const VIDEO_SIDECARS_DIR: &str = "video_sidecars";
const TRANSCRIPTS_WORK_DIR: &str = "transcripts_work";
const VIDEO_SIDECAR_EXTENSION: &str = "json";
const THUMBNAIL_EXTENSION: &str = "jpg";
const INFO_JSON_EXTENSION: &str = "json";
const VIDEO_EXTENSIONS: [&str; 3] = ["mkv", "mp4", "webm"];
const SUBTITLE_EXTENSION: &str = "vtt";
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

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VideoMetadataSidecar {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ai_summary: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    watched: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct VideoIdsData {
    video_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct YtDlpInfoJson {
    id: Option<String>,
    title: Option<String>,
    channel_id: Option<String>,
    channel: Option<String>,
    uploader: Option<String>,
    upload_date: Option<String>,
    timestamp: Option<i64>,
    release_timestamp: Option<i64>,
    thumbnail: Option<String>,
    duration: Option<f64>,
}

impl VideoMetadataSidecar {
    const fn is_empty(&self) -> bool {
        self.transcript.is_none() && self.ai_summary.is_none() && !self.watched
    }
}

impl YtDlpInfoJson {
    fn into_video(self, video_id: &str) -> Option<Video> {
        if self.id.as_deref().is_some_and(|id| id != video_id) {
            return None;
        }

        let published = self
            .release_timestamp
            .or(self.timestamp)
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
            .or_else(|| published_from_upload_date(self.upload_date.as_deref()))?;
        let duration_seconds = self.duration.and_then(duration_to_seconds);

        Some(Video::new(NewVideo {
            video_id: video_id.to_string(),
            channel_id: self.channel_id.unwrap_or_default(),
            channel_name: self
                .channel
                .or(self.uploader)
                .unwrap_or_else(|| "Unknown channel".to_string()),
            title: self.title.unwrap_or_else(|| "Untitled".to_string()),
            published,
            thumbnail_url: self
                .thumbnail
                .unwrap_or_else(|| urls::thumbnail_url(video_id)),
            duration_seconds,
        }))
    }
}

fn duration_to_seconds(duration: f64) -> Option<u32> {
    #[allow(clippy::cast_sign_loss)]
    if duration.is_finite() && duration >= 0.0 && duration <= f64::from(u32::MAX) {
        Some(duration.round() as u32)
    } else {
        None
    }
}

fn published_from_upload_date(upload_date: Option<&str>) -> Option<DateTime<Utc>> {
    NaiveDate::parse_from_str(upload_date?, "%Y%m%d")
        .ok()?
        .and_hms_opt(0, 0, 0)?
        .and_local_timezone(Utc)
        .single()
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

fn is_valid_video_id(video_id: &str) -> bool {
    video_id.len() == YOUTUBE_VIDEO_ID_LENGTH
        && video_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

/// Reads and deserializes a JSON file, returning `None` on any failure.
///
/// Missing files are silently ignored; read/parse errors are logged as warnings.
fn try_load_json_file<T: DeserializeOwned>(path: &Path, context: &str) -> Option<T> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("Failed to read {} {}: {}", context, path.display(), e);
            return None;
        }
    };
    serde_json::from_reader(file)
        .map_err(|e| warn!("Failed to parse {} {}: {}", context, path.display(), e))
        .ok()
}

fn collect_cached_video_ids_from_dir(videos_dir: &Path) -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(videos_dir) else {
        return HashSet::new();
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

fn video_id_from_subtitle_path(path: &Path) -> Option<&str> {
    if path.extension().and_then(OsStr::to_str) != Some(SUBTITLE_EXTENSION) {
        return None;
    }

    path.file_stem()
        .and_then(OsStr::to_str)
        .and_then(|stem| stem.split('.').next())
        .and_then(video_id_from_stem)
}

fn video_id_from_info_json_path(path: &Path) -> Option<&str> {
    if path.extension().and_then(OsStr::to_str) != Some(INFO_JSON_EXTENSION) {
        return None;
    }

    path.file_name()
        .and_then(OsStr::to_str)
        .and_then(|file_name| file_name.strip_suffix(".info.json"))
        .and_then(video_id_from_stem)
}

fn video_id_from_thumbnail_path(path: &Path) -> Option<&str> {
    if path.extension().and_then(OsStr::to_str) != Some(THUMBNAIL_EXTENSION) {
        return None;
    }

    path.file_stem()
        .and_then(OsStr::to_str)
        .filter(|video_id| is_valid_video_id(video_id))
}

fn video_id_from_sidecar_path(path: &Path) -> Option<&str> {
    if path.extension().and_then(OsStr::to_str) != Some(VIDEO_SIDECAR_EXTENSION) {
        return None;
    }

    path.file_stem()
        .and_then(OsStr::to_str)
        .filter(|video_id| is_valid_video_id(video_id))
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
#[allow(clippy::struct_field_names)]
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
        let thumbnails_dir = cache_dir.join(THUMBNAILS_DIR);
        let videos_dir = cache_dir.join(VIDEOS_DIR);
        let video_sidecars_dir = cache_dir.join(VIDEO_SIDECARS_DIR);
        let transcripts_work_dir = cache_dir.join(TRANSCRIPTS_WORK_DIR);

        let storage = Self {
            data_dir,
            cache_dir,
            thumbnails_dir,
            videos_dir,
            video_sidecars_dir,
            transcripts_work_dir,
        };
        storage.ensure_directories()?;
        Ok(storage)
    }

    fn ensure_directories(&self) -> StorageResult<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        Self::ensure_owned_cache_dir(&self.thumbnails_dir)?;
        Self::ensure_owned_cache_dir(&self.videos_dir)?;
        Self::ensure_owned_cache_dir(&self.video_sidecars_dir)?;
        Self::ensure_owned_cache_dir(&self.transcripts_work_dir)?;
        Ok(())
    }

    fn ensure_owned_cache_dir(path: &Path) -> StorageResult<()> {
        if path.exists() && !path.is_dir() {
            std::fs::remove_file(path)?;
        }
        std::fs::create_dir_all(path)?;
        Ok(())
    }

    fn remove_path(path: &Path) -> StorageResult<usize> {
        if path.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
        Ok(1)
    }

    #[allow(clippy::unused_self)]
    fn prune_directory_entries<F>(&self, dir: &Path, keep_entry: F) -> StorageResult<usize>
    where
        F: Fn(&Path) -> bool,
    {
        let mut removed_count = 0usize;
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if keep_entry(&path) {
                continue;
            }
            removed_count += Self::remove_path(&path)?;
        }
        Ok(removed_count)
    }

    /// Removes cache artifacts that cannot be referenced by current app state.
    ///
    /// Downloaded video artifacts are kept only for Watch Later IDs. Card metadata artifacts such
    /// as thumbnails and sidecars are kept only for IDs present in the currently loaded feed.
    ///
    /// # Arguments
    ///
    /// * `watch_later` - Video IDs allowed to keep downloaded media, subtitles, and info JSON.
    /// * `feed_video_ids` - Video IDs allowed to keep card display artifacts.
    ///
    /// # Returns
    ///
    /// Number of files or directories removed.
    ///
    /// # Errors
    ///
    /// Returns an error if cache directory creation, scanning, or deletion fails.
    pub fn cleanup_unreferenced_cache_files(
        &self,
        watch_later: &HashSet<String>,
        feed_video_ids: &HashSet<String>,
    ) -> StorageResult<usize> {
        self.ensure_directories()?;

        let retained_card_video_ids = feed_video_ids
            .union(watch_later)
            .map(String::as_str)
            .collect::<HashSet<_>>();

        let mut removed_count = self.prune_directory_entries(&self.cache_dir, |path| {
            let name = path.file_name().and_then(OsStr::to_str);
            (path.is_file() && matches!(name, Some(VIDEOS_CACHE_FILE | FEED_VIDEO_IDS_CACHE_FILE)))
                || path.is_dir()
                    && matches!(
                        name,
                        Some(
                            THUMBNAILS_DIR | VIDEOS_DIR | VIDEO_SIDECARS_DIR | TRANSCRIPTS_WORK_DIR
                        )
                    )
        })?;

        removed_count += self.prune_directory_entries(&self.thumbnails_dir, |path| {
            path.is_file()
                && video_id_from_thumbnail_path(path)
                    .is_some_and(|video_id| retained_card_video_ids.contains(video_id))
        })?;
        removed_count += self.prune_directory_entries(&self.video_sidecars_dir, |path| {
            path.is_file()
                && video_id_from_sidecar_path(path)
                    .is_some_and(|video_id| retained_card_video_ids.contains(video_id))
        })?;
        removed_count += self.prune_directory_entries(&self.videos_dir, |path| {
            path.is_file()
                && (cached_video_id_for_path(path)
                    .or_else(|| video_id_from_subtitle_path(path))
                    .or_else(|| video_id_from_info_json_path(path)))
                .is_some_and(|video_id| watch_later.contains(video_id))
        })?;
        removed_count += self.prune_directory_entries(&self.transcripts_work_dir, |_| false)?;

        self.ensure_directories()?;
        Ok(removed_count)
    }

    /// Returns the transcript extraction work directory path.
    pub fn transcripts_work_dir(&self) -> &Path {
        self.transcripts_work_dir.as_path()
    }

    /// Builds the cache path for a video's thumbnail image.
    ///
    /// # Arguments
    ///
    /// * `video_id` - `YouTube` video identifier.
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
    /// * `video_id` - `YouTube` video identifier.
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
    /// * `video_id` - `YouTube` video identifier.
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

    /// Scans the videos directory and returns all `(path, video_id)` pairs for cached video files.
    fn scan_video_files(&self) -> StorageResult<Vec<(PathBuf, String)>> {
        Ok(std::fs::read_dir(&self.videos_dir)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let path = e.path();
                let id = cached_video_id_for_path(&path)?.to_string();
                Some((path, id))
            })
            .collect())
    }

    /// Deletes all cached video files for a single video ID.
    ///
    /// This removes every matching file in the videos cache directory across supported
    /// extensions and titles.
    ///
    /// # Arguments
    ///
    /// * `video_id` - `YouTube` video identifier to remove from local video cache.
    ///
    /// # Returns
    ///
    /// Number of files removed.
    ///
    /// # Errors
    ///
    /// Returns an error if directory scanning or file deletion fails.
    pub fn remove_cached_video_files(&self, video_id: &str) -> StorageResult<usize> {
        let mut removed_count = 0usize;
        for (path, id) in self.scan_video_files()? {
            if id == video_id {
                std::fs::remove_file(path)?;
                removed_count += 1;
            }
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
        video.set_watched(sidecar.watched);
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
        if let Err(error) = self.ensure_directories() {
            warn!("Failed to recreate storage directories before loading watch-later: {error}");
        }
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
        self.ensure_directories()?;
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
        if let Err(error) = self.ensure_directories() {
            warn!("Failed to recreate storage directories before loading videos: {error}");
        }
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
        self.ensure_directories()?;
        let path = self.cache_dir.join(VIDEOS_CACHE_FILE);
        let json = serde_json::to_string_pretty(videos)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Loads cached IDs for the current feed.
    ///
    /// # Returns
    ///
    /// Saved feed IDs, or `None` if no feed-ID cache exists or the cache cannot be read.
    pub fn load_feed_video_ids(&self) -> Option<Vec<String>> {
        if let Err(error) = self.ensure_directories() {
            warn!("Failed to recreate storage directories before loading feed IDs: {error}");
        }
        let path = self.cache_dir.join(FEED_VIDEO_IDS_CACHE_FILE);
        try_load_json_file::<VideoIdsData>(&path, "feed video IDs cache").map(|data| data.video_ids)
    }

    /// Persists IDs that belong to the current feed.
    ///
    /// # Arguments
    ///
    /// * `video_ids` - Current feed video IDs, in display order.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or file writes fail.
    pub fn save_feed_video_ids(&self, video_ids: &[String]) -> StorageResult<()> {
        self.ensure_directories()?;
        let path = self.cache_dir.join(FEED_VIDEO_IDS_CACHE_FILE);
        let data = VideoIdsData {
            video_ids: video_ids.to_vec(),
        };
        let json = serde_json::to_string_pretty(&data)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Rebuilds missing Watch Later video records from cached `yt-dlp` info sidecars.
    ///
    /// # Arguments
    ///
    /// * `watch_later` - Saved Watch Later IDs to repair.
    /// * `known_video_ids` - Video IDs already present in `videos.json`.
    ///
    /// # Returns
    ///
    /// Videos reconstructed from local `.info.json` files.
    pub fn load_missing_watch_later_videos_from_info_json(
        &self,
        watch_later: &HashSet<String>,
        known_video_ids: &HashSet<String>,
    ) -> Vec<Video> {
        let entries = match std::fs::read_dir(&self.videos_dir) {
            Ok(entries) => entries,
            Err(error) => {
                warn!(
                    "Failed to scan watch-later info JSON directory {}: {}",
                    self.videos_dir.display(),
                    error
                );
                return Vec::new();
            }
        };

        let mut repaired_videos = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let video_id = video_id_from_info_json_path(&path)?;
                if known_video_ids.contains(video_id) || !watch_later.contains(video_id) {
                    return None;
                }
                let info = try_load_json_file::<YtDlpInfoJson>(&path, "yt-dlp info JSON")?;
                let video = info.into_video(video_id)?;
                Some(video)
            })
            .collect::<Vec<_>>();
        repaired_videos.sort_by_key(|video| std::cmp::Reverse(video.published()));
        repaired_videos
    }

    /// Persists transcript and/or AI summary in a per-video sidecar file.
    ///
    /// # Arguments
    ///
    /// * `video_id` - `YouTube` video identifier.
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
        self.ensure_directories()?;
        let mut sidecar = self.read_video_sidecar(video_id).unwrap_or_default();
        if let Some(transcript) = transcript {
            sidecar.transcript = Some(transcript.to_string());
        }
        if let Some(ai_summary) = ai_summary {
            sidecar.ai_summary = Some(ai_summary.to_string());
        }
        self.write_video_sidecar(video_id, &sidecar)
    }

    /// Persists the watched state for a video in its sidecar file.
    ///
    /// # Arguments
    ///
    /// * `video_id` - `YouTube` video identifier.
    /// * `watched` - Whether the video has been watched.
    ///
    /// # Errors
    ///
    /// Returns an error when sidecar loading, serialization, or file writes fail.
    pub fn save_video_watched(&self, video_id: &str, watched: bool) -> StorageResult<()> {
        self.ensure_directories()?;
        let mut sidecar = self.read_video_sidecar(video_id).unwrap_or_default();
        sidecar.watched = watched;
        self.write_video_sidecar(video_id, &sidecar)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_TITLE_LENGTH, Storage, VIDEOS_CACHE_FILE, VideoMetadataSidecar,
        collect_cached_video_ids_from_dir, sanitize_filename, video_id_from_stem,
    };
    use crate::data::{NewVideo, Video};
    use chrono::{DateTime, Utc};
    use std::collections::HashSet;
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
        Video::new(NewVideo {
            video_id: video_id.to_string(),
            channel_id: "UC123".to_string(),
            channel_name: "Channel".to_string(),
            title: format!("Title {video_id}"),
            published: published.with_timezone(&Utc),
            thumbnail_url: "https://example.com/thumb.jpg".to_string(),
            duration_seconds: Some(120),
        })
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
    fn cleanup_unreferenced_cache_files_keeps_only_state_referenced_cache_artifacts() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);

        let valid_video_id = "C9ww_8cg_5g";
        let stale_video_id = "abc123DEF45";
        let removed_paths = [
            storage.cache_dir.join("stray.tmp"),
            storage.thumbnails_dir.join("short.jpg"),
            storage.thumbnails_dir.join(format!("{valid_video_id}.png")),
            storage.thumbnails_dir.join(format!("{stale_video_id}.jpg")),
            storage.videos_dir.join("notes.txt"),
            storage.videos_dir.join("bad_video_name.mkv"),
            storage
                .videos_dir
                .join(format!("stale_{stale_video_id}.mkv")),
            storage
                .videos_dir
                .join(format!("stale_{stale_video_id}.en.vtt")),
            storage
                .videos_dir
                .join(format!("stale_{stale_video_id}.info.json")),
            storage.video_sidecars_dir.join("bad.json"),
            storage
                .video_sidecars_dir
                .join(format!("{stale_video_id}.json")),
            storage
                .transcripts_work_dir
                .join(format!("{valid_video_id}.en.json3")),
        ];
        let kept_paths = [
            storage.cache_dir.join(VIDEOS_CACHE_FILE),
            storage.thumbnails_dir.join(format!("{valid_video_id}.jpg")),
            storage
                .videos_dir
                .join(format!("title_{valid_video_id}.mkv")),
            storage
                .videos_dir
                .join(format!("title_{valid_video_id}.en.vtt")),
            storage
                .videos_dir
                .join(format!("title_{valid_video_id}.info.json")),
            storage
                .video_sidecars_dir
                .join(format!("{valid_video_id}.json")),
        ];

        for path in removed_paths.iter().chain(kept_paths.iter()) {
            touch(path);
        }
        std::fs::create_dir_all(storage.cache_dir.join("unknown_dir"))
            .expect("unknown cache dir should be creatable");

        let watch_later = HashSet::from([valid_video_id.to_string()]);
        let feed_video_ids = HashSet::from([valid_video_id.to_string()]);
        let removed_count = storage
            .cleanup_unreferenced_cache_files(&watch_later, &feed_video_ids)
            .expect("cleanup should succeed");

        assert_eq!(removed_count, removed_paths.len() + 1);
        for path in removed_paths {
            assert!(!path.exists(), "expected {} to be removed", path.display());
        }
        for path in kept_paths {
            assert!(path.exists(), "expected {} to be kept", path.display());
        }
        assert!(!storage.cache_dir.join("unknown_dir").exists());
        assert!(storage.transcripts_work_dir.exists());

        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }

    #[test]
    fn save_videos_recreates_missing_cache_directories() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);
        std::fs::remove_dir_all(&storage.cache_dir).expect("cache dir should be removable");

        storage
            .save_videos(&[sample_video("C9ww_8cg_5g")])
            .expect("saving videos should recreate cache directories");

        assert!(storage.cache_dir.join(VIDEOS_CACHE_FILE).exists());
        assert!(storage.thumbnails_dir.exists());
        assert!(storage.videos_dir.exists());
        assert!(storage.video_sidecars_dir.exists());
        assert!(storage.transcripts_work_dir.exists());

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
    fn feed_video_ids_round_trip_in_display_order() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);
        let ids = vec!["b".to_string(), "a".to_string(), "c".to_string()];

        storage
            .save_feed_video_ids(&ids)
            .expect("feed IDs should save");

        assert_eq!(storage.load_feed_video_ids(), Some(ids));
        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }

    #[test]
    fn missing_watch_later_videos_are_rebuilt_from_info_json() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);
        let video_id = "C9ww_8cg_5g";
        std::fs::write(
            storage
                .videos_dir
                .join(format!("Local Title_{video_id}.info.json")),
            r#"{
                "id": "C9ww_8cg_5g",
                "title": "Local Title",
                "channel_id": "UC123",
                "channel": "Local Channel",
                "upload_date": "20240501",
                "thumbnail": "https://example.com/local.jpg",
                "duration": 91.4
            }"#,
        )
        .expect("info JSON should be writable");

        let repaired = storage.load_missing_watch_later_videos_from_info_json(
            &HashSet::from([video_id.to_string()]),
            &HashSet::new(),
        );

        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].video_id(), video_id);
        assert_eq!(repaired[0].title(), "Local Title");
        assert_eq!(repaired[0].channel_id(), "UC123");
        assert_eq!(repaired[0].channel_name(), "Local Channel");
        assert_eq!(repaired[0].duration_seconds(), Some(91));
        std::fs::remove_dir_all(temp_dir).expect("Failed to cleanup temp directory");
    }

    #[test]
    fn missing_watch_later_repair_skips_known_or_unlisted_ids() {
        let temp_dir = create_unique_temp_dir();
        let storage = create_test_storage(&temp_dir);
        let video_id = "C9ww_8cg_5g";
        std::fs::write(
            storage
                .videos_dir
                .join(format!("Known_{video_id}.info.json")),
            r#"{"id":"C9ww_8cg_5g","title":"Known","upload_date":"20240501"}"#,
        )
        .expect("info JSON should be writable");

        assert!(
            storage
                .load_missing_watch_later_videos_from_info_json(
                    &HashSet::from([video_id.to_string()]),
                    &HashSet::from([video_id.to_string()]),
                )
                .is_empty()
        );
        assert!(
            storage
                .load_missing_watch_later_videos_from_info_json(&HashSet::new(), &HashSet::new())
                .is_empty()
        );
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
                    watched: false,
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
                    watched: false,
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
