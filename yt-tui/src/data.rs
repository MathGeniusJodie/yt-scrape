use crate::gemini::SummaryState;
use crate::urls;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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
    pub transcript: Option<String>,
}

impl Video {
    /// Returns the YouTube watch URL
    #[allow(dead_code)]
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

/// Application state
#[derive(Debug)]
pub struct AppState {
    pub videos: Vec<Video>,
    pub watch_later: HashSet<String>,
    pub current_tab: Tab,
    pub selected_index: Option<usize>,
    pub is_refreshing: bool,
    pub last_refresh: Option<DateTime<Utc>>,
    pub terminal_cols: u16,
    pub terminal_rows: u16,
    pub should_quit: bool,
    pub show_help: bool,
    pub status_message: Option<String>,
    pub show_summary: bool,
    pub summary_state: Option<SummaryState>,
    pub summary_scroll: u16,
    pub summary_video_title: Option<String>,
    // Transcript modal
    pub show_transcript: bool,
    pub transcript_content: Option<String>,
    pub transcript_scroll: u16,
    pub transcript_video_title: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            videos: Vec::new(),
            watch_later: HashSet::new(),
            current_tab: Tab::Feed,
            selected_index: None,
            is_refreshing: false,
            last_refresh: None,
            terminal_cols: 80,
            terminal_rows: 24,
            should_quit: false,
            show_help: false,
            status_message: None,
            show_summary: false,
            summary_state: None,
            summary_scroll: 0,
            summary_video_title: None,
            show_transcript: false,
            transcript_content: None,
            transcript_scroll: 0,
            transcript_video_title: None,
        }
    }
}

impl AppState {
    /// Get videos for the current tab
    pub fn current_videos(&self) -> Vec<&Video> {
        match self.current_tab {
            Tab::Feed => self.videos.iter().collect(),
            Tab::WatchLater => self
                .videos
                .iter()
                .filter(|v| self.watch_later.contains(&v.video_id))
                .collect(),
        }
    }
}

/// Persisted watch-later state
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WatchLaterData {
    pub video_ids: Vec<String>,
}
