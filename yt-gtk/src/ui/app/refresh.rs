use super::cards::populate_flow_box;
use super::{update_watch_later_badge, AppState, UiContext};
use crate::cache::Storage;
use crate::data::{Tab, Video};

use futures::stream::{self, StreamExt};
use log::warn;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

pub(super) fn refresh_video_lists(state_rc: &Rc<RefCell<AppState>>, ui_context: &UiContext) {
    let state_ref = state_rc.borrow();
    let downloaded_video_ids = state_ref.storage.cached_video_ids();
    update_watch_later_badge(&ui_context.badge, state_ref.watch_later.len());
    populate_flow_box(
        &ui_context.feed_flow,
        Tab::Feed,
        &downloaded_video_ids,
        state_rc,
        ui_context,
    );
    populate_flow_box(
        &ui_context.watch_later_flow,
        Tab::WatchLater,
        &downloaded_video_ids,
        state_rc,
        ui_context,
    );
}

pub(super) fn refresh_watch_later_tab(state_rc: &Rc<RefCell<AppState>>, ui_context: &UiContext) {
    let state_ref = state_rc.borrow();
    let downloaded_video_ids = state_ref.storage.cached_video_ids();
    update_watch_later_badge(&ui_context.badge, state_ref.watch_later.len());
    populate_flow_box(
        &ui_context.watch_later_flow,
        Tab::WatchLater,
        &downloaded_video_ids,
        state_rc,
        ui_context,
    );
}

/// Downloads thumbnails missing from local storage.
///
/// # Arguments
///
/// * `videos` - Videos whose thumbnails should be present locally.
/// * `storage` - Storage backend used to resolve thumbnail paths.
/// * `runtime` - Tokio runtime used to execute network and file I/O.
///
/// # Returns
///
/// `Some(receiver)` when at least one thumbnail download was scheduled. The receiver yields once
/// when all scheduled downloads have completed (successfully or not). `None` when there is no
/// work to do.
pub(super) fn download_missing_thumbnails(
    videos: &[Video],
    storage: &Storage,
    client: reqwest::Client,
    runtime: Arc<tokio::runtime::Runtime>,
) -> Option<async_channel::Receiver<()>> {
    const THUMBNAIL_DOWNLOAD_CONCURRENCY: usize = 12;

    let pending_downloads: Vec<(String, PathBuf)> = videos
        .iter()
        .filter_map(|video| {
            let path = storage.thumbnail_path(video.video_id());
            if path.exists() {
                None
            } else {
                Some((video.thumbnail_url().to_string(), path))
            }
        })
        .collect();

    if pending_downloads.is_empty() {
        return None;
    }

    let (completion_tx, completion_rx) = async_channel::bounded(1);
    runtime.spawn(async move {
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

        let _ = completion_tx.send(()).await;
    });

    Some(completion_rx)
}
