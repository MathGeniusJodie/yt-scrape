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
}

impl Video {
    /// Returns the YouTube watch URL
    #[allow(dead_code)]
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.video_id)
    }

    /// Returns the local thumbnail cache path
    #[allow(dead_code)]
    pub fn thumbnail_path(&self, cache_dir: &Path) -> PathBuf {
        cache_dir.join("thumbnails").join(format!("{}.jpg", self.video_id))
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
    pub scroll_offset: usize,
    pub selected_index: Option<usize>,
    pub is_refreshing: bool,
    pub last_refresh: Option<DateTime<Utc>>,
    pub terminal_cols: u16,
    pub terminal_rows: u16,
    pub should_quit: bool,
    pub show_help: bool,
    pub status_message: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            videos: Vec::new(),
            watch_later: HashSet::new(),
            current_tab: Tab::Feed,
            scroll_offset: 0,
            selected_index: None,
            is_refreshing: false,
            last_refresh: None,
            terminal_cols: 80,
            terminal_rows: 24,
            should_quit: false,
            show_help: false,
            status_message: None,
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
