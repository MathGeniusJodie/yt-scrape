use super::cards::{
    populate_flow_box, refresh_video_summary_badges as update_video_summary_badges,
    refresh_video_thumbnail, sync_watch_later_card as sync_single_watch_later_card,
    update_watch_later_toggles as set_watch_later_toggles,
};
use super::{update_watch_later_badge, AppContext, AppState};
use crate::cache::Storage;
use crate::data::{Tab, Video};

use futures::stream::{self, StreamExt};
use log::warn;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

fn refresh_tab(
    tab: Tab,
    downloaded_video_ids: &HashSet<String>,
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
) {
    populate_flow_box(tab, downloaded_video_ids, state_rc, ui_context);
}

pub(super) fn refresh_video_lists(state_rc: &Rc<RefCell<AppState>>, ui_context: &AppContext) {
    let state_ref = state_rc.borrow();
    let downloaded_video_ids = state_ref.storage.cached_video_ids();
    update_watch_later_badge(&ui_context.badge, state_ref.watch_later.len());
    refresh_tab(Tab::Feed, &downloaded_video_ids, state_rc, ui_context);
    refresh_tab(Tab::WatchLater, &downloaded_video_ids, state_rc, ui_context);
}

pub(super) fn update_watch_later_toggles(
    ui_context: &AppContext,
    video_id: &str,
    is_watch_later: bool,
) {
    set_watch_later_toggles(ui_context, video_id, is_watch_later);
}

pub(super) fn sync_watch_later_card(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let watch_later_count = state_rc.borrow().watch_later.len();
    update_watch_later_badge(&ui_context.badge, watch_later_count);
    sync_single_watch_later_card(state_rc, ui_context, video_id);
}

pub(super) fn refresh_video_summary_badges(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let has_summary = state_rc
        .borrow()
        .video_by_id(video_id)
        .is_some_and(Video::has_ai_summary);
    update_video_summary_badges(ui_context, video_id, has_summary);
}

pub(super) fn refresh_video_thumbnails(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_ids: &[String],
) {
    for video_id in video_ids {
        refresh_video_thumbnail(state_rc, ui_context, video_id);
    }
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
/// with the downloaded video's IDs when all scheduled downloads have completed (successfully or
/// not). `None` when there is no work to do.
pub(super) fn download_missing_thumbnails<'a>(
    videos: impl IntoIterator<Item = &'a Video>,
    storage: &Storage,
    client: reqwest::Client,
    runtime: Arc<tokio::runtime::Runtime>,
) -> Option<async_channel::Receiver<Vec<String>>> {
    const THUMBNAIL_DOWNLOAD_CONCURRENCY: usize = 12;

    let pending_downloads: Vec<(String, String, PathBuf)> = videos
        .into_iter()
        .filter_map(|video| {
            let path = storage.thumbnail_path(video.video_id());
            if path.exists() {
                None
            } else {
                Some((
                    video.video_id().to_string(),
                    video.thumbnail_url().to_string(),
                    path,
                ))
            }
        })
        .collect();

    if pending_downloads.is_empty() {
        return None;
    }

    let (completion_tx, completion_rx) = async_channel::bounded(1);
    let pending_video_ids = pending_downloads
        .iter()
        .map(|(video_id, _, _)| video_id.clone())
        .collect::<Vec<_>>();
    runtime.spawn(async move {
        stream::iter(pending_downloads)
            .for_each_concurrent(
                THUMBNAIL_DOWNLOAD_CONCURRENCY,
                move |(_video_id, url, path)| {
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
                                    warn!(
                                        "Failed writing thumbnail to {}: {}",
                                        path.display(),
                                        error
                                    );
                                }
                            }
                            Err(error) => {
                                warn!("Failed reading thumbnail bytes for {}: {}", url, error);
                            }
                        }
                    }
                },
            )
            .await;

        let _ = completion_tx.send(pending_video_ids).await;
    });

    Some(completion_rx)
}
