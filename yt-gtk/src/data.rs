use crate::urls;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single video entry from a YouTube RSS feed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub video_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub title: String,
    pub published: DateTime<Utc>,
    pub thumbnail_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

impl Video {
    /// Returns the YouTube watch URL
    pub fn watch_url(&self) -> String {
        urls::watch_url(&self.video_id)
    }

    /// Returns the YouTube thumbnail URL
    #[allow(dead_code)]
    pub fn thumbnail_url(&self) -> String {
        urls::thumbnail_url(&self.video_id)
    }

    /// Returns the local thumbnail cache path
    #[allow(dead_code)]
    pub fn thumbnail_path(&self, cache_dir: &Path) -> PathBuf {
        cache_dir
            .join("thumbnails")
            .join(format!("{}.jpg", self.video_id))
    }
}

/// Which tab is currently active
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Feed,
    WatchLater,
}

/// Persisted watch-later state
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WatchLaterData {
    pub video_ids: Vec<String>,
}
