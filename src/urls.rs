//! `YouTube` URL constants - single source of truth for all `YouTube` URLs

/// Format a `YouTube` watch URL from a video ID
pub fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

/// Format a `YouTube` thumbnail URL (hqdefault quality) from a video ID
pub fn thumbnail_url(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")
}
