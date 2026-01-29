use crate::data::{Video, WatchLaterData};
use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;

const MAX_TITLE_LENGTH: usize = 100;

/// Sanitize a string for use in a filename
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_TITLE_LENGTH)
        .collect()
}

/// Handles persistence of app data
pub struct Storage {
    data_dir: PathBuf,
    cache_dir: PathBuf,
    videos_dir: PathBuf,
    transcripts_work_dir: PathBuf,
}

impl Storage {
    pub fn new() -> Result<Self> {
        let project_dirs = directories::ProjectDirs::from("", "", "yt-tui")
            .ok_or_else(|| anyhow::anyhow!("Could not determine project directories"))?;

        let data_dir = project_dirs.data_dir().to_path_buf();
        let cache_dir = project_dirs.cache_dir().to_path_buf();
        let videos_dir = cache_dir.join("videos");
        let transcripts_work_dir = cache_dir.join("transcripts_work");

        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&videos_dir)?;
        std::fs::create_dir_all(&transcripts_work_dir)?;

        Ok(Self {
            data_dir,
            cache_dir,
            videos_dir,
            transcripts_work_dir,
        })
    }

    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    pub fn videos_dir(&self) -> &PathBuf {
        &self.videos_dir
    }

    pub fn transcripts_work_dir(&self) -> &PathBuf {
        &self.transcripts_work_dir
    }

    /// Get the path where a video would be stored (with sanitized title)
    pub fn video_path(&self, video_id: &str, title: &str) -> PathBuf {
        let sanitized_title = sanitize_filename(title);
        self.videos_dir
            .join(format!("{}_{}.mp4", sanitized_title, video_id))
    }

    /// Find an existing video file by video_id (regardless of title in filename)
    pub fn find_video_path(&self, video_id: &str) -> Option<PathBuf> {
        let pattern = format!("*_{}.mp4", video_id);
        glob::glob(self.videos_dir.join(&pattern).to_str()?)
            .ok()?
            .filter_map(Result::ok)
            .next()
    }

    /// Check if a video is downloaded
    pub fn has_video(&self, video_id: &str) -> bool {
        self.find_video_path(video_id).is_some()
    }

    /// Load watch later video IDs
    pub fn load_watch_later(&self) -> HashSet<String> {
        let path = self.data_dir.join("watch_later.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<WatchLaterData>(&s).ok())
            .map(|d| d.video_ids.into_iter().collect())
            .unwrap_or_default()
    }

    /// Save watch later video IDs
    pub fn save_watch_later(&self, video_ids: &HashSet<String>) -> Result<()> {
        let path = self.data_dir.join("watch_later.json");
        let data = WatchLaterData {
            video_ids: video_ids.iter().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&data)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Load cached videos from disk
    pub fn load_videos(&self) -> Vec<Video> {
        let path = self.cache_dir.join("videos.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save videos to cache
    pub fn save_videos(&self, videos: &[Video]) -> Result<()> {
        let path = self.cache_dir.join("videos.json");
        let json = serde_json::to_string_pretty(videos)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Update a video's transcript and save to disk
    #[allow(dead_code)]
    pub fn save_transcript(
        &self,
        videos: &mut [Video],
        video_id: &str,
        transcript: String,
    ) -> Result<()> {
        if let Some(video) = videos.iter_mut().find(|v| v.video_id == video_id) {
            video.transcript = Some(transcript);
            self.save_videos(videos)?;
        }
        Ok(())
    }

    /// Check if a video has a transcript
    #[allow(dead_code)]
    pub fn has_transcript(&self, videos: &[Video], video_id: &str) -> bool {
        videos
            .iter()
            .find(|v| v.video_id == video_id)
            .map(|v| v.transcript.is_some())
            .unwrap_or(false)
    }
}
