mod cards;
mod comments;
mod summary_generator;

use crate::cache::{convert_to_miyoo, download_video, Storage, StorageError};
use crate::data::{Tab, Video};
use crate::feed::{fetch_all_feeds, fetch_youtube_search, load_channel_ids, FetchProgress};
use cards::VideoCardWidgets;

use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Button, FlowBox, Label, Popover, ScrolledWindow, SearchEntry,
    Spinner, Stack,
};
use indexmap::IndexMap;
use log::{error, info, warn};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;
use tokio::runtime::Runtime;

use cards::{
    create_context_menu, download_missing_thumbnails, populate_flow_box,
    refresh_video_downloaded_badge, refresh_video_downloading_badge, sync_watch_later_card,
    update_watch_later_toggles,
};
use summary_generator::{maybe_prefetch_summary_for_watch_later, SummaryGenerator};

struct AppState {
    videos: IndexMap<String, Video>,
    feed_video_ids: Vec<String>,
    search_result_ids: Vec<String>,
    watch_later: HashSet<String>,
    storage: Storage,
}

#[derive(Debug, Error)]
pub(super) enum CacheVideoError {
    #[error("Failed to persist {sidecar_name} sidecar for {video_id}: {source}")]
    Persist {
        video_id: String,
        sidecar_name: &'static str,
        #[source]
        source: StorageError,
    },
    #[error("Video {video_id} is no longer available")]
    MissingVideo { video_id: String },
}

impl AppState {
    fn new(videos: Vec<Video>, watch_later: HashSet<String>, storage: Storage) -> Self {
        let videos = videos
            .into_iter()
            .map(|video| (video.video_id().to_string(), video))
            .collect::<IndexMap<_, _>>();
        let feed_video_ids = videos.keys().cloned().collect();
        Self {
            videos,
            feed_video_ids,
            search_result_ids: Vec::new(),
            watch_later,
            storage,
        }
    }

    fn set_videos(&mut self, videos: Vec<Video>) {
        self.videos = videos
            .into_iter()
            .map(|video| (video.video_id().to_string(), video))
            .collect();
        self.feed_video_ids = self.videos.keys().cloned().collect();
    }

    fn set_search_results(&mut self, videos: Vec<Video>) {
        let mut search_result_ids = Vec::with_capacity(videos.len());
        let mut seen_video_ids = HashSet::with_capacity(videos.len());

        for video in videos {
            let video_id = video.video_id().to_string();
            if seen_video_ids.insert(video_id.clone()) {
                search_result_ids.push(video_id.clone());
            }
            self.videos.insert(video_id, video);
        }

        self.search_result_ids = search_result_ids;
    }

    fn video_by_id(&self, video_id: &str) -> Option<&Video> {
        self.videos.get(video_id)
    }

    fn video_by_id_mut(&mut self, video_id: &str) -> Option<&mut Video> {
        self.videos.get_mut(video_id)
    }

    fn feed_video_ids(&self) -> Vec<String> {
        self.feed_video_ids.clone()
    }

    fn search_video_ids(&self) -> Vec<String> {
        self.search_result_ids.clone()
    }

    fn cache_video_transcript(
        &mut self,
        video_id: &str,
        transcript: String,
    ) -> Result<(), CacheVideoError> {
        self.storage
            .save_video_metadata(video_id, Some(&transcript), None)
            .map_err(|source| CacheVideoError::Persist {
                video_id: video_id.to_string(),
                sidecar_name: "transcript",
                source,
            })?;

        let video =
            self.video_by_id_mut(video_id)
                .ok_or_else(|| CacheVideoError::MissingVideo {
                    video_id: video_id.to_string(),
                })?;
        video.set_transcript(Some(transcript));
        Ok(())
    }

    fn set_video_watched(&mut self, video_id: &str, watched: bool) -> Result<(), CacheVideoError> {
        self.storage
            .save_video_watched(video_id, watched)
            .map_err(|source| CacheVideoError::Persist {
                video_id: video_id.to_string(),
                sidecar_name: "watched",
                source,
            })?;
        let video =
            self.video_by_id_mut(video_id)
                .ok_or_else(|| CacheVideoError::MissingVideo {
                    video_id: video_id.to_string(),
                })?;
        video.set_watched(watched);
        Ok(())
    }

    fn cache_video_ai_summary(
        &mut self,
        video_id: &str,
        ai_summary: String,
    ) -> Result<(), CacheVideoError> {
        self.storage
            .save_video_metadata(video_id, None, Some(&ai_summary))
            .map_err(|source| CacheVideoError::Persist {
                video_id: video_id.to_string(),
                sidecar_name: "summary",
                source,
            })?;

        let video =
            self.video_by_id_mut(video_id)
                .ok_or_else(|| CacheVideoError::MissingVideo {
                    video_id: video_id.to_string(),
                })?;
        video.set_ai_summary(Some(ai_summary));
        Ok(())
    }
}

type CardMap = Rc<RefCell<HashMap<String, VideoCardWidgets>>>;

#[derive(Clone)]
struct AppContext {
    runtime: Arc<Runtime>,
    http_client: reqwest::Client,
    summary_generator: SummaryGenerator,
    window: ApplicationWindow,
    context_menu: Popover,
    feed_flow: FlowBox,
    search_flow: FlowBox,
    watch_later_flow: FlowBox,
    selected_video: Rc<RefCell<Option<String>>>,
    badge: Label,
    feed_cards: CardMap,
    search_cards: CardMap,
    watch_later_cards: CardMap,
    subs_file: PathBuf,
}

const CARD_WIDTH: i32 = 320;
const CARD_SPACING: i32 = 16;
const GRID_PADDING: i32 = 16;
const FROGPOINTS_REFRESH_COST: i64 = 10;
const FROGPOINTS_RELATIVE_PATH: &[&str] = &["Desktop", "RemoteVault", "frogpoints.md"];

#[derive(Debug, Error)]
enum FrogpointsError {
    #[error("HOME is not set")]
    MissingHome,
    #[error("Failed to read frogpoints: {0}")]
    Read(#[source] std::io::Error),
    #[error("frogpoints.md must contain a whole number")]
    InvalidNumber(#[source] std::num::ParseIntError),
    #[error("Need {cost} frogpoints to refresh, but only {available} remain")]
    Insufficient { available: i64, cost: i64 },
    #[error("Failed to save frogpoints: {0}")]
    Write(#[source] std::io::Error),
}

fn is_legacy_download(path: &Path) -> bool {
    !matches!(path.extension().and_then(|ext| ext.to_str()), Some("mkv"))
}

fn needs_download_upgrade(local_path: Option<&Path>) -> bool {
    local_path.map(is_legacy_download).unwrap_or(true)
}

fn frogpoints_path() -> Result<PathBuf, FrogpointsError> {
    let mut path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(FrogpointsError::MissingHome)?;
    path.extend(FROGPOINTS_RELATIVE_PATH);
    Ok(path)
}

fn debit_frogpoints(path: &Path, cost: i64) -> Result<i64, FrogpointsError> {
    let contents = fs::read_to_string(path).map_err(FrogpointsError::Read)?;
    let current = contents
        .trim()
        .parse::<i64>()
        .map_err(FrogpointsError::InvalidNumber)?;

    if current < cost {
        return Err(FrogpointsError::Insufficient {
            available: current,
            cost,
        });
    }

    let remaining = current - cost;
    fs::write(path, format!("{remaining}\n")).map_err(FrogpointsError::Write)?;
    Ok(remaining)
}

fn debit_refresh_frogpoints() -> Result<i64, FrogpointsError> {
    let path = frogpoints_path()?;
    debit_frogpoints(&path, FROGPOINTS_REFRESH_COST)
}

fn spawn_video_download(
    runtime: Arc<Runtime>,
    storage: Storage,
    video_id: String,
    video_path: PathBuf,
    miyoo_path: PathBuf,
) -> async_channel::Receiver<String> {
    let (tx, rx) = async_channel::bounded(1);
    runtime.spawn(async move {
        if let Err(download_error) = download_video(&video_id, &video_path).await {
            error!("Failed to download video {}: {}", video_id, download_error);
        } else {
            let subtitle_path = storage.find_subtitle_path(&video_id);
            if let Err(convert_error) =
                convert_to_miyoo(&video_path, subtitle_path.as_deref(), &miyoo_path).await
            {
                error!(
                    "Failed to convert video {} for miyoo: {}",
                    video_id, convert_error
                );
            }
            let _ = tx.send(video_id.clone()).await;
        }
    });
    rx
}

fn retry_missing_miyoo_conversions(runtime: Arc<Runtime>, storage: Storage) {
    runtime.spawn(async move {
        let storage_for_scan = storage.clone();
        let missing_conversions =
            match tokio::task::spawn_blocking(move || storage_for_scan.missing_miyoo_conversions())
                .await
            {
                Ok(Ok(missing_conversions)) => missing_conversions,
                Ok(Err(scan_error)) => {
                    warn!(
                        "Failed to scan for missing Miyoo conversions: {}",
                        scan_error
                    );
                    return;
                }
                Err(join_error) => {
                    error!("Miyoo conversion scan task failed: {}", join_error);
                    return;
                }
            };

        if missing_conversions.is_empty() {
            return;
        }

        info!(
            "Retrying {} missing Miyoo video conversions",
            missing_conversions.len()
        );
        for (input_path, output_path, video_id) in missing_conversions {
            if output_path.exists() {
                continue;
            }
            let subtitle_path = storage.find_subtitle_path(&video_id);
            if let Err(convert_error) =
                convert_to_miyoo(&input_path, subtitle_path.as_deref(), &output_path).await
            {
                error!(
                    "Failed to retry Miyoo conversion for video {}: {}",
                    video_id, convert_error
                );
            }
        }
    });
}

fn persist_watch_later(runtime: Arc<Runtime>, storage: Storage, watch_later: HashSet<String>) {
    runtime.spawn(async move {
        match tokio::task::spawn_blocking(move || storage.save_watch_later(&watch_later)).await {
            Ok(Ok(())) => {}
            Ok(Err(save_error)) => {
                error!("Failed to persist watch-later list: {}", save_error);
            }
            Err(join_error) => {
                error!("Watch-later persistence task failed: {}", join_error);
            }
        }
    });
}

fn persist_unsubscribe(runtime: Arc<Runtime>, subs_file: PathBuf, channel_id: String) {
    runtime.spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let content = std::fs::read_to_string(&subs_file)?;
            let mut output = content
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    // Keep comments, blank lines, and lines not matching this channel
                    trimmed.starts_with('#') || trimmed.is_empty() || trimmed != channel_id
                })
                .collect::<Vec<_>>()
                .join("\n");
            if content.ends_with('\n') {
                output.push('\n');
            }
            std::fs::write(&subs_file, output)
        })
        .await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(io_error)) => error!("Failed to remove channel from subs file: {}", io_error),
            Err(join_error) => error!("Unsubscribe task panicked: {}", join_error),
        }
    });
}

fn persist_videos(runtime: Arc<Runtime>, storage: Storage, videos: Vec<Video>) {
    runtime.spawn(async move {
        match tokio::task::spawn_blocking(move || storage.save_videos(&videos)).await {
            Ok(Ok(())) => {}
            Ok(Err(save_error)) => {
                error!("Failed to persist refreshed videos cache: {}", save_error);
            }
            Err(join_error) => {
                error!("Video cache persistence task failed: {}", join_error);
            }
        }
    });
}

fn resolve_playback_path(
    storage: &Storage,
    runtime: Arc<Runtime>,
    video_id: &str,
    video_title: &str,
    local_path: Option<PathBuf>,
) -> Option<PathBuf> {
    match local_path {
        Some(path) if is_legacy_download(&path) => {
            // Legacy downloads lack embedded chapter/caption metadata. Upgrade in background
            // but still play the local file.
            let upgraded_path = storage.video_path(video_id, video_title);
            let miyoo_path = storage.miyoo_video_path(video_id, video_title);
            spawn_video_download(
                runtime,
                storage.clone(),
                video_id.to_string(),
                upgraded_path,
                miyoo_path,
            );
            Some(path)
        }
        other => other,
    }
}

fn update_flow_width(flow: &FlowBox, viewport_width: i32) {
    let available_width = (viewport_width - GRID_PADDING * 2).max(1);
    let num_columns = ((available_width + CARD_SPACING) / (CARD_WIDTH + CARD_SPACING)).max(1);
    let optimal_width = num_columns * CARD_WIDTH + (num_columns - 1) * CARD_SPACING;
    flow.set_size_request(optimal_width, -1);
}

fn toggle_watch_later_and_download(
    state_rc: &Rc<RefCell<AppState>>,
    runtime: &Arc<Runtime>,
    video_id: &str,
    video_title: &str,
) -> (bool, Option<async_channel::Receiver<String>>) {
    let (added, download_rx, storage, watch_later_snapshot) = {
        let mut state = state_rc.borrow_mut();
        let added = !state.watch_later.remove(video_id);
        if added {
            state.watch_later.insert(video_id.to_string());
        }

        let local_path = state.storage.find_video_path(video_id);
        let download_rx = if added && needs_download_upgrade(local_path.as_deref()) {
            let video_path = state.storage.video_path(video_id, video_title);
            let miyoo_path = state.storage.miyoo_video_path(video_id, video_title);
            Some(spawn_video_download(
                runtime.clone(),
                state.storage.clone(),
                video_id.to_string(),
                video_path,
                miyoo_path,
            ))
        } else {
            None
        };

        if !added {
            if let Err(remove_error) = state.storage.remove_cached_video_files(video_id) {
                error!(
                    "Failed to remove cached video {}: {}",
                    video_id, remove_error
                );
            }
        }

        (
            added,
            download_rx,
            state.storage.clone(),
            state.watch_later.clone(),
        )
    };

    persist_watch_later(runtime.clone(), storage, watch_later_snapshot);
    (added, download_rx)
}

fn apply_watch_later_action(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: String,
) {
    let video_title = {
        let state = state_rc.borrow();
        state
            .video_by_id(&video_id)
            .map(|video| video.title().to_string())
    };
    let Some(video_title) = video_title else {
        error!("Cannot toggle watch-later for missing video {}", video_id);
        return;
    };

    let (added, download_rx) =
        toggle_watch_later_and_download(state_rc, &ui_context.runtime, &video_id, &video_title);
    update_watch_later_toggles(ui_context, &video_id, added);
    update_watch_later_badge(&ui_context.badge, state_rc.borrow().watch_later.len());
    sync_watch_later_card(state_rc, ui_context, &video_id);
    if added {
        maybe_prefetch_summary_for_watch_later(state_rc, ui_context, &video_id);
        if let Some(rx) = download_rx {
            refresh_video_downloading_badge(ui_context, &video_id);
            let ui_ctx = ui_context.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Ok(completed_video_id) = rx.recv().await {
                    refresh_video_downloaded_badge(&ui_ctx, &completed_video_id);
                }
            });
        }
    }
}

/// Unsubscribes from the channel of the given video after a confirmation dialog.
///
/// Removes the channel ID from the subscriptions file and purges all videos from that channel
/// from the in-memory state and UI. Also removes any affected watch-later entries and persists
/// the updated watch-later list.
///
/// # Arguments
///
/// * `state_rc` - Shared application state.
/// * `ui_context` - UI handle used to parent the dialog and update card lists.
/// * `video_id` - ID of a video belonging to the channel to unsubscribe from.
fn unsubscribe_channel(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: String,
) {
    let channel_info = {
        let state = state_rc.borrow();
        state
            .video_by_id(&video_id)
            .map(|v| (v.channel_id().to_string(), v.channel_name().to_string()))
    };
    let Some((channel_id, channel_name)) = channel_info else {
        error!("Cannot unsubscribe: missing video {}", video_id);
        return;
    };

    let dialog = gtk::MessageDialog::new(
        Some(&ui_context.window),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        gtk::MessageType::Question,
        gtk::ButtonsType::YesNo,
        &format!("Unsubscribe from {}?", channel_name),
    );
    dialog.set_secondary_text(Some(
        "All videos from this channel will be removed from your feed.",
    ));
    let response = dialog.run();
    dialog.close();

    if response != gtk::ResponseType::Yes {
        return;
    }

    // Only persist the change — videos disappear from the feed on next refresh.
    persist_unsubscribe(
        ui_context.runtime.clone(),
        ui_context.subs_file.clone(),
        channel_id,
    );
}

fn spawn_refresh_progress_updates(
    progress_rx: async_channel::Receiver<FetchProgress>,
    spinner: Spinner,
    status_label: Label,
    refresh_button: Button,
) {
    glib::MainContext::default().spawn_local(async move {
        let mut total_channels = 0usize;
        let mut completed_channels = 0usize;
        let mut failed_channels = 0usize;

        while let Ok(progress) = progress_rx.recv().await {
            match progress {
                FetchProgress::Started { total } => {
                    total_channels = total;
                    completed_channels = 0;
                    failed_channels = 0;
                    status_label.set_text(&format!("Fetching feeds (0/{total})..."));
                }
                FetchProgress::ChannelComplete { channel, count } => {
                    completed_channels += 1;
                    let failed = if failed_channels > 0 {
                        format!(", {failed_channels} failed")
                    } else {
                        String::new()
                    };
                    status_label.set_text(&format!(
                        "Fetching feeds ({completed_channels}/{total_channels}{failed})..."
                    ));
                    info!("Fetched {} videos for channel {}", count, channel);
                }
                FetchProgress::RetryScheduled {
                    channel_id,
                    next_attempt,
                    max_attempts,
                    delay_secs,
                    reason,
                } => {
                    status_label.set_text(&format!(
                        "Retrying {} ({}/{}) in {}s...",
                        channel_id, next_attempt, max_attempts, delay_secs
                    ));
                    warn!(
                        "Retrying channel {} ({}/{}) in {}s: {}",
                        channel_id, next_attempt, max_attempts, delay_secs, reason
                    );
                }
                FetchProgress::Error { channel_id, error } => {
                    completed_channels += 1;
                    failed_channels += 1;
                    status_label.set_text(&format!(
                        "Failed {} of {} channels (last: {})",
                        failed_channels, total_channels, channel_id
                    ));
                    error!("Error fetching {}: {}", channel_id, error);
                }
                FetchProgress::Fatal { error } => {
                    spinner.stop();
                    status_label.set_text(&format!("Refresh failed: {}", error));
                    error!("Fatal refresh error: {}", error);
                    break;
                }
                FetchProgress::AllComplete {
                    total_videos,
                    successful_channels,
                    failed_channels: final_failed,
                } => {
                    spinner.stop();
                    if final_failed > 0 {
                        status_label.set_text(&format!(
                            "{} videos loaded ({} channels ok, {} failed)",
                            total_videos, successful_channels, final_failed
                        ));
                    } else {
                        status_label.set_text(&format!("{} videos loaded", total_videos));
                    }
                    break;
                }
            }
        }

        spinner.stop();
        refresh_button.set_sensitive(true);
    });
}

fn spawn_refreshed_videos_apply(
    videos_rx: async_channel::Receiver<Vec<Video>>,
    state_rc: Rc<RefCell<AppState>>,
    ui_context: AppContext,
) {
    glib::MainContext::default().spawn_local(async move {
        if let Ok(mut videos) = videos_rx.recv().await {
            let mut state = state_rc.borrow_mut();
            state.storage.hydrate_videos_from_sidecars(&mut videos);
            let videos_for_persistence = videos.clone();
            let storage_for_persistence = state.storage.clone();
            let runtime_for_persistence = ui_context.runtime.clone();
            state.set_videos(videos);
            persist_videos(
                runtime_for_persistence,
                storage_for_persistence,
                videos_for_persistence,
            );

            let thumbnail_completion = download_missing_thumbnails(
                state.videos.values(),
                &state.storage,
                ui_context.http_client.clone(),
                ui_context.runtime.clone(),
            );
            drop(state);

            refresh_video_lists(&state_rc, &ui_context);

            if let Some(thumbnail_completion) = thumbnail_completion {
                let state2 = state_rc.clone();
                let ui2 = ui_context.clone();
                glib::MainContext::default().spawn_local(async move {
                    if let Ok(video_ids) = thumbnail_completion.recv().await {
                        for video_id in &video_ids {
                            cards::refresh_video_thumbnail(&state2, &ui2, video_id);
                        }
                    }
                });
            }
        }
    });
}

fn refresh_video_lists(state_rc: &Rc<RefCell<AppState>>, ui_context: &AppContext) {
    let state_ref = state_rc.borrow();
    let downloaded_video_ids = state_ref.storage.cached_video_ids();
    update_watch_later_badge(&ui_context.badge, state_ref.watch_later.len());
    populate_flow_box(Tab::Feed, &downloaded_video_ids, state_rc, ui_context);
    populate_flow_box(Tab::Search, &downloaded_video_ids, state_rc, ui_context);
    populate_flow_box(Tab::WatchLater, &downloaded_video_ids, state_rc, ui_context);
}

fn refresh_search_results(state_rc: &Rc<RefCell<AppState>>, ui_context: &AppContext) {
    let downloaded_video_ids = state_rc.borrow().storage.cached_video_ids();
    populate_flow_box(Tab::Search, &downloaded_video_ids, state_rc, ui_context);
}

fn start_youtube_search(
    state_rc: Rc<RefCell<AppState>>,
    ui_context: AppContext,
    spinner: Spinner,
    status_label: Label,
    search_button: Button,
    query: String,
) {
    let query = query.trim().to_string();
    if query.is_empty() {
        state_rc.borrow_mut().set_search_results(Vec::new());
        refresh_search_results(&state_rc, &ui_context);
        status_label.set_text("");
        return;
    }

    search_button.set_sensitive(false);
    spinner.start();
    status_label.set_text(&format!("Searching YouTube for \"{query}\"..."));

    let (results_tx, results_rx) = async_channel::bounded(1);
    let client = ui_context.http_client.clone();
    let runtime = ui_context.runtime.clone();
    runtime.spawn(async move {
        let search_result = fetch_youtube_search(&client, &query).await;
        let _ = results_tx.send(search_result).await;
    });

    glib::MainContext::default().spawn_local(async move {
        match results_rx.recv().await {
            Ok(Ok(mut videos)) => {
                let result_count = videos.len();
                let thumbnail_completion = {
                    let mut state = state_rc.borrow_mut();
                    state.storage.hydrate_videos_from_sidecars(&mut videos);
                    let completion = download_missing_thumbnails(
                        videos.iter(),
                        &state.storage,
                        ui_context.http_client.clone(),
                        ui_context.runtime.clone(),
                    );
                    state.set_search_results(videos);
                    completion
                };

                refresh_search_results(&state_rc, &ui_context);
                status_label.set_text(&format!("{result_count} search results"));

                if let Some(thumbnail_completion) = thumbnail_completion {
                    let state_for_thumbnails = state_rc.clone();
                    let ui_context_for_thumbnails = ui_context.clone();
                    glib::MainContext::default().spawn_local(async move {
                        if let Ok(video_ids) = thumbnail_completion.recv().await {
                            for video_id in &video_ids {
                                cards::refresh_video_thumbnail(
                                    &state_for_thumbnails,
                                    &ui_context_for_thumbnails,
                                    video_id,
                                );
                            }
                        }
                    });
                }
            }
            Ok(Err(error)) => {
                status_label.set_text(&format!("Search failed: {error}"));
                error!("YouTube search failed: {}", error);
            }
            Err(error) => {
                status_label.set_text("Search failed");
                error!("YouTube search result channel closed: {}", error);
            }
        }

        spinner.stop();
        search_button.set_sensitive(true);
    });
}

fn start_feed_refresh(
    state: Rc<RefCell<AppState>>,
    ui_context: AppContext,
    spinner: Spinner,
    status_label: Label,
    refresh_button: Button,
    subs_file: PathBuf,
) {
    refresh_button.set_sensitive(false);
    spinner.start();
    status_label.set_text("Refreshing...");

    match debit_refresh_frogpoints() {
        Ok(remaining) => {
            status_label.set_text(&format!("Refreshing... ({remaining} frogpoints remaining)"));
        }
        Err(error) => {
            spinner.stop();
            status_label.set_text(&format!("Refresh blocked: {error}"));
            refresh_button.set_sensitive(true);
            return;
        }
    }

    let channel_ids = match load_channel_ids(&subs_file) {
        Ok(ids) => ids,
        Err(error) => {
            spinner.stop();
            status_label.set_text(&format!("Error: {}", error));
            refresh_button.set_sensitive(true);
            return;
        }
    };

    let (progress_tx, progress_rx) = async_channel::bounded::<FetchProgress>(100);
    let (videos_tx, videos_rx) = async_channel::bounded::<Vec<Video>>(1);

    let progress_tx_for_errors = progress_tx.clone();
    let fetch_client = ui_context.http_client.clone();
    ui_context.runtime.spawn(async move {
        match fetch_all_feeds(&fetch_client, channel_ids, progress_tx).await {
            Ok(videos) => {
                let _ = videos_tx.send(videos).await;
            }
            Err(error) => {
                let _ = progress_tx_for_errors
                    .send(FetchProgress::Fatal {
                        error: error.to_string(),
                    })
                    .await;
            }
        }
    });

    spawn_refresh_progress_updates(progress_rx, spinner, status_label, refresh_button);
    spawn_refreshed_videos_apply(videos_rx, state, ui_context);
}

/// Builds and presents the primary GTK application window.
///
/// # Arguments
///
/// * `app` - Active GTK application instance.
/// * `subs_file` - Path to the channel subscription file.
pub fn build_ui(app: &Application, subs_file: PathBuf) {
    let runtime = match Runtime::new() {
        Ok(runtime) => Arc::new(runtime),
        Err(runtime_error) => {
            error!("Failed to create tokio runtime: {}", runtime_error);
            return;
        }
    };

    let storage = match Storage::new() {
        Ok(storage) => storage,
        Err(storage_error) => {
            error!("Failed to initialize storage: {}", storage_error);
            return;
        }
    };

    let watch_later = storage.load_watch_later();
    let videos = storage.load_videos();
    let feed_video_ids = videos
        .iter()
        .map(|video| video.video_id().to_string())
        .collect::<HashSet<_>>();

    match storage.cleanup_unreferenced_cache_files(&watch_later, &feed_video_ids) {
        Ok(removed_count) if removed_count > 0 => {
            info!("Removed {} unreferenced cache artifacts", removed_count);
        }
        Ok(_) => {}
        Err(cleanup_error) => {
            warn!(
                "Failed to clean up unreferenced cache artifacts: {}",
                cleanup_error
            );
        }
    }

    retry_missing_miyoo_conversions(runtime.clone(), storage.clone());
    let http_client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(client_error) => {
            error!("Failed to initialize HTTP client: {}", client_error);
            return;
        }
    };

    let state = Rc::new(RefCell::new(AppState::new(videos, watch_later, storage)));
    let selected_video: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("yt-gtk")
        .default_width(1200)
        .default_height(800)
        .build();

    let css_provider = gtk::CssProvider::new();
    if let Err(css_error) = css_provider.load_from_data(include_bytes!("../style.css")) {
        warn!("Failed to load CSS: {}", css_error);
    }
    if let Some(screen) = gdk::Screen::default() {
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    // Load static window structure from .ui file
    let builder = gtk::Builder::from_string(include_str!("window.ui"));
    let header = builder
        .object::<gtk::HeaderBar>("header")
        .expect("header in window.ui");
    let refresh_button = builder
        .object::<Button>("refresh_button")
        .expect("refresh_button in window.ui");
    let status_label = builder
        .object::<Label>("status_label")
        .expect("status_label in window.ui");
    let spinner = builder
        .object::<Spinner>("spinner")
        .expect("spinner in window.ui");
    let feed_tab = builder
        .object::<gtk::ToggleButton>("feed_tab")
        .expect("feed_tab in window.ui");
    let search_tab = builder
        .object::<gtk::ToggleButton>("search_tab")
        .expect("search_tab in window.ui");
    let watch_later_tab = builder
        .object::<gtk::ToggleButton>("watch_later_tab")
        .expect("watch_later_tab in window.ui");
    let badge = builder
        .object::<Label>("watch_later_badge")
        .expect("watch_later_badge in window.ui");
    let stack = builder
        .object::<Stack>("stack")
        .expect("stack in window.ui");
    let feed_scroll = builder
        .object::<ScrolledWindow>("feed_scroll")
        .expect("feed_scroll in window.ui");
    let feed_flow = builder
        .object::<FlowBox>("feed_flow")
        .expect("feed_flow in window.ui");
    let search_entry = builder
        .object::<SearchEntry>("search_entry")
        .expect("search_entry in window.ui");
    let search_button = builder
        .object::<Button>("search_button")
        .expect("search_button in window.ui");
    let search_scroll = builder
        .object::<ScrolledWindow>("search_scroll")
        .expect("search_scroll in window.ui");
    let search_flow = builder
        .object::<FlowBox>("search_flow")
        .expect("search_flow in window.ui");
    let watch_later_scroll = builder
        .object::<ScrolledWindow>("watch_later_scroll")
        .expect("watch_later_scroll in window.ui");
    let watch_later_flow = builder
        .object::<FlowBox>("watch_later_flow")
        .expect("watch_later_flow in window.ui");

    window.set_titlebar(Some(&header));
    window.add(&stack);

    // Resize flow columns when scroll viewport changes
    {
        let feed_flow = feed_flow.clone();
        feed_scroll.connect_size_allocate(move |_, allocation| {
            update_flow_width(&feed_flow, allocation.width());
        });
    }
    {
        let watch_later_flow = watch_later_flow.clone();
        watch_later_scroll.connect_size_allocate(move |_, allocation| {
            update_flow_width(&watch_later_flow, allocation.width());
        });
    }
    {
        let search_flow = search_flow.clone();
        search_scroll.connect_size_allocate(move |_, allocation| {
            update_flow_width(&search_flow, allocation.width());
        });
    }

    // Tab toggle buttons switch the stack page and update sibling active state
    {
        let stack = stack.clone();
        let search_tab = search_tab.clone();
        let watch_later_tab = watch_later_tab.clone();
        let feed_flow = feed_flow.clone();
        feed_tab.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            stack.set_visible_child_name("feed");
            search_tab.set_active(false);
            watch_later_tab.set_active(false);
            update_flow_width(&feed_flow, stack.allocated_width());
        });
    }
    {
        let stack = stack.clone();
        let feed_tab = feed_tab.clone();
        let search_tab = search_tab.clone();
        let watch_later_flow = watch_later_flow.clone();
        watch_later_tab.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            stack.set_visible_child_name("watch-later");
            feed_tab.set_active(false);
            search_tab.set_active(false);
            update_flow_width(&watch_later_flow, stack.allocated_width());
        });
    }
    {
        let stack = stack.clone();
        let feed_tab = feed_tab.clone();
        let watch_later_tab = watch_later_tab.clone();
        let search_entry = search_entry.clone();
        let search_flow = search_flow.clone();
        search_tab.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            stack.set_visible_child_name("search");
            feed_tab.set_active(false);
            watch_later_tab.set_active(false);
            update_flow_width(&search_flow, stack.allocated_width());
            search_entry.grab_focus();
        });
    }

    let feed_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let search_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let watch_later_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let context_menu = Popover::new(None::<&gtk::Widget>);

    let ui_context = AppContext {
        summary_generator: SummaryGenerator::new(runtime.clone(), http_client.clone()),
        runtime: runtime.clone(),
        http_client: http_client.clone(),
        window: window.clone(),
        context_menu: context_menu.clone(),
        feed_flow: feed_flow.clone(),
        search_flow: search_flow.clone(),
        watch_later_flow: watch_later_flow.clone(),
        selected_video: selected_video.clone(),
        badge: badge.clone(),
        feed_cards: feed_cards.clone(),
        search_cards: search_cards.clone(),
        watch_later_cards: watch_later_cards.clone(),
        subs_file: subs_file.clone(),
    };
    create_context_menu(&context_menu, state.clone(), &ui_context);

    refresh_video_lists(&state, &ui_context);

    {
        let state = state.clone();
        let ui_context = ui_context.clone();
        let spinner = spinner.clone();
        let status_label = status_label.clone();
        let search_button = search_button.clone();
        search_entry.connect_activate(move |entry| {
            start_youtube_search(
                state.clone(),
                ui_context.clone(),
                spinner.clone(),
                status_label.clone(),
                search_button.clone(),
                entry.text().to_string(),
            );
        });
    }
    {
        let state = state.clone();
        let ui_context = ui_context.clone();
        let spinner = spinner.clone();
        let status_label = status_label.clone();
        let search_entry = search_entry.clone();
        search_button.connect_clicked(move |button| {
            start_youtube_search(
                state.clone(),
                ui_context.clone(),
                spinner.clone(),
                status_label.clone(),
                button.clone(),
                search_entry.text().to_string(),
            );
        });
    }

    {
        let state = state.clone();
        let status_label = status_label.clone();
        let spinner = spinner.clone();
        let ui_context = ui_context.clone();
        let subs_file = subs_file.clone();
        let refresh_button_for_handler = refresh_button.clone();
        refresh_button.connect_clicked(move |_| {
            start_feed_refresh(
                state.clone(),
                ui_context.clone(),
                spinner.clone(),
                status_label.clone(),
                refresh_button_for_handler.clone(),
                subs_file.clone(),
            );
        });
    }

    let startup_thumbnail_completion = {
        let state_ref = state.borrow();
        download_missing_thumbnails(
            state_ref.videos.values(),
            &state_ref.storage,
            ui_context.http_client.clone(),
            ui_context.runtime.clone(),
        )
    };
    if let Some(startup_thumbnail_completion) = startup_thumbnail_completion {
        let state_for_startup = state.clone();
        let ui_context_for_startup = ui_context.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Ok(video_ids) = startup_thumbnail_completion.recv().await {
                for video_id in &video_ids {
                    cards::refresh_video_thumbnail(
                        &state_for_startup,
                        &ui_context_for_startup,
                        video_id,
                    );
                }
            }
        });
    }

    window.show_all();
}

fn update_watch_later_badge(badge: &Label, count: usize) {
    if count > 0 {
        badge.set_text(&count.to_string());
        badge.show();
    } else {
        badge.hide();
    }
}

#[cfg(test)]
mod tests {
    use super::{debit_frogpoints, AppState, FrogpointsError, FROGPOINTS_REFRESH_COST};
    use crate::cache::Storage;
    use crate::data::Video;
    use chrono::{TimeZone, Utc};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR_ID: AtomicU64 = AtomicU64::new(0);

    fn temporary_frogpoints_path(test_name: &str) -> PathBuf {
        let unique_id = NEXT_TEST_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "yt-gtk-{test_name}-{}-{unique_id}.md",
            std::process::id()
        ))
    }

    struct TestDirs {
        root: PathBuf,
    }

    impl TestDirs {
        fn new() -> Self {
            let unique_id = NEXT_TEST_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "yt-gtk-app-state-tests-{}-{}",
                std::process::id(),
                unique_id
            ));
            std::fs::create_dir_all(&root).expect("test directory must be creatable");
            Self { root }
        }

        fn data_dir(&self) -> PathBuf {
            self.root.join("data")
        }

        fn cache_dir(&self) -> PathBuf {
            self.root.join("cache")
        }
    }

    impl Drop for TestDirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn test_storage(dirs: &TestDirs) -> Storage {
        Storage::new_at(dirs.data_dir(), dirs.cache_dir()).expect("test storage must initialize")
    }

    #[test]
    fn debit_frogpoints_subtracts_cost_and_returns_remaining_balance() {
        let path = temporary_frogpoints_path("debit");
        std::fs::write(&path, "18").expect("write test frogpoints file");

        let remaining =
            debit_frogpoints(&path, FROGPOINTS_REFRESH_COST).expect("debit enough frogpoints");

        assert_eq!(remaining, 8);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read updated frogpoints file"),
            "8\n"
        );

        std::fs::remove_file(path).expect("remove test frogpoints file");
    }

    #[test]
    fn debit_frogpoints_blocks_when_balance_is_too_small() {
        let path = temporary_frogpoints_path("block");
        std::fs::write(&path, "9").expect("write test frogpoints file");

        let error = debit_frogpoints(&path, FROGPOINTS_REFRESH_COST)
            .expect_err("block insufficient frogpoints");

        assert!(matches!(
            error,
            FrogpointsError::Insufficient {
                available: 9,
                cost: FROGPOINTS_REFRESH_COST
            }
        ));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read unchanged frogpoints file"),
            "9"
        );

        std::fs::remove_file(path).expect("remove test frogpoints file");
    }

    fn test_video(video_id: &str) -> Video {
        test_video_with_metadata(video_id, "channel-name", &format!("title-{video_id}"))
    }

    fn test_video_with_metadata(video_id: &str, channel_name: &str, title: &str) -> Video {
        let published = Utc
            .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
            .single()
            .expect("valid fixed test timestamp");

        Video::new(
            video_id.to_string(),
            "channel-id".to_string(),
            channel_name.to_string(),
            title.to_string(),
            published,
            "https://example.com/thumb.jpg".to_string(),
            None,
        )
    }

    #[test]
    fn app_state_new_keeps_insertion_order() {
        let dirs = TestDirs::new();
        let state = AppState::new(
            vec![test_video("a"), test_video("b"), test_video("c")],
            HashSet::new(),
            test_storage(&dirs),
        );
        let ids = state.videos.keys().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn app_state_set_videos_deduplicates_by_video_id() {
        let dirs = TestDirs::new();
        let mut state = AppState::new(Vec::new(), HashSet::new(), test_storage(&dirs));
        state.set_videos(vec![test_video("dup"), test_video("x"), test_video("dup")]);
        let ids = state.videos.keys().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(ids, vec!["dup", "x"]);
        assert_eq!(state.feed_video_ids(), vec!["dup", "x"]);
        assert_eq!(state.videos.len(), 2);
    }

    #[test]
    fn app_state_search_results_do_not_change_feed_ids() {
        let dirs = TestDirs::new();
        let mut state = AppState::new(
            vec![test_video("feed-a"), test_video("feed-b")],
            HashSet::new(),
            test_storage(&dirs),
        );

        state.set_search_results(vec![test_video("search-a"), test_video("feed-a")]);

        assert_eq!(state.feed_video_ids(), vec!["feed-a", "feed-b"]);
        assert_eq!(state.search_video_ids(), vec!["search-a", "feed-a"]);
        assert!(state.video_by_id("search-a").is_some());
    }
}
