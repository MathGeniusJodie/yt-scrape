use crate::data::{NewVideo, Video};
use crate::urls;
use async_channel::Sender;
use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use log::warn;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::time::{Duration, Instant, sleep};

const YOUTUBE_PLAYLIST_ITEMS_API_URL: &str = "https://www.googleapis.com/youtube/v3/playlistItems";
const YOUTUBE_CHANNELS_API_URL: &str = "https://www.googleapis.com/youtube/v3/channels";
const YOUTUBE_VIDEOS_API_URL: &str = "https://www.googleapis.com/youtube/v3/videos";
const YOUTUBE_SEARCH_API_URL: &str = "https://www.googleapis.com/youtube/v3/search";
const YOUTUBE_COMMENT_THREADS_API_URL: &str =
    "https://www.googleapis.com/youtube/v3/commentThreads";
const PLAYLIST_ITEMS_MAX_RESULTS: u32 = 25;
const SEARCH_MAX_RESULTS: u32 = 25;
const COMMENTS_PAGE_SIZE: u32 = 50;
const MAX_COMMENT_THREADS: usize = 100;
const MAX_FETCH_ATTEMPTS: usize = 3;
const MAX_CONCURRENT_CHANNEL_FETCHES: usize = 32;
const MAX_FEED_VIDEOS: usize = 400;
const INITIAL_BACKOFF_MS: u64 = 500;
const MAX_BACKOFF_MULTIPLIER: u64 = 4;

/// Errors that can occur while loading and fetching feed metadata.
#[derive(Debug, Error)]
pub enum FeedError {
    /// Channel ID file could not be read.
    #[error("failed to read channel IDs from {path}: {source}")]
    ReadChannelIds {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Every subscribed channel failed to fetch, so the existing feed is kept
    /// rather than replaced with an empty one.
    #[error("all {failed} channels failed to fetch; keeping the existing feed")]
    AllChannelsFailed {
        /// Number of channels that failed.
        failed: usize,
    },
}

/// A failed `YouTube` Data API request with the server's explanation preserved.
#[derive(Debug, Error)]
pub enum ApiRequestError {
    /// Network-level failure (connect, timeout, TLS, ...). The URL is stripped
    /// so the API key in the query string never reaches logs or the UI.
    #[error("request failed: {0}")]
    Transport(reqwest::Error),
    /// The API returned a non-success status; `message` carries the error body's
    /// explanation (quota exceeded, invalid key, ...).
    #[error("HTTP {status}: {message}")]
    Status {
        /// HTTP status code returned by the API.
        status: reqwest::StatusCode,
        /// Human-readable error message extracted from the response body.
        message: String,
    },
    /// The response body could not be parsed as the expected JSON shape.
    #[error("failed to parse API response: {0}")]
    Parse(#[from] serde_json::Error),
}

impl ApiRequestError {
    fn status(&self) -> Option<reqwest::StatusCode> {
        match self {
            Self::Transport(error) => error.status(),
            Self::Status { status, .. } => Some(*status),
            Self::Parse(_) => None,
        }
    }

    fn is_flaky_transport(&self) -> bool {
        matches!(self, Self::Transport(error) if error.is_timeout() || error.is_connect())
    }
}

impl From<reqwest::Error> for ApiRequestError {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url())
    }
}

/// Extracts `error.message` from a `YouTube` API error body, falling back to a
/// truncated raw body.
fn api_error_message(body: &str) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        error: ErrorDetail,
    }
    #[derive(Deserialize)]
    struct ErrorDetail {
        message: String,
    }

    serde_json::from_str::<ErrorBody>(body).map_or_else(
        |_| body.trim().chars().take(300).collect(),
        |parsed| parsed.error.message,
    )
}

/// Performs a GET request against the `YouTube` Data API and decodes the JSON body,
/// preserving the API's own error message on failure.
async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    query: &[(&str, &str)],
) -> Result<T, ApiRequestError> {
    let response = client.get(url).query(query).send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(ApiRequestError::Status {
            status,
            message: api_error_message(&body),
        });
    }
    Ok(serde_json::from_str(&body)?)
}

/// Progress updates during feed fetching
#[derive(Debug, Clone)]
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
struct SearchResponse {
    #[serde(default)]
    items: Vec<SearchItem>,
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
#[serde(rename_all = "camelCase")]
struct CommentThreadsResponse {
    #[serde(default)]
    items: Vec<CommentThreadItem>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CommentThreadItem {
    snippet: Option<CommentThreadSnippet>,
    replies: Option<CommentThreadReplies>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentThreadSnippet {
    top_level_comment: Option<CommentItem>,
    total_reply_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CommentThreadReplies {
    #[serde(default)]
    comments: Vec<CommentItem>,
}

#[derive(Debug, Deserialize)]
struct CommentItem {
    snippet: Option<CommentSnippet>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentSnippet {
    author_display_name: Option<String>,
    text_display: Option<String>,
    published_at: Option<DateTime<Utc>>,
    like_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YoutubeVideoItem {
    id: Option<String>,
    content_details: Option<YoutubeVideoContentDetails>,
}

#[derive(Debug, Deserialize)]
struct YoutubeVideoContentDetails {
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelItem {
    content_details: Option<ChannelContentDetails>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelContentDetails {
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
struct SearchItem {
    id: Option<VideoResourceId>,
    snippet: Option<SearchSnippet>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistItemSnippet {
    published_at: Option<DateTime<Utc>>,
    title: Option<String>,
    channel_id: Option<String>,
    channel_title: Option<String>,
    resource_id: Option<VideoResourceId>,
    thumbnails: Option<PlaylistThumbnails>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchSnippet {
    published_at: Option<DateTime<Utc>>,
    title: Option<String>,
    channel_id: Option<String>,
    channel_title: Option<String>,
    thumbnails: Option<PlaylistThumbnails>,
}

/// Video reference shared by playlist-item `snippet.resourceId` and search `id` payloads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoResourceId {
    video_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistThumbnails {
    maxres: Option<Thumbnail>,
    standard: Option<Thumbnail>,
    high: Option<Thumbnail>,
    medium: Option<Thumbnail>,
    #[serde(rename = "default")]
    default_thumbnail: Option<Thumbnail>,
}

impl PlaylistThumbnails {
    fn preferred_url(&self) -> Option<String> {
        [
            &self.medium,
            &self.high,
            &self.standard,
            &self.maxres,
            &self.default_thumbnail,
        ]
        .into_iter()
        .find_map(|t| t.as_ref().map(|t| t.url.clone()))
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

impl PendingChannel {
    fn schedule_retry(&mut self, delay_ms: u64) {
        self.attempt += 1;
        self.not_before = Instant::now() + Duration::from_millis(delay_ms);
    }
}

async fn send_retry_progress(
    tx: &Sender<FetchProgress>,
    channel_id: &str,
    next_attempt: usize,
    delay_secs: u64,
    reason: &str,
) {
    let _ = tx
        .send(FetchProgress::RetryScheduled {
            channel_id: channel_id.to_string(),
            next_attempt,
            max_attempts: MAX_FETCH_ATTEMPTS,
            delay_secs,
            reason: reason.to_string(),
        })
        .await;
}

enum ChannelFetchResult {
    Success { videos: Vec<Video> },
    Failed,
}

/// Fetch all feeds from the given channel IDs
pub async fn fetch_all_feeds(
    client: &reqwest::Client,
    api_key: String,
    channel_ids: Vec<String>,
    tx: Sender<FetchProgress>,
) -> Result<Vec<Video>, FeedError> {
    let total = channel_ids.len();
    let _ = tx.send(FetchProgress::Started { total }).await;

    let mut all_videos = Vec::new();
    let mut successful_channels = 0usize;
    let mut failed_channels = 0usize;
    let mut pending_channels = Vec::new();

    for channel_id in channel_ids {
        if let Some(uploads_playlist_id) = uploads_playlist_id_for_channel(&channel_id) {
            pending_channels.push(PendingChannel {
                channel_id,
                uploads_playlist_id,
                attempt: 1,
                not_before: Instant::now(),
            });
        } else {
            failed_channels += 1;
            let _ = tx
                .send(FetchProgress::Error {
                    channel_id,
                    error: "Invalid channel ID. Expected an ID starting with 'UC'.".to_string(),
                })
                .await;
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

    // A total failure (e.g. network outage) must not be applied as an empty
    // feed: that would wipe cached videos and their artifacts permanently.
    if successful_channels == 0 && total > 0 {
        return Err(FeedError::AllChannelsFailed {
            failed: failed_channels,
        });
    }

    // Sort by published date (newest first)
    all_videos.sort_by_key(|video| std::cmp::Reverse(video.published()));

    // Keep only the most recent videos to cap UI and cache churn.
    all_videos.truncate(MAX_FEED_VIDEOS);

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

/// Search `YouTube` for videos matching a query.
///
/// # Arguments
///
/// * `client` - HTTP client used for `YouTube` Data API requests.
/// * `api_key` - `YouTube` Data API key.
/// * `query` - Search text to send to `YouTube`.
///
/// # Returns
///
/// Video metadata in the order returned by `YouTube` search.
///
/// # Errors
///
/// Returns [`ApiRequestError`] if the `YouTube` search request fails.
pub async fn fetch_youtube_search(
    client: &reqwest::Client,
    api_key: &str,
    query: &str,
) -> Result<Vec<Video>, ApiRequestError> {
    let max_results = SEARCH_MAX_RESULTS.to_string();
    let payload: SearchResponse = get_json(
        client,
        YOUTUBE_SEARCH_API_URL,
        &[
            ("part", "snippet"),
            ("type", "video"),
            ("maxResults", max_results.as_str()),
            ("q", query),
            ("key", api_key),
        ],
    )
    .await?;
    let video_ids = video_ids_from_search_items(&payload);
    // Durations are cosmetic; a failed lookup must not fail the whole search.
    let durations_by_video_id = match fetch_video_durations(client, api_key, &video_ids).await {
        Ok(durations) => durations,
        Err(error) => {
            warn!("Failed fetching durations for search results: {error}");
            HashMap::new()
        }
    };

    Ok(videos_from_search_items(payload, &durations_by_video_id))
}

/// Fetches and formats public comments for a `YouTube` video.
///
/// # Arguments
///
/// * `client` - HTTP client used for `YouTube` Data API requests.
/// * `api_key` - `YouTube` Data API key.
/// * `video_id` - `YouTube` video ID whose public comment threads should be loaded.
///
/// # Returns
///
/// A readable text representation of relevant public comments and included replies.
///
/// # Errors
///
/// Returns [`ApiRequestError`] if `YouTube` rejects or fails the comments request.
pub async fn fetch_youtube_comments(
    client: &reqwest::Client,
    api_key: &str,
    video_id: &str,
) -> Result<String, ApiRequestError> {
    let mut comments = Vec::with_capacity(MAX_COMMENT_THREADS);
    let mut next_page_token = None::<String>;

    while comments.len() < MAX_COMMENT_THREADS {
        let page_size = COMMENTS_PAGE_SIZE
            .min((MAX_COMMENT_THREADS - comments.len()) as u32)
            .to_string();

        let mut query = vec![
            ("part", "snippet,replies"),
            ("videoId", video_id),
            ("maxResults", page_size.as_str()),
            ("order", "relevance"),
            ("textFormat", "plainText"),
            ("key", api_key),
        ];
        if let Some(page_token) = next_page_token.as_deref() {
            query.push(("pageToken", page_token));
        }

        let payload: CommentThreadsResponse =
            get_json(client, YOUTUBE_COMMENT_THREADS_API_URL, &query).await?;

        if payload.items.is_empty() {
            break;
        }

        comments.extend(payload.items);
        next_page_token = payload.next_page_token;
        if next_page_token.is_none() {
            break;
        }
    }

    Ok(format_comment_threads(&comments))
}

async fn fetch_channel_with_retries(
    client: reqwest::Client,
    api_key: String,
    mut pending: PendingChannel,
    tx: Sender<FetchProgress>,
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
                            if pending.attempt >= MAX_FETCH_ATTEMPTS {
                                let _ = tx
                                    .send(FetchProgress::Error {
                                        channel_id: pending.channel_id,
                                        error: "Exceeded retry budget while resolving uploads playlist ID."
                                            .to_string(),
                                    })
                                    .await;
                                return ChannelFetchResult::Failed;
                            }

                            send_retry_progress(
                                &tx,
                                &pending.channel_id,
                                pending.attempt + 1,
                                0,
                                "Resolved uploads playlist ID after 404; queued for retry.",
                            )
                            .await;

                            pending.schedule_retry(0);
                            pending.uploads_playlist_id = resolved_uploads_playlist_id;
                            continue;
                        }
                        Ok(_) => {
                            // No alternate uploads playlist was found (or it matches the current one),
                            // so we intentionally fall through to the normal retry/terminal handling.
                        }
                        Err(lookup_error) => {
                            warn!(
                                "Failed resolving uploads playlist for {} after 404: {}",
                                pending.channel_id, lookup_error
                            );
                        }
                    }
                }

                if should_retry(&error) && pending.attempt < MAX_FETCH_ATTEMPTS {
                    let delay_ms = backoff_ms_for_attempt(pending.attempt, &error);
                    send_retry_progress(
                        &tx,
                        &pending.channel_id,
                        pending.attempt + 1,
                        delay_ms.div_ceil(1000),
                        &error.to_string(),
                    )
                    .await;
                    pending.schedule_retry(delay_ms);
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
) -> Result<Vec<Video>, ApiRequestError> {
    let max_results = PLAYLIST_ITEMS_MAX_RESULTS.to_string();
    let payload: PlaylistItemsResponse = get_json(
        client,
        YOUTUBE_PLAYLIST_ITEMS_API_URL,
        &[
            ("part", "snippet"),
            ("playlistId", uploads_playlist_id),
            ("maxResults", max_results.as_str()),
            ("key", api_key),
        ],
    )
    .await?;
    let video_ids = video_ids_from_playlist_items(&payload);
    let durations_by_video_id = match fetch_video_durations(client, api_key, &video_ids).await {
        Ok(durations) => durations,
        Err(error) => {
            warn!("Failed fetching durations for channel {channel_id}: {error}");
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
) -> Result<Option<String>, ApiRequestError> {
    let payload: ChannelsResponse = get_json(
        client,
        YOUTUBE_CHANNELS_API_URL,
        &[
            ("part", "contentDetails"),
            ("id", channel_id),
            ("key", api_key),
        ],
    )
    .await?;

    Ok(payload
        .items
        .into_iter()
        .find_map(|item| item.content_details?.related_playlists?.uploads))
}

fn format_comment_threads(items: &[CommentThreadItem]) -> String {
    if items.is_empty() {
        return "No public comments are available for this video.".to_string();
    }

    let mut output = String::new();
    let mut thread_number = 0usize;
    for item in items {
        let Some(snippet) = item.snippet.as_ref() else {
            continue;
        };
        let Some(top_level_comment) = snippet.top_level_comment.as_ref() else {
            continue;
        };

        if !output.is_empty() {
            output.push('\n');
        }
        thread_number += 1;
        push_formatted_comment(&mut output, thread_number, top_level_comment, "");

        let replies = item
            .replies
            .as_ref()
            .map(|replies| replies.comments.as_slice())
            .unwrap_or_default();
        for reply in replies {
            push_formatted_comment(&mut output, 0, reply, "  ");
        }

        let loaded_reply_count = replies.len() as u32;
        let total_reply_count = snippet.total_reply_count.unwrap_or(loaded_reply_count);
        if total_reply_count > loaded_reply_count {
            let _ = writeln!(
                output,
                "  ... {} more replies not loaded by YouTube in this response",
                total_reply_count - loaded_reply_count
            );
        }
    }

    if output.is_empty() {
        "No public comments are available for this video.".to_string()
    } else {
        output
    }
}

fn push_formatted_comment(output: &mut String, index: usize, comment: &CommentItem, indent: &str) {
    let Some(snippet) = comment.snippet.as_ref() else {
        return;
    };

    let author = snippet.author_display_name.as_deref().unwrap_or("Unknown");
    let text = snippet.text_display.as_deref().unwrap_or("").trim();
    let like_count = snippet.like_count.unwrap_or_default();
    let published = snippet
        .published_at
        .map(|published| format!(" | {}", published.format("%Y-%m-%d")))
        .unwrap_or_default();
    let prefix = if index == 0 {
        "-".to_string()
    } else {
        format!("{index}.")
    };

    let _ = writeln!(
        output,
        "{indent}{prefix} {author} | {like_count} likes{published}"
    );
    let _ = writeln!(output, "{indent}{}", text.replace('\n', "\n  "));
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
                .and_then(PlaylistThumbnails::preferred_url)
                .unwrap_or_else(|| urls::thumbnail_url(&video_id));
            let duration_seconds = durations_by_video_id.get(&video_id).copied();

            Some(Video::new(NewVideo {
                video_id,
                channel_id,
                channel_name,
                title,
                published,
                thumbnail_url,
                duration_seconds,
            }))
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

fn videos_from_search_items(
    response: SearchResponse,
    durations_by_video_id: &HashMap<String, u32>,
) -> Vec<Video> {
    response
        .items
        .into_iter()
        .filter_map(|item| {
            let video_id = item.id?.video_id?;
            let snippet = item.snippet?;
            let published = snippet.published_at?;

            let title = snippet.title.unwrap_or_else(|| "Untitled".to_string());
            let channel_id = snippet.channel_id.unwrap_or_default();
            let channel_name = snippet
                .channel_title
                .unwrap_or_else(|| "Unknown channel".to_string());
            let thumbnail_url = snippet
                .thumbnails
                .as_ref()
                .and_then(PlaylistThumbnails::preferred_url)
                .unwrap_or_else(|| urls::thumbnail_url(&video_id));
            let duration_seconds = durations_by_video_id.get(&video_id).copied();

            Some(Video::new(NewVideo {
                video_id,
                channel_id,
                channel_name,
                title,
                published,
                thumbnail_url,
                duration_seconds,
            }))
        })
        .collect()
}

fn video_ids_from_search_items(response: &SearchResponse) -> Vec<String> {
    response
        .items
        .iter()
        .filter_map(|item| item.id.as_ref()?.video_id.clone())
        .collect()
}

async fn fetch_video_durations(
    client: &reqwest::Client,
    api_key: &str,
    video_ids: &[String],
) -> Result<HashMap<String, u32>, ApiRequestError> {
    if video_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let video_ids_csv = video_ids.join(",");
    let payload: VideosResponse = get_json(
        client,
        YOUTUBE_VIDEOS_API_URL,
        &[
            ("part", "contentDetails"),
            ("id", video_ids_csv.as_str()),
            ("key", api_key),
        ],
    )
    .await?;
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
    let duration = std::time::Duration::from(iso8601::duration(input).ok()?);
    let rounded_seconds = duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_millis() >= 500));
    u32::try_from(rounded_seconds).ok()
}

fn uploads_playlist_id_for_channel(channel_id: &str) -> Option<String> {
    channel_id
        .strip_prefix("UC")
        .map(|suffix| format!("UU{suffix}"))
}

fn should_retry(error: &ApiRequestError) -> bool {
    if error.is_flaky_transport() {
        return true;
    }

    // 404 is handled by uploads-playlist resolution in the caller and is not transient here.
    error.status().is_some_and(|status| {
        status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status.is_server_error()
    })
}

fn backoff_ms_for_attempt(attempt: usize, error: &ApiRequestError) -> u64 {
    let base_ms = if error.is_flaky_transport() {
        3_000
    } else {
        match error.status() {
            Some(s) if s == reqwest::StatusCode::TOO_MANY_REQUESTS => 10_000,
            Some(s) if s.is_server_error() => 5_000,
            Some(s) if s == reqwest::StatusCode::REQUEST_TIMEOUT => 3_000,
            _ => INITIAL_BACKOFF_MS,
        }
    };

    backoff_ms_with_base(base_ms, attempt)
}

fn backoff_ms_with_base(base_ms: u64, attempt: usize) -> u64 {
    let max_exponent = MAX_BACKOFF_MULTIPLIER.ilog2() as usize;
    let attempt_multiplier = 1u64 << attempt.saturating_sub(1).min(max_exponent);
    base_ms.saturating_mul(attempt_multiplier)
}

/// Load channel IDs from a file (one per line)
///
/// # Arguments
///
/// * `path` - Text file containing channel IDs, one per line.
///
/// # Returns
///
/// Parsed channel IDs excluding blank lines and comment lines beginning with `#`.
///
/// # Errors
///
/// Returns an error if reading the input file fails.
pub fn load_channel_ids(path: &Path) -> Result<Vec<String>, FeedError> {
    let content = std::fs::read_to_string(path).map_err(|source| FeedError::ReadChannelIds {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .map(ToString::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        ApiRequestError, CommentItem, CommentSnippet, CommentThreadItem, CommentThreadReplies,
        CommentThreadSnippet, FeedError, FetchProgress, INITIAL_BACKOFF_MS, api_error_message,
        backoff_ms_with_base, fetch_all_feeds, format_comment_threads, get_json,
        parse_iso8601_duration_seconds, should_retry, uploads_playlist_id_for_channel,
    };
    use chrono::{TimeZone, Utc};
    use serde::Deserialize;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// Starts a throwaway HTTP server on an ephemeral port that answers exactly one
    /// request with `response` (a full raw HTTP response including status line and
    /// headers, minus the trailing blank line and body which this appends).
    fn spawn_one_shot_server(status_line: &str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        let status_line = status_line.to_string();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept connection");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            // Drain the request until the blank line terminating the headers.
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let mut stream = stream;
            let response = format!(
                "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
        });
        format!("http://127.0.0.1:{port}/")
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct TestPayload {
        value: u32,
    }

    #[tokio::test]
    async fn get_json_decodes_success_body() {
        let url = spawn_one_shot_server("HTTP/1.1 200 OK", r#"{"value":42}"#);
        let client = reqwest::Client::new();

        let payload: TestPayload = get_json(&client, &url, &[]).await.expect("should succeed");

        assert_eq!(payload, TestPayload { value: 42 });
    }

    #[tokio::test]
    async fn get_json_surfaces_youtube_error_message_on_403() {
        let url = spawn_one_shot_server(
            "HTTP/1.1 403 Forbidden",
            r#"{"error":{"message":"quota exceeded"}}"#,
        );
        let client = reqwest::Client::new();

        let error = get_json::<TestPayload>(&client, &url, &[])
            .await
            .expect_err("403 should fail");

        match error {
            ApiRequestError::Status { status, message } => {
                assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
                assert_eq!(message, "quota exceeded");
            }
            other => panic!("expected Status error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_json_falls_back_to_truncated_body_on_non_json_error() {
        let url = spawn_one_shot_server("HTTP/1.1 500 Internal Server Error", "kaboom");
        let client = reqwest::Client::new();

        let error = get_json::<TestPayload>(&client, &url, &[])
            .await
            .expect_err("500 should fail");

        match error {
            ApiRequestError::Status { status, message } => {
                assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
                assert_eq!(message, "kaboom");
            }
            other => panic!("expected Status error, got {other:?}"),
        }
    }

    #[test]
    fn api_error_message_truncates_non_json_body() {
        let long_body = "x".repeat(400);
        assert_eq!(api_error_message(&long_body).chars().count(), 300);
    }

    #[test]
    fn parses_common_iso_durations() {
        assert_eq!(parse_iso8601_duration_seconds("PT15M33S"), Some(933));
        assert_eq!(parse_iso8601_duration_seconds("PT2H"), Some(7_200));
        assert_eq!(parse_iso8601_duration_seconds("P1DT2H3M4S"), Some(93_784));
        assert_eq!(parse_iso8601_duration_seconds("P2W"), Some(1_209_600));
        assert_eq!(parse_iso8601_duration_seconds("P1M"), Some(2_592_000));
        assert_eq!(parse_iso8601_duration_seconds("P1Y"), Some(31_536_000));
    }

    #[test]
    fn rejects_invalid_iso_durations() {
        assert_eq!(parse_iso8601_duration_seconds("15M"), None);
    }

    #[test]
    fn parses_empty_duration_as_zero_seconds() {
        assert_eq!(parse_iso8601_duration_seconds("PT"), Some(0));
    }

    #[test]
    fn rounds_fractional_seconds() {
        assert_eq!(parse_iso8601_duration_seconds("PT1.499S"), Some(1));
        assert_eq!(parse_iso8601_duration_seconds("PT1.5S"), Some(2));
    }

    #[test]
    fn caps_growth_for_small_base() {
        assert_eq!(backoff_ms_with_base(INITIAL_BACKOFF_MS, 1), 500);
        assert_eq!(backoff_ms_with_base(INITIAL_BACKOFF_MS, 2), 1_000);
        assert_eq!(backoff_ms_with_base(INITIAL_BACKOFF_MS, 3), 2_000);
        assert_eq!(backoff_ms_with_base(INITIAL_BACKOFF_MS, 4), 2_000);
    }

    #[test]
    fn preserves_large_base_backoff() {
        assert_eq!(backoff_ms_with_base(20_000, 1), 20_000);
        assert_eq!(backoff_ms_with_base(20_000, 2), 40_000);
        assert_eq!(backoff_ms_with_base(30_000, 2), 60_000);
    }

    #[test]
    fn formats_comment_threads_with_replies() {
        let item = CommentThreadItem {
            snippet: Some(CommentThreadSnippet {
                top_level_comment: Some(comment("Ada", "Great talk", 12)),
                total_reply_count: Some(2),
            }),
            replies: Some(CommentThreadReplies {
                comments: vec![comment("Grace", "Agreed", 3)],
            }),
        };

        let formatted = format_comment_threads(&[item]);

        assert!(formatted.contains("1. Ada | 12 likes | 2024-01-01"));
        assert!(formatted.contains("Great talk"));
        assert!(formatted.contains("  - Grace | 3 likes | 2024-01-01"));
        assert!(formatted.contains("... 1 more replies not loaded"));
    }

    fn comment(author: &str, text: &str, like_count: u32) -> CommentItem {
        CommentItem {
            snippet: Some(CommentSnippet {
                author_display_name: Some(author.to_string()),
                text_display: Some(text.to_string()),
                published_at: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).single(),
                like_count: Some(like_count),
            }),
        }
    }

    #[test]
    fn uploads_playlist_id_derives_from_uc_prefixed_channel_id() {
        assert_eq!(
            uploads_playlist_id_for_channel("UCabc"),
            Some("UUabc".to_string())
        );
        assert_eq!(uploads_playlist_id_for_channel("xyz"), None);
    }

    fn status_error(status: reqwest::StatusCode) -> ApiRequestError {
        ApiRequestError::Status {
            status,
            message: "boom".to_string(),
        }
    }

    #[test]
    fn should_retry_on_transient_server_errors() {
        assert!(should_retry(&status_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        )));
        assert!(should_retry(&status_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        )));
        assert!(should_retry(&status_error(
            reqwest::StatusCode::REQUEST_TIMEOUT
        )));
    }

    #[test]
    fn should_not_retry_on_permanent_client_errors() {
        assert!(!should_retry(&status_error(reqwest::StatusCode::NOT_FOUND)));
        assert!(!should_retry(&status_error(reqwest::StatusCode::FORBIDDEN)));
    }

    #[tokio::test]
    async fn fetch_all_feeds_fails_when_every_channel_id_is_invalid() {
        let client = reqwest::Client::new();
        let (tx, rx) = async_channel::unbounded();
        let channel_ids = vec!["invalid-1".to_string(), "invalid-2".to_string()];

        let result = fetch_all_feeds(&client, "unused-key".to_string(), channel_ids, tx).await;

        match result {
            Err(FeedError::AllChannelsFailed { failed }) => assert_eq!(failed, 2),
            other => panic!("expected AllChannelsFailed, got {other:?}"),
        }

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(matches!(events[0], FetchProgress::Started { total: 2 }));
        let error_count = events
            .iter()
            .filter(|event| matches!(event, FetchProgress::Error { .. }))
            .count();
        assert_eq!(error_count, 2);
    }

    #[tokio::test]
    async fn fetch_all_feeds_with_no_channels_completes_with_empty_feed() {
        let client = reqwest::Client::new();
        let (tx, rx) = async_channel::unbounded();

        let videos = fetch_all_feeds(&client, "unused-key".to_string(), Vec::new(), tx)
            .await
            .expect("no channels should not fail");

        assert!(videos.is_empty());

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(events.iter().any(|event| matches!(
            event,
            FetchProgress::AllComplete {
                total_videos: 0,
                ..
            }
        )));
    }
}
