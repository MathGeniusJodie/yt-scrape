use crate::data::Video;
use crate::feed::parser::parse_feed;
use crate::urls;
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};

/// Progress updates during feed fetching
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FetchProgress {
    Started { total: usize },
    ChannelComplete { channel: String, count: usize },
    Error { channel_id: String, error: String },
    AllComplete { total_videos: usize },
}

/// Fetch all feeds from the given channel IDs
pub async fn fetch_all_feeds(
    channel_ids: Vec<String>,
    tx: mpsc::Sender<FetchProgress>,
) -> anyhow::Result<Vec<Video>> {
    let total = channel_ids.len();
    tx.send(FetchProgress::Started { total }).await?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .gzip(true)
        .build()?;

    // Limit concurrent requests
    let semaphore = Arc::new(Semaphore::new(100));
    let mut handles = Vec::new();

    for channel_id in channel_ids {
        let permit = semaphore.clone().acquire_owned().await?;
        let client = client.clone();
        let tx = tx.clone();
        let cid = channel_id.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let url = urls::feed_url(&cid);

            match client.get(&url).send().await {
                Ok(resp) => match resp.text().await {
                    Ok(text) => {
                        let videos = parse_feed(&text, &cid);
                        let count = videos.len();
                        let _ = tx
                            .send(FetchProgress::ChannelComplete {
                                channel: cid,
                                count,
                            })
                            .await;
                        videos
                    }
                    Err(e) => {
                        let _ = tx
                            .send(FetchProgress::Error {
                                channel_id: cid,
                                error: e.to_string(),
                            })
                            .await;
                        Vec::new()
                    }
                },
                Err(e) => {
                    let _ = tx
                        .send(FetchProgress::Error {
                            channel_id: cid,
                            error: e.to_string(),
                        })
                        .await;
                    Vec::new()
                }
            }
        }));
    }

    // Collect all results
    let mut all_videos = Vec::new();
    for handle in handles {
        if let Ok(videos) = handle.await {
            all_videos.extend(videos);
        }
    }

    // Sort by published date (newest first)
    all_videos.sort_by(|a, b| b.published.cmp(&a.published));

    // Keep only the most recent 400
    all_videos.truncate(400);

    let total_videos = all_videos.len();
    tx.send(FetchProgress::AllComplete { total_videos }).await?;

    Ok(all_videos)
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
