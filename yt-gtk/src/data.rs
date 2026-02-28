use crate::urls;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    // Transcript content is persisted in per-video sidecar files, not in `videos.json`.
    #[serde(default, skip_serializing)]
    pub transcript: Option<String>,
    // AI summaries are persisted in per-video sidecar files, not in `videos.json`.
    #[serde(default, skip_serializing)]
    pub ai_summary: Option<String>,
}

impl Video {
    /// Returns the YouTube watch URL
    pub fn watch_url(&self) -> String {
        urls::watch_url(&self.video_id)
    }

    /// Returns `true` when a non-empty AI summary is cached for this video.
    pub fn has_ai_summary(&self) -> bool {
        self.ai_summary
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
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
