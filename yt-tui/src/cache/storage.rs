use crate::data::{Video, WatchLaterData};
use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;

/// Handles persistence of app data
pub struct Storage {
    data_dir: PathBuf,
    cache_dir: PathBuf,
    videos_dir: PathBuf,
}

impl Storage {
    pub fn new() -> Result<Self> {
        let project_dirs = directories::ProjectDirs::from("", "", "yt-tui")
            .ok_or_else(|| anyhow::anyhow!("Could not determine project directories"))?;

        let data_dir = project_dirs.data_dir().to_path_buf();
        let cache_dir = project_dirs.cache_dir().to_path_buf();
        let videos_dir = cache_dir.join("videos");

        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::create_dir_all(&videos_dir)?;

        Ok(Self {
            data_dir,
            cache_dir,
            videos_dir,
        })
    }

    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    pub fn videos_dir(&self) -> &PathBuf {
        &self.videos_dir
    }

    /// Get the path where a video would be stored
    pub fn video_path(&self, video_id: &str) -> PathBuf {
        self.videos_dir.join(format!("{}.mp4", video_id))
    }

    /// Check if a video is downloaded
    pub fn has_video(&self, video_id: &str) -> bool {
        self.video_path(video_id).exists()
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
}
