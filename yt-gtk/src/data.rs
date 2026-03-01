use crate::urls;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single video entry from a YouTube RSS feed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    video_id: String,
    channel_id: String,
    channel_name: String,
    title: String,
    published: DateTime<Utc>,
    thumbnail_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<u32>,
    // Transcript content is persisted in per-video sidecar files, not in `videos.json`.
    #[serde(default, skip_serializing)]
    transcript: Option<String>,
    // AI summaries are persisted in per-video sidecar files, not in `videos.json`.
    #[serde(default, skip_serializing)]
    ai_summary: Option<String>,
}

impl Video {
    /// Constructs a new [`Video`] without transcript/summary sidecar fields.
    pub fn new(
        video_id: String,
        channel_id: String,
        channel_name: String,
        title: String,
        published: DateTime<Utc>,
        thumbnail_url: String,
        duration_seconds: Option<u32>,
    ) -> Self {
        Self {
            video_id,
            channel_id,
            channel_name,
            title,
            published,
            thumbnail_url,
            duration_seconds,
            transcript: None,
            ai_summary: None,
        }
    }

    /// Returns the YouTube video ID.
    pub fn video_id(&self) -> &str {
        &self.video_id
    }

    /// Returns the channel display name.
    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    /// Returns the video title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the publication timestamp in UTC.
    pub fn published(&self) -> DateTime<Utc> {
        self.published
    }

    /// Returns the thumbnail URL.
    pub fn thumbnail_url(&self) -> &str {
        &self.thumbnail_url
    }

    /// Returns video duration in seconds when known.
    pub fn duration_seconds(&self) -> Option<u32> {
        self.duration_seconds
    }

    /// Returns cached transcript text when present.
    pub fn transcript(&self) -> Option<&str> {
        self.transcript.as_deref()
    }

    /// Returns cached AI summary when present.
    pub fn ai_summary(&self) -> Option<&str> {
        self.ai_summary.as_deref()
    }

    /// Replaces cached transcript content for this video.
    pub fn set_transcript(&mut self, transcript: Option<String>) {
        self.transcript = transcript;
    }

    /// Replaces cached AI summary content for this video.
    pub fn set_ai_summary(&mut self, ai_summary: Option<String>) {
        self.ai_summary = ai_summary;
    }

    /// Returns the YouTube watch URL
    pub fn watch_url(&self) -> String {
        urls::watch_url(&self.video_id)
    }

    /// Returns `true` when a non-empty AI summary is cached for this video.
    pub fn has_ai_summary(&self) -> bool {
        self.ai_summary.as_ref().is_some_and(|s| !s.trim().is_empty())
    }
}

/// Which tab is currently active
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Main feed showing all fetched videos.
    #[default]
    Feed,
    /// User-curated watch-later list.
    WatchLater,
}

/// Persisted watch-later state
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WatchLaterData {
    pub video_ids: Vec<String>,
}
