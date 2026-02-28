use super::cards::populate_flow_box;
use super::{update_watch_later_badge, AppState, UiContext};
use crate::cache::Storage;
use crate::data::{Tab, Video};

use futures::stream::{self, StreamExt};
use log::warn;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

pub(super) fn merge_cached_video_fields(videos: &mut [Video], cached_videos: &[Video]) {
    let cached_by_id: HashMap<&str, &Video> = cached_videos
        .iter()
        .map(|video| (video.video_id.as_str(), video))
        .collect();

    for video in videos {
        if let Some(cached) = cached_by_id.get(video.video_id.as_str()) {
            if video.transcript.is_none() {
                video.transcript = cached.transcript.clone();
            }
            if video.ai_summary.is_none() {
                video.ai_summary = cached.ai_summary.clone();
            }
        }
    }
}

pub(super) fn refresh_video_lists(state_rc: &Rc<RefCell<AppState>>, ui_context: &UiContext) {
    let state_ref = state_rc.borrow();
    let downloaded_video_ids = state_ref.storage.cached_video_ids();
    update_watch_later_badge(&ui_context.badge, state_ref.watch_later.len());
    populate_flow_box(
        &ui_context.feed_flow,
        &state_ref,
        Tab::Feed,
        &downloaded_video_ids,
        state_rc,
        ui_context,
    );
    populate_flow_box(
        &ui_context.watch_later_flow,
        &state_ref,
        Tab::WatchLater,
        &downloaded_video_ids,
        state_rc,
        ui_context,
    );
}

pub(super) fn download_missing_thumbnails(
    videos: &[Video],
    storage: &Storage,
    runtime: Arc<tokio::runtime::Runtime>,
) {
    const THUMBNAIL_DOWNLOAD_CONCURRENCY: usize = 12;

    let pending_downloads: Vec<(String, PathBuf)> = videos
        .iter()
        .filter_map(|video| {
            let path = storage.thumbnail_path(&video.video_id);
            if path.exists() {
                None
            } else {
                Some((video.thumbnail_url.clone(), path))
            }
        })
        .collect();

    if pending_downloads.is_empty() {
        return;
    }

    runtime.spawn(async move {
        let client = reqwest::Client::new();

        stream::iter(pending_downloads)
            .for_each_concurrent(THUMBNAIL_DOWNLOAD_CONCURRENCY, move |(url, path)| {
                let client = client.clone();
                async move {
                    if path.exists() {
                        return;
                    }

                    let response = match client.get(&url).send().await {
                        Ok(response) => response,
                        Err(error) => {
                            warn!("Thumbnail request failed for {}: {}", url, error);
                            return;
                        }
                    };

                    let response = match response.error_for_status() {
                        Ok(response) => response,
                        Err(error) => {
                            warn!("Thumbnail response failed for {}: {}", url, error);
                            return;
                        }
                    };

                    match response.bytes().await {
                        Ok(bytes) => {
                            if let Err(error) = tokio::fs::write(&path, &bytes).await {
                                warn!("Failed writing thumbnail to {}: {}", path.display(), error);
                            }
                        }
                        Err(error) => {
                            warn!("Failed reading thumbnail bytes for {}: {}", url, error);
                        }
                    }
                }
            })
            .await;
    });
}
