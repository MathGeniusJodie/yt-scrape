use crate::data::Video;
use crate::feed::parser::parse_feed;
use crate::urls;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, Duration, Instant};

// Conservative defaults to avoid tripping YouTube rate limits.
const MAX_CONCURRENT_REQUESTS: usize = 1;
const MIN_REQUEST_SPACING_MS: u64 = 4_000;
const MAX_REQUEST_JITTER_MS: u64 = 1_500;
const MAX_FETCH_ATTEMPTS: usize = 3;
const INITIAL_BACKOFF_MS: u64 = 15_000;
const MAX_BACKOFF_MS: u64 = 300_000;
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

#[derive(Debug)]
struct ChannelFetchResult {
    videos: Vec<Video>,
    failed: bool,
}

#[derive(Debug)]
struct RequestThrottle {
    next_allowed_at: Mutex<Instant>,
}

impl RequestThrottle {
    fn new() -> Self {
        Self {
            next_allowed_at: Mutex::new(Instant::now()),
        }
    }

    async fn wait_for_turn(&self, channel_id: &str, attempt: usize) {
        let spacing = Duration::from_millis(request_spacing_ms(channel_id, attempt));

        let mut next_allowed_at = self.next_allowed_at.lock().await;
        let now = Instant::now();
        let scheduled_start = if *next_allowed_at > now {
            *next_allowed_at
        } else {
            now
        };
        *next_allowed_at = scheduled_start + spacing;
        drop(next_allowed_at);

        let now = Instant::now();
        if scheduled_start > now {
            sleep(scheduled_start - now).await;
        }
    }
}

/// Fetch all feeds from the given channel IDs
pub async fn fetch_all_feeds(
    channel_ids: Vec<String>,
    tx: mpsc::Sender<FetchProgress>,
) -> anyhow::Result<Vec<Video>> {
    let total = channel_ids.len();
    let _ = tx.send(FetchProgress::Started { total }).await;

    let client = reqwest::Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .gzip(true)
        .build()?;
    let throttle = Arc::new(RequestThrottle::new());

    // Strictly bounded concurrency; request starts are additionally rate-limited globally.
    let mut fetches = stream::iter(channel_ids.into_iter().map(|channel_id| {
        let client = client.clone();
        let tx = tx.clone();
        let throttle = throttle.clone();
        async move { fetch_channel_with_retry(client, tx, throttle, channel_id).await }
    }))
    .buffer_unordered(MAX_CONCURRENT_REQUESTS);

    // Collect all results
    let mut all_videos = Vec::new();
    let mut successful_channels = 0usize;
    let mut failed_channels = 0usize;

    while let Some(ChannelFetchResult { videos, failed }) = fetches.next().await {
        if failed {
            failed_channels += 1;
        } else {
            successful_channels += 1;
        }
        all_videos.extend(videos);
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

async fn fetch_channel_with_retry(
    client: reqwest::Client,
    tx: mpsc::Sender<FetchProgress>,
    throttle: Arc<RequestThrottle>,
    channel_id: String,
) -> ChannelFetchResult {
    let url = urls::feed_url(&channel_id);

    for attempt in 1..=MAX_FETCH_ATTEMPTS {
        throttle.wait_for_turn(&channel_id, attempt).await;

        let request_result = async {
            let response = client.get(&url).send().await?;
            let response = response.error_for_status()?;
            response.text().await
        }
        .await;

        match request_result {
            Ok(text) => {
                let videos = parse_feed(&text, &channel_id);
                let count = videos.len();
                let _ = tx
                    .send(FetchProgress::ChannelComplete {
                        channel: channel_id.clone(),
                        count,
                    })
                    .await;
                return ChannelFetchResult {
                    videos,
                    failed: false,
                };
            }
            Err(error) => {
                if should_retry(&error) && attempt < MAX_FETCH_ATTEMPTS {
                    let delay_ms = backoff_ms_for_attempt(attempt, &channel_id, &error);
                    let _ = tx
                        .send(FetchProgress::RetryScheduled {
                            channel_id: channel_id.clone(),
                            next_attempt: attempt + 1,
                            max_attempts: MAX_FETCH_ATTEMPTS,
                            delay_secs: delay_ms.div_ceil(1000),
                            reason: error.to_string(),
                        })
                        .await;
                    sleep(Duration::from_millis(delay_ms)).await;
                    continue;
                }

                let _ = tx
                    .send(FetchProgress::Error {
                        channel_id: channel_id.clone(),
                        error: error.to_string(),
                    })
                    .await;
                return ChannelFetchResult {
                    videos: Vec::new(),
                    failed: true,
                };
            }
        }
    }

    ChannelFetchResult {
        videos: Vec::new(),
        failed: true,
    }
}

fn should_retry(error: &reqwest::Error) -> bool {
    if error.is_timeout() || error.is_connect() {
        return true;
    }

    match error.status() {
        Some(status) => {
            status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::REQUEST_TIMEOUT
                || status.is_server_error()
        }
        None => false,
    }
}

fn request_spacing_ms(channel_id: &str, attempt: usize) -> u64 {
    MIN_REQUEST_SPACING_MS + deterministic_jitter_ms(channel_id, attempt, MAX_REQUEST_JITTER_MS)
}

fn backoff_ms_for_attempt(attempt: usize, channel_id: &str, error: &reqwest::Error) -> u64 {
    let base_ms = match error.status() {
        Some(reqwest::StatusCode::TOO_MANY_REQUESTS) => 30_000,
        Some(reqwest::StatusCode::REQUEST_TIMEOUT) => 12_000,
        Some(status) if status.is_server_error() => 20_000,
        _ if error.is_timeout() => 12_000,
        _ if error.is_connect() => 12_000,
        _ => INITIAL_BACKOFF_MS,
    };

    let shift = attempt.saturating_sub(1).min(6) as u32;
    let exponential = base_ms.saturating_mul(1u64 << shift);
    let jitter = deterministic_jitter_ms(channel_id, attempt + 13, 2_000);
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
