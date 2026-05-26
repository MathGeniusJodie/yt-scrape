use crate::urls;
use chrono::{DateTime, Utc};
use quick_xml::escape::unescape;
use serde::{Deserialize, Serialize};

fn decode_html_entities(input: &str) -> String {
    unescape(input)
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| input.to_string())
}

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
    // Watched state is persisted in per-video sidecar files, not in `videos.json`.
    #[serde(default, skip_serializing)]
    watched: bool,
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
            title: decode_html_entities(&title),
            published,
            thumbnail_url,
            duration_seconds,
            transcript: None,
            ai_summary: None,
            watched: false,
        }
    }

    /// Returns the YouTube video ID.
    pub fn video_id(&self) -> &str {
        &self.video_id
    }

    /// Returns the YouTube channel ID.
    pub fn channel_id(&self) -> &str {
        &self.channel_id
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
        self.ai_summary
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty())
    }

    /// Returns `true` when the video has been marked as watched.
    pub fn is_watched(&self) -> bool {
        self.watched
    }

    /// Sets the watched state.
    pub fn set_watched(&mut self, watched: bool) {
        self.watched = watched;
    }
}

/// Which tab is currently active
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Main feed showing all fetched videos.
    #[default]
    Feed,
    /// Videos returned by the most recent YouTube search.
    Search,
    /// User-curated watch-later list.
    WatchLater,
}

/// Persisted watch-later state
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WatchLaterData {
    pub video_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::Video;
    use chrono::{TimeZone, Utc};

    #[test]
    fn video_new_decodes_html_entities_in_titles() {
        let video = Video::new(
            "video-id".to_string(),
            "channel-id".to_string(),
            "channel-name".to_string(),
            "Tom &amp; Jerry &quot;Best Of&quot; &#x27;24".to_string(),
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
                .single()
                .expect("valid fixed test timestamp"),
            "https://example.com/thumb.jpg".to_string(),
            None,
        );

        assert_eq!(video.title(), "Tom & Jerry \"Best Of\" '24");
    }
}
