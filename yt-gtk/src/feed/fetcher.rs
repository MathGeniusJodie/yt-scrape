use crate::data::Video;
use crate::urls;
use anyhow::Context;
use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration, Instant};

const YOUTUBE_PLAYLIST_ITEMS_API_URL: &str = "https://www.googleapis.com/youtube/v3/playlistItems";
const YOUTUBE_CHANNELS_API_URL: &str = "https://www.googleapis.com/youtube/v3/channels";
const YOUTUBE_VIDEOS_API_URL: &str = "https://www.googleapis.com/youtube/v3/videos";
const PLAYLIST_ITEMS_MAX_RESULTS: &str = "25";
const MAX_FETCH_ATTEMPTS: usize = 3;
const MAX_CONCURRENT_CHANNEL_FETCHES: usize = 8;
const INITIAL_BACKOFF_MS: u64 = 1_000;
const MAX_BACKOFF_MS: u64 = 4_000;
const MAX_BACKOFF_JITTER_MS: u64 = 100;
const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36";

/// Progress updates during feed fetching
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FetchProgress {
    Started {
        total: usize,
    },
    ChannelComplete {
        channel: String,
        count: usize,
    },
    RetryScheduled {
        channel_id: String,
        next_attempt: usize,
        max_attempts: usize,
        delay_secs: u64,
        reason: String,
    },
    Error {
        channel_id: String,
        error: String,
    },
    Fatal {
        error: String,
    },
    AllComplete {
        total_videos: usize,
        successful_channels: usize,
        failed_channels: usize,
    },
}

#[derive(Debug, Deserialize)]
struct PlaylistItemsResponse {
    #[serde(default)]
    items: Vec<PlaylistItem>,
}

#[derive(Debug, Deserialize)]
struct ChannelsResponse {
    #[serde(default)]
    items: Vec<ChannelItem>,
}

#[derive(Debug, Deserialize)]
struct VideosResponse {
    #[serde(default)]
    items: Vec<YoutubeVideoItem>,
}

#[derive(Debug, Deserialize)]
struct YoutubeVideoItem {
    id: Option<String>,
    #[serde(rename = "contentDetails")]
    content_details: Option<YoutubeVideoContentDetails>,
}

#[derive(Debug, Deserialize)]
struct YoutubeVideoContentDetails {
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChannelItem {
    #[serde(rename = "contentDetails")]
    content_details: Option<ChannelContentDetails>,
}

#[derive(Debug, Deserialize)]
struct ChannelContentDetails {
    #[serde(rename = "relatedPlaylists")]
    related_playlists: Option<RelatedPlaylists>,
}

#[derive(Debug, Deserialize)]
struct RelatedPlaylists {
    uploads: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistItem {
    snippet: Option<PlaylistItemSnippet>,
}

#[derive(Debug, Deserialize)]
struct PlaylistItemSnippet {
    #[serde(rename = "publishedAt")]
    published_at: Option<DateTime<Utc>>,
    title: Option<String>,
    #[serde(rename = "channelId")]
    channel_id: Option<String>,
    #[serde(rename = "channelTitle")]
    channel_title: Option<String>,
    #[serde(rename = "resourceId")]
    resource_id: Option<PlaylistResourceId>,
    thumbnails: Option<PlaylistThumbnails>,
}

#[derive(Debug, Deserialize)]
struct PlaylistResourceId {
    #[serde(rename = "videoId")]
    video_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistThumbnails {
    maxres: Option<Thumbnail>,
    standard: Option<Thumbnail>,
    high: Option<Thumbnail>,
    medium: Option<Thumbnail>,
    #[serde(rename = "default")]
    default_thumbnail: Option<Thumbnail>,
}

impl PlaylistThumbnails {
    fn best_url(&self) -> Option<String> {
        self.maxres
            .as_ref()
            .or(self.standard.as_ref())
            .or(self.high.as_ref())
            .or(self.medium.as_ref())
            .or(self.default_thumbnail.as_ref())
            .map(|t| t.url.clone())
    }
}

#[derive(Debug, Deserialize)]
struct Thumbnail {
    url: String,
}

#[derive(Debug)]
struct PendingChannel {
    channel_id: String,
    uploads_playlist_id: String,
    attempt: usize,
    not_before: Instant,
}

enum ChannelFetchResult {
    Success { videos: Vec<Video> },
    Failed,
}

/// Fetch all feeds from the given channel IDs
pub async fn fetch_all_feeds(
    channel_ids: Vec<String>,
    tx: mpsc::Sender<FetchProgress>,
) -> anyhow::Result<Vec<Video>> {
    let total = channel_ids.len();
    let _ = tx.send(FetchProgress::Started { total }).await;

    let api_key = std::env::var("GOOGLE_API_KEY")
        .context("GOOGLE_API_KEY is not set. Set it before refreshing feeds.")?;

    let client = reqwest::Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .gzip(true)
        .build()?;

    let mut all_videos = Vec::new();
    let mut successful_channels = 0usize;
    let mut failed_channels = 0usize;
    let mut pending_channels = Vec::new();

    for channel_id in channel_ids {
        match uploads_playlist_id_for_channel(&channel_id) {
            Some(uploads_playlist_id) => pending_channels.push(PendingChannel {
                channel_id,
                uploads_playlist_id,
                attempt: 1,
                not_before: Instant::now(),
            }),
            None => {
                failed_channels += 1;
                let _ = tx
                    .send(FetchProgress::Error {
                        channel_id,
                        error: "Invalid channel ID. Expected an ID starting with 'UC'.".to_string(),
                    })
                    .await;
            }
        }
    }

    let mut fetches = stream::iter(pending_channels.into_iter().map(|pending| {
        let client = client.clone();
        let api_key = api_key.clone();
        let tx = tx.clone();

        async move { fetch_channel_with_retries(client, api_key, pending, tx).await }
    }))
    .buffer_unordered(MAX_CONCURRENT_CHANNEL_FETCHES);

    while let Some(result) = fetches.next().await {
        match result {
            ChannelFetchResult::Success { videos } => {
                successful_channels += 1;
                all_videos.extend(videos);
            }
            ChannelFetchResult::Failed => {
                failed_channels += 1;
            }
        }
    }

    // Sort by published date (newest first)
    all_videos.sort_by(|a, b| b.published.cmp(&a.published));

    // Keep only the most recent 400
    all_videos.truncate(400);

    let total_videos = all_videos.len();
    let _ = tx
        .send(FetchProgress::AllComplete {
            total_videos,
            successful_channels,
            failed_channels,
        })
        .await;

    Ok(all_videos)
}

async fn fetch_channel_with_retries(
    client: reqwest::Client,
    api_key: String,
    mut pending: PendingChannel,
    tx: mpsc::Sender<FetchProgress>,
) -> ChannelFetchResult {
    loop {
        let now = Instant::now();
        if pending.not_before > now {
            sleep(pending.not_before - now).await;
        }

        let fetch_result = fetch_channel_once(
            &client,
            &api_key,
            &pending.uploads_playlist_id,
            &pending.channel_id,
        )
        .await;

        match fetch_result {
            Ok(videos) => {
                let _ = tx
                    .send(FetchProgress::ChannelComplete {
                        channel: pending.channel_id.clone(),
                        count: videos.len(),
                    })
                    .await;
                return ChannelFetchResult::Success { videos };
            }
            Err(error) => {
                if error.status() == Some(reqwest::StatusCode::NOT_FOUND) {
                    match resolve_uploads_playlist_id(&client, &api_key, &pending.channel_id).await
                    {
                        Ok(Some(resolved_uploads_playlist_id))
                            if resolved_uploads_playlist_id != pending.uploads_playlist_id =>
                        {
                            let _ = tx
                                .send(FetchProgress::RetryScheduled {
                                    channel_id: pending.channel_id.clone(),
                                    next_attempt: pending.attempt,
                                    max_attempts: MAX_FETCH_ATTEMPTS,
                                    delay_secs: 0,
                                    reason:
                                        "Resolved uploads playlist ID after 404; queued for retry."
                                            .to_string(),
                                })
                                .await;

                            pending.uploads_playlist_id = resolved_uploads_playlist_id;
                            pending.not_before = Instant::now();
                            continue;
                        }
                        Ok(_) => {}
                        Err(lookup_error) => {
                            eprintln!(
                                "Failed resolving uploads playlist for {} after 404: {}",
                                pending.channel_id, lookup_error
                            );
                        }
                    }
                }

                if should_retry(&error) && pending.attempt < MAX_FETCH_ATTEMPTS {
                    let delay_ms =
                        backoff_ms_for_attempt(pending.attempt, &pending.channel_id, &error);
                    let _ = tx
                        .send(FetchProgress::RetryScheduled {
                            channel_id: pending.channel_id.clone(),
                            next_attempt: pending.attempt + 1,
                            max_attempts: MAX_FETCH_ATTEMPTS,
                            delay_secs: delay_ms.div_ceil(1000),
                            reason: error.to_string(),
                        })
                        .await;

                    pending.attempt += 1;
                    pending.not_before = Instant::now() + Duration::from_millis(delay_ms);
                    continue;
                }

                let _ = tx
                    .send(FetchProgress::Error {
                        channel_id: pending.channel_id,
                        error: error.to_string(),
                    })
                    .await;
                return ChannelFetchResult::Failed;
            }
        }
    }
}

async fn fetch_channel_once(
    client: &reqwest::Client,
    api_key: &str,
    uploads_playlist_id: &str,
    channel_id: &str,
) -> Result<Vec<Video>, reqwest::Error> {
    let response = client
        .get(YOUTUBE_PLAYLIST_ITEMS_API_URL)
        .query(&[
            ("part", "snippet"),
            ("playlistId", uploads_playlist_id),
            ("maxResults", PLAYLIST_ITEMS_MAX_RESULTS),
            ("key", api_key),
        ])
        .send()
        .await?;

    let response = response.error_for_status()?;
    let payload = response.json::<PlaylistItemsResponse>().await?;
    let video_ids = video_ids_from_playlist_items(&payload);
    let durations_by_video_id = match fetch_video_durations(client, api_key, &video_ids).await {
        Ok(durations) => durations,
        Err(error) => {
            eprintln!(
                "Failed fetching durations for channel {}: {}",
                channel_id, error
            );
            HashMap::new()
        }
    };

    Ok(videos_from_playlist_items(
        payload,
        channel_id,
        &durations_by_video_id,
    ))
}

async fn resolve_uploads_playlist_id(
    client: &reqwest::Client,
    api_key: &str,
    channel_id: &str,
) -> Result<Option<String>, reqwest::Error> {
    let response = client
        .get(YOUTUBE_CHANNELS_API_URL)
        .query(&[
            ("part", "contentDetails"),
            ("id", channel_id),
            ("key", api_key),
        ])
        .send()
        .await?;

    let response = response.error_for_status()?;
    let payload = response.json::<ChannelsResponse>().await?;

    Ok(payload
        .items
        .into_iter()
        .find_map(|item| item.content_details?.related_playlists?.uploads))
}

fn videos_from_playlist_items(
    response: PlaylistItemsResponse,
    fallback_channel_id: &str,
    durations_by_video_id: &HashMap<String, u32>,
) -> Vec<Video> {
    response
        .items
        .into_iter()
        .filter_map(|item| {
            let snippet = item.snippet?;
            let published = snippet.published_at?;
            let video_id = snippet.resource_id?.video_id?;

            let title = snippet.title.unwrap_or_else(|| "Untitled".to_string());
            let channel_id = snippet
                .channel_id
                .unwrap_or_else(|| fallback_channel_id.to_string());
            let channel_name = snippet
                .channel_title
                .unwrap_or_else(|| "Unknown channel".to_string());
            let thumbnail_url = snippet
                .thumbnails
                .as_ref()
                .and_then(PlaylistThumbnails::best_url)
                .unwrap_or_else(|| urls::thumbnail_url(&video_id));
            let duration_seconds = durations_by_video_id.get(&video_id).copied();

            Some(Video {
                video_id,
                channel_id,
                channel_name,
                title,
                published,
                thumbnail_url,
                duration_seconds,
                transcript: None,
            })
        })
        .collect()
}

fn video_ids_from_playlist_items(response: &PlaylistItemsResponse) -> Vec<String> {
    response
        .items
        .iter()
        .filter_map(|item| {
            item.snippet
                .as_ref()?
                .resource_id
                .as_ref()?
                .video_id
                .clone()
        })
        .collect()
}

async fn fetch_video_durations(
    client: &reqwest::Client,
    api_key: &str,
    video_ids: &[String],
) -> Result<HashMap<String, u32>, reqwest::Error> {
    if video_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let video_ids_csv = video_ids.join(",");
    let response = client
        .get(YOUTUBE_VIDEOS_API_URL)
        .query(&[
            ("part", "contentDetails"),
            ("id", video_ids_csv.as_str()),
            ("key", api_key),
        ])
        .send()
        .await?;

    let response = response.error_for_status()?;
    let payload = response.json::<VideosResponse>().await?;
    Ok(payload
        .items
        .into_iter()
        .filter_map(|item| {
            let video_id = item.id?;
            let iso_duration = item.content_details?.duration?;
            let seconds = parse_iso8601_duration_seconds(&iso_duration)?;
            Some((video_id, seconds))
        })
        .collect())
}

fn parse_iso8601_duration_seconds(input: &str) -> Option<u32> {
    let mut chars = input.chars();
    if chars.next()? != 'P' {
        return None;
    }

    let mut in_time = false;
    let mut saw_component = false;
    let mut total_seconds = 0u64;
    let mut current_number: Option<u64> = None;

    for ch in chars {
        if ch == 'T' {
            in_time = true;
            continue;
        }

        if ch.is_ascii_digit() {
            let digit = ch.to_digit(10)? as u64;
            current_number = Some(
                current_number
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit),
            );
            continue;
        }

        let value = current_number.take()?;
        let unit_seconds = match (ch, in_time) {
            ('W', false) => 7 * 24 * 60 * 60,
            ('D', false) => 24 * 60 * 60,
            ('H', true) => 60 * 60,
            ('M', true) => 60,
            ('S', true) => 1,
            _ => return None,
        };
        total_seconds = total_seconds.saturating_add(value.saturating_mul(unit_seconds));
        saw_component = true;
    }

    if current_number.is_some() || !saw_component {
        return None;
    }

    u32::try_from(total_seconds).ok()
}

fn uploads_playlist_id_for_channel(channel_id: &str) -> Option<String> {
    channel_id
        .strip_prefix("UC")
        .map(|suffix| format!("UU{}", suffix))
}

fn should_retry(error: &reqwest::Error) -> bool {
    if error.is_timeout() || error.is_connect() {
        return true;
    }

    match error.status() {
        Some(status) => {
            status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::FORBIDDEN
                || status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::NOT_FOUND
                || status == reqwest::StatusCode::REQUEST_TIMEOUT
                || status.is_server_error()
        }
        None => false,
    }
}

fn backoff_ms_for_attempt(attempt: usize, channel_id: &str, error: &reqwest::Error) -> u64 {
    let base_ms = match error.status() {
        Some(reqwest::StatusCode::TOO_MANY_REQUESTS) => 30_000,
        Some(reqwest::StatusCode::FORBIDDEN) => 30_000,
        Some(reqwest::StatusCode::UNAUTHORIZED) => 30_000,
        Some(reqwest::StatusCode::NOT_FOUND) => 20_000,
        Some(reqwest::StatusCode::REQUEST_TIMEOUT) => 12_000,
        Some(status) if status.is_server_error() => 20_000,
        _ if error.is_timeout() => 12_000,
        _ if error.is_connect() => 12_000,
        _ => INITIAL_BACKOFF_MS,
    };

    let shift = attempt.saturating_sub(1).min(6) as u32;
    let exponential = base_ms.saturating_mul(1u64 << shift);
    let jitter = deterministic_jitter_ms(channel_id, attempt + 13, MAX_BACKOFF_JITTER_MS);
    exponential.saturating_add(jitter).min(MAX_BACKOFF_MS)
}

fn deterministic_jitter_ms(channel_id: &str, attempt: usize, max_jitter_ms: u64) -> u64 {
    if max_jitter_ms == 0 {
        return 0;
    }

    let seed = channel_id
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(131).wrapping_add(b as u64))
        .wrapping_add((attempt as u64).wrapping_mul(97));
    seed % (max_jitter_ms + 1)
}

/// Load channel IDs from a file (one per line)
pub fn load_channel_ids(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    Ok(content
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::parse_iso8601_duration_seconds;

    #[test]
    fn parses_common_iso_durations() {
        assert_eq!(parse_iso8601_duration_seconds("PT15M33S"), Some(933));
        assert_eq!(parse_iso8601_duration_seconds("PT2H"), Some(7_200));
        assert_eq!(parse_iso8601_duration_seconds("P1DT2H3M4S"), Some(93_784));
    }

    #[test]
    fn rejects_unsupported_or_invalid_durations() {
        assert_eq!(parse_iso8601_duration_seconds("15M"), None);
        assert_eq!(parse_iso8601_duration_seconds("P1M"), None);
        assert_eq!(parse_iso8601_duration_seconds("PT"), None);
    }
}
