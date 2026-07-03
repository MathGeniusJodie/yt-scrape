mod cards;
mod comments;
mod summary_generator;

use crate::cache::{Storage, StorageError, download_video};
use crate::data::{Tab, Video};
use crate::feed::{FetchProgress, fetch_all_feeds, fetch_youtube_search, load_channel_ids};
use crate::frogpoints;
use cards::TabCards;

use adw::prelude::*;
use gtk::{Button, FlowBox, Label, Popover, Spinner};
use gtk::{gdk, glib};
use indexmap::IndexMap;
use log::{error, info, warn};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;
use tokio::runtime::Runtime;

use cards::{
    create_context_menu, download_missing_thumbnails, populate_flow_box,
    refresh_video_download_failed_badge, refresh_video_downloaded_badge,
    refresh_video_downloading_badge, sync_watch_later_card, update_watch_later_toggles,
};
use summary_generator::{SummaryGenerator, maybe_prefetch_summary_for_watch_later};

struct AppState {
    videos: IndexMap<String, Video>,
    feed_video_ids: Vec<String>,
    search_result_ids: Vec<String>,
    watch_later: HashSet<String>,
    /// IDs with a completed local download, kept in sync with the videos cache
    /// directory so UI paths never rescan it on the main thread.
    downloaded_video_ids: HashSet<String>,
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
    fn new(
        videos: Vec<Video>,
        feed_video_ids: Vec<String>,
        watch_later: HashSet<String>,
        storage: Storage,
    ) -> Self {
        let videos = videos
            .into_iter()
            .map(|video| (video.video_id().to_string(), video))
            .collect::<IndexMap<_, _>>();
        let downloaded_video_ids = storage.cached_video_ids();
        Self {
            videos,
            feed_video_ids,
            search_result_ids: Vec::new(),
            watch_later,
            downloaded_video_ids,
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

    fn set_refreshed_feed_videos(&mut self, videos: Vec<Video>) {
        let incoming_video_id_set: HashSet<String> = videos
            .iter()
            .map(|video| video.video_id().to_string())
            .collect();
        #[allow(clippy::needless_collect)]
        let preserved_watch_later_videos: Vec<_> = self
            .videos
            .iter()
            .filter(|(video_id, _)| {
                self.watch_later.contains(video_id.as_str())
                    && !incoming_video_id_set.contains(*video_id)
            })
            .map(|(_, video)| video.clone())
            .collect();

        let feed_video_ids: Vec<String> = videos
            .iter()
            .map(|video| video.video_id().to_string())
            .collect();

        self.set_videos(videos);
        self.videos.extend(
            preserved_watch_later_videos
                .into_iter()
                .map(|video| (video.video_id().to_string(), video)),
        );
        self.feed_video_ids = feed_video_ids;
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

    fn feed_video_ids(&self) -> &[String] {
        &self.feed_video_ids
    }

    fn search_video_ids(&self) -> &[String] {
        &self.search_result_ids
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

type CardMap = Rc<RefCell<TabCards>>;

#[derive(Clone)]
struct AppContext {
    runtime: Arc<Runtime>,
    http_client: reqwest::Client,
    summary_generator: SummaryGenerator,
    window: adw::ApplicationWindow,
    context_menu: Popover,
    stack: adw::ViewStack,
    watch_later_page: adw::ViewStackPage,
    feed_flow: FlowBox,
    search_flow: FlowBox,
    watch_later_flow: FlowBox,
    selected_video: Rc<RefCell<Option<String>>>,
    feed_cards: CardMap,
    search_cards: CardMap,
    watch_later_cards: CardMap,
    /// Ordered queue for watch-later persistence (see [`spawn_watch_later_persister`]).
    watch_later_saves: async_channel::Sender<HashSet<String>>,
    /// Ordered queue for subscription removals (see [`spawn_subscription_remover`]).
    subscription_removals: async_channel::Sender<String>,
    /// Video IDs with an in-flight yt-dlp download, so duplicate downloads are
    /// never spawned and file deletion is deferred until the download finishes.
    downloads_in_progress: Rc<RefCell<HashSet<String>>>,
    /// Video IDs with a running mpv session, so accidental double-plays never
    /// spawn a second player racing the watched-state heuristic.
    videos_playing: Rc<RefCell<HashSet<String>>>,
}

/// Toolbar widgets that report long-running feed/search activity.
#[derive(Clone)]
struct StatusUi {
    spinner: Spinner,
    label: Label,
    /// Button disabled while the activity runs.
    button: Button,
}

impl StatusUi {
    /// Stops the spinner, shows `message`, and re-enables the action button.
    fn finish(&self, message: &str) {
        self.spinner.stop();
        self.label.set_text(message);
        self.button.set_sensitive(true);
    }
}

fn is_legacy_download(path: &Path) -> bool {
    !matches!(path.extension().and_then(|ext| ext.to_str()), Some("mkv"))
}

fn needs_download_upgrade(local_path: Option<&Path>) -> bool {
    local_path.is_none_or(is_legacy_download)
}

/// Spawns a background video download, reporting completion (`true`) or failure
/// (`false`) so the UI can always clear the downloading spinner.
fn spawn_video_download(
    runtime: &Arc<Runtime>,
    video_id: String,
    video_path: PathBuf,
) -> async_channel::Receiver<bool> {
    let (tx, rx) = async_channel::bounded(1);
    runtime.spawn(async move {
        let succeeded = match download_video(&video_id, &video_path).await {
            Ok(()) => true,
            Err(download_error) => {
                error!("Failed to download video {video_id}: {download_error}");
                false
            }
        };
        let _ = tx.send(succeeded).await;
    });
    rx
}

/// Starts a download for `video_id` unless one is already in flight, showing the
/// downloading spinner and handling all completion bookkeeping (badges, the
/// downloaded-ID set, and deferred deletion when the video was removed from
/// Watch Later mid-download).
fn start_tracked_download(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
    video_title: &str,
) {
    if !ui_context
        .downloads_in_progress
        .borrow_mut()
        .insert(video_id.to_string())
    {
        return;
    }

    let video_path = state_rc.borrow().storage.video_path(video_id, video_title);
    let completion_rx = spawn_video_download(&ui_context.runtime, video_id.to_string(), video_path);
    refresh_video_downloading_badge(ui_context, video_id);

    let state_rc = state_rc.clone();
    let ui_context = ui_context.clone();
    let video_id = video_id.to_string();
    glib::MainContext::default().spawn_local(async move {
        // A closed channel means the download task died; treat as failure so
        // the spinner never spins forever.
        let succeeded = completion_rx.recv().await == Ok(true);
        ui_context
            .downloads_in_progress
            .borrow_mut()
            .remove(&video_id);

        let still_wanted = {
            let mut state = state_rc.borrow_mut();
            if succeeded {
                state.downloaded_video_ids.insert(video_id.clone());
                // A legacy-format upgrade leaves the old .mp4/.webm behind;
                // delete it so it can never shadow the fresh .mkv.
                if let Err(remove_error) = state.storage.remove_legacy_video_files(&video_id) {
                    error!("Failed to remove legacy video files for {video_id}: {remove_error}");
                }
            }
            let still_wanted = state.watch_later.contains(&video_id);
            // Removed from Watch Later mid-download: honor the removal now
            // that yt-dlp is no longer writing the files.
            if !still_wanted {
                state.downloaded_video_ids.remove(&video_id);
                if let Err(remove_error) = state.storage.remove_cached_video_files(&video_id) {
                    error!("Failed to remove cached video {video_id}: {remove_error}");
                }
            }
            still_wanted
        };

        if succeeded && still_wanted {
            refresh_video_downloaded_badge(&ui_context, &video_id);
        } else {
            refresh_video_download_failed_badge(&ui_context, &video_id);
        }
    });
}

/// Runs a blocking persistence task on the runtime, logging failures under `description`.
fn persist_in_background<E, F>(runtime: &Arc<Runtime>, description: &'static str, task: F)
where
    E: std::fmt::Display + Send + 'static,
    F: FnOnce() -> Result<(), E> + Send + 'static,
{
    runtime.spawn(async move {
        match tokio::task::spawn_blocking(task).await {
            Ok(Ok(())) => {}
            Ok(Err(save_error)) => error!("Failed to persist {description}: {save_error}"),
            Err(join_error) => error!("{description} persistence task failed: {join_error}"),
        }
    });
}

/// Starts the single consumer task that persists watch-later snapshots strictly
/// in the order they were produced, so rapid toggles can never be clobbered by
/// an older snapshot finishing last.
fn spawn_watch_later_persister(
    runtime: &Arc<Runtime>,
    storage: Storage,
) -> async_channel::Sender<HashSet<String>> {
    let (tx, rx) = async_channel::unbounded::<HashSet<String>>();
    runtime.spawn(async move {
        while let Ok(mut snapshot) = rx.recv().await {
            // Coalesce queued snapshots; only the newest needs to hit disk.
            while let Ok(newer_snapshot) = rx.try_recv() {
                snapshot = newer_snapshot;
            }
            let storage = storage.clone();
            let save_result =
                tokio::task::spawn_blocking(move || storage.save_watch_later(&snapshot)).await;
            match save_result {
                Ok(Ok(())) => {}
                Ok(Err(save_error)) => error!("Failed to persist watch-later list: {save_error}"),
                Err(join_error) => error!("Watch-later persistence task failed: {join_error}"),
            }
        }
    });
    tx
}

fn remove_channel_from_subs_file(subs_file: &Path, channel_id: &str) -> std::io::Result<()> {
    let content = std::fs::read_to_string(subs_file)?;
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
    std::fs::write(subs_file, output)
}

/// Starts the single consumer that rewrites the subscriptions file strictly in
/// request order, so rapid unsubscribes can never interleave their
/// read-modify-write cycles and resurrect a removed channel.
fn spawn_subscription_remover(
    runtime: &Arc<Runtime>,
    subs_file: PathBuf,
) -> async_channel::Sender<String> {
    let (tx, rx) = async_channel::unbounded::<String>();
    runtime.spawn(async move {
        while let Ok(channel_id) = rx.recv().await {
            let subs_file = subs_file.clone();
            let remove_result = tokio::task::spawn_blocking(move || {
                remove_channel_from_subs_file(&subs_file, &channel_id)
            })
            .await;
            match remove_result {
                Ok(Ok(())) => {}
                Ok(Err(save_error)) => {
                    error!("Failed to persist subscription removal: {save_error}");
                }
                Err(join_error) => error!("Subscription removal task failed: {join_error}"),
            }
        }
    });
    tx
}

fn persist_videos(runtime: &Arc<Runtime>, storage: Storage, videos: Vec<Video>) {
    persist_in_background(runtime, "refreshed videos cache", move || {
        storage.save_videos(&videos)
    });
}

fn persist_feed_video_ids(runtime: &Arc<Runtime>, storage: Storage, video_ids: Vec<String>) {
    persist_in_background(runtime, "refreshed feed video IDs", move || {
        storage.save_feed_video_ids(&video_ids)
    });
}

fn resolve_playback_path(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
    video_title: &str,
) -> Option<PathBuf> {
    let (local_path, in_watch_later) = {
        let state = state_rc.borrow();
        (
            state.storage.find_video_path(video_id),
            state.watch_later.contains(video_id),
        )
    };
    match local_path {
        Some(path) if is_legacy_download(&path) => {
            // Legacy downloads lack embedded chapter/caption metadata. Upgrade in
            // background — but only for Watch Later videos, since the cache policy
            // deletes downloads of anything else — and still play the local file.
            if in_watch_later {
                start_tracked_download(state_rc, ui_context, video_id, video_title);
            }
            Some(path)
        }
        other => other,
    }
}

fn perform_watch_later_toggle(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
    video_title: &str,
) {
    let (added, needs_download, watch_later_snapshot) = {
        let mut state = state_rc.borrow_mut();
        let added = !state.watch_later.remove(video_id);
        if added {
            state.watch_later.insert(video_id.to_string());
        }

        let needs_download = added && {
            let local_path = state.storage.find_video_path(video_id);
            needs_download_upgrade(local_path.as_deref())
        };

        // When a download is in flight its completion handler owns the files
        // and performs the deferred deletion; deleting here would race yt-dlp.
        if !added && !ui_context.downloads_in_progress.borrow().contains(video_id) {
            state.downloaded_video_ids.remove(video_id);
            if let Err(remove_error) = state.storage.remove_cached_video_files(video_id) {
                error!("Failed to remove cached video {video_id}: {remove_error}");
            }
        }

        (added, needs_download, state.watch_later.clone())
    };

    if ui_context
        .watch_later_saves
        .send_blocking(watch_later_snapshot)
        .is_err()
    {
        error!("Watch-later persister is gone; changes will not be saved");
    }

    update_watch_later_toggles(ui_context, video_id, added);
    update_watch_later_badge(
        &ui_context.watch_later_page,
        state_rc.borrow().watch_later.len(),
    );
    sync_watch_later_card(state_rc, ui_context, video_id);
    if added {
        maybe_prefetch_summary_for_watch_later(state_rc, ui_context, video_id);
        if needs_download {
            start_tracked_download(state_rc, ui_context, video_id, video_title);
        }
    }
}

/// Asks for confirmation before a Watch Later removal that would delete
/// downloaded media, then performs the toggle on confirmation.
fn confirm_watch_later_removal(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
    video_title: &str,
) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Remove \"{video_title}\" from Watch Later?")),
        Some("The downloaded video and subtitles will be deleted."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove and Delete")]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let state_rc = state_rc.clone();
    let ui_context_for_response = ui_context.clone();
    let video_id = video_id.to_string();
    let video_title = video_title.to_string();
    dialog.connect_response(Some("remove"), move |_, _| {
        // Re-check before acting: the toggle below is a blind toggle, and the
        // video may already have been removed (e.g. via another card's button)
        // while this dialog was open — confirming would otherwise re-ADD it.
        if !state_rc.borrow().watch_later.contains(&video_id) {
            return;
        }
        perform_watch_later_toggle(&state_rc, &ui_context_for_response, &video_id, &video_title);
    });
    dialog.present(Some(&ui_context.window));
}

fn apply_watch_later_action(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let (video_title, removal_deletes_files) = {
        let state = state_rc.borrow();
        let Some(video) = state.video_by_id(video_id) else {
            error!("Cannot toggle watch-later for missing video {video_id}");
            return;
        };
        let removing = state.watch_later.contains(video_id);
        let has_local_files = state.downloaded_video_ids.contains(video_id)
            || ui_context.downloads_in_progress.borrow().contains(video_id);
        (video.title().to_string(), removing && has_local_files)
    };

    if removal_deletes_files {
        confirm_watch_later_removal(state_rc, ui_context, video_id, &video_title);
    } else {
        perform_watch_later_toggle(state_rc, ui_context, video_id, &video_title);
    }
}

/// Unsubscribes from the channel of the given video after a confirmation dialog.
///
/// Removes the channel ID from the subscriptions file. Videos from the channel disappear
/// from the feed on the next refresh.
///
/// # Arguments
///
/// * `state_rc` - Shared application state.
/// * `ui_context` - UI handle used to parent the dialog.
/// * `video_id` - ID of a video belonging to the channel to unsubscribe from.
fn unsubscribe_channel(state_rc: &Rc<RefCell<AppState>>, ui_context: &AppContext, video_id: &str) {
    let channel_info = {
        let state = state_rc.borrow();
        state
            .video_by_id(video_id)
            .map(|v| (v.channel_id().to_string(), v.channel_name().to_string()))
    };
    let Some((channel_id, channel_name)) = channel_info else {
        error!("Cannot unsubscribe: missing video {video_id}");
        return;
    };

    let dialog = adw::AlertDialog::new(
        Some(&format!("Unsubscribe from {channel_name}?")),
        Some("All videos from this channel will be removed from your feed."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("unsubscribe", "Unsubscribe")]);
    dialog.set_response_appearance("unsubscribe", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let subscription_removals = ui_context.subscription_removals.clone();
    dialog.connect_response(Some("unsubscribe"), move |_, _| {
        // Only persist the change — videos disappear from the feed on next refresh.
        if subscription_removals
            .send_blocking(channel_id.clone())
            .is_err()
        {
            error!("Subscription remover is gone; unsubscribe will not be saved");
        }
    });
    dialog.present(Some(&ui_context.window));
}

/// Refunds the refresh cost off the main thread after a fatal refresh failure.
fn refund_refresh_frogpoints_in_background() {
    std::thread::spawn(|| match frogpoints::refund_refresh_frogpoints() {
        Ok(remaining) => {
            info!("Refunded refresh frogpoints after fatal error; {remaining} remaining");
        }
        Err(refund_error) => warn!("Failed to refund refresh frogpoints: {refund_error}"),
    });
}

fn spawn_refresh_progress_updates(
    progress_rx: async_channel::Receiver<FetchProgress>,
    status_ui: StatusUi,
) {
    let StatusUi {
        spinner,
        label: status_label,
        button: refresh_button,
    } = status_ui;
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
                    info!("Fetched {count} videos for channel {channel}");
                }
                FetchProgress::RetryScheduled {
                    channel_id,
                    next_attempt,
                    max_attempts,
                    delay_secs,
                    reason,
                } => {
                    status_label.set_text(&format!(
                        "Retrying {channel_id} ({next_attempt}/{max_attempts}) in {delay_secs}s..."
                    ));
                    warn!(
                        "Retrying channel {channel_id} ({next_attempt}/{max_attempts}) in {delay_secs}s: {reason}"
                    );
                }
                FetchProgress::Error { channel_id, error } => {
                    completed_channels += 1;
                    failed_channels += 1;
                    status_label.set_text(&format!(
                        "Failed {failed_channels} of {total_channels} channels (last: {channel_id})"
                    ));
                    error!("Error fetching {channel_id}: {error}");
                }
                FetchProgress::Fatal { error } => {
                    spinner.stop();
                    status_label.set_text(&format!("Refresh failed (frogpoints refunded): {error}"));
                    error!("Fatal refresh error: {error}");
                    refund_refresh_frogpoints_in_background();
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
                            "{total_videos} videos loaded ({successful_channels} channels ok, {final_failed} failed)"
                        ));
                    } else {
                        status_label.set_text(&format!("{total_videos} videos loaded"));
                    }
                    break;
                }
            }
        }

        spinner.stop();
        refresh_button.set_sensitive(true);
    });
}

/// Refreshes card thumbnails on the main loop once background downloads complete.
fn spawn_thumbnail_refreshes(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    completion_rx: Option<async_channel::Receiver<Vec<String>>>,
) {
    let Some(completion_rx) = completion_rx else {
        return;
    };
    let state_rc = state_rc.clone();
    let ui_context = ui_context.clone();
    glib::MainContext::default().spawn_local(async move {
        if let Ok(video_ids) = completion_rx.recv().await {
            for video_id in &video_ids {
                cards::refresh_video_thumbnail(&state_rc, &ui_context, video_id);
            }
        }
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
            let feed_video_ids = videos
                .iter()
                .map(|video| video.video_id().to_string())
                .collect::<Vec<_>>();
            state.set_refreshed_feed_videos(videos);
            let all_videos = state.videos.values().cloned().collect::<Vec<_>>();
            persist_videos(&ui_context.runtime, state.storage.clone(), all_videos);
            persist_feed_video_ids(&ui_context.runtime, state.storage.clone(), feed_video_ids);

            let thumbnail_completion = download_missing_thumbnails(
                state.videos.values(),
                &state.storage,
                ui_context.http_client.clone(),
                &ui_context.runtime,
            );
            drop(state);

            refresh_video_lists(&state_rc, &ui_context);
            spawn_thumbnail_refreshes(&state_rc, &ui_context, thumbnail_completion);
        }
    });
}

fn refresh_video_lists(state_rc: &Rc<RefCell<AppState>>, ui_context: &AppContext) {
    update_watch_later_badge(
        &ui_context.watch_later_page,
        state_rc.borrow().watch_later.len(),
    );
    populate_flow_box(Tab::Feed, state_rc, ui_context);
    populate_flow_box(Tab::Search, state_rc, ui_context);
    populate_flow_box(Tab::WatchLater, state_rc, ui_context);
}

fn refresh_search_results(state_rc: &Rc<RefCell<AppState>>, ui_context: &AppContext) {
    populate_flow_box(Tab::Search, state_rc, ui_context);
}

fn start_youtube_search(
    state_rc: Rc<RefCell<AppState>>,
    ui_context: AppContext,
    status_ui: StatusUi,
    query: &str,
) {
    let query = query.trim().to_string();
    if query.is_empty() {
        state_rc.borrow_mut().set_search_results(Vec::new());
        refresh_search_results(&state_rc, &ui_context);
        status_ui.label.set_text("");
        return;
    }

    status_ui.button.set_sensitive(false);
    status_ui.spinner.start();
    status_ui
        .label
        .set_text(&format!("Searching YouTube for \"{query}\"..."));

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
                        &ui_context.runtime,
                    );
                    state.set_search_results(videos);
                    completion
                };

                refresh_search_results(&state_rc, &ui_context);
                status_ui
                    .label
                    .set_text(&format!("{result_count} search results"));
                spawn_thumbnail_refreshes(&state_rc, &ui_context, thumbnail_completion);
            }
            Ok(Err(error)) => {
                status_ui.label.set_text(&format!("Search failed: {error}"));
                error!("YouTube search failed: {error}");
            }
            Err(error) => {
                status_ui.label.set_text("Search failed");
                error!("YouTube search result channel closed: {error}");
            }
        }

        status_ui.spinner.stop();
        status_ui.button.set_sensitive(true);
    });
}

fn start_feed_refresh(
    state: Rc<RefCell<AppState>>,
    ui_context: AppContext,
    status_ui: StatusUi,
    subs_file: &Path,
) {
    status_ui.button.set_sensitive(false);
    status_ui.spinner.start();
    status_ui.label.set_text("Refreshing...");

    // Validate every cheap precondition before charging frogpoints, so a
    // refresh that cannot possibly succeed never costs anything.
    let channel_ids = match load_channel_ids(subs_file) {
        Ok(ids) => ids,
        Err(error) => {
            status_ui.finish(&format!("Error: {error}"));
            return;
        }
    };
    if !crate::feed::has_google_api_key() {
        status_ui.finish("Refresh blocked: GOOGLE_API_KEY is not set.");
        return;
    }

    match frogpoints::debit_refresh_frogpoints() {
        Ok(remaining) => {
            status_ui
                .label
                .set_text(&format!("Refreshing... ({remaining} frogpoints remaining)"));
        }
        Err(error) => {
            status_ui.finish(&format!("Refresh blocked: {error}"));
            return;
        }
    }

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

    spawn_refresh_progress_updates(progress_rx, status_ui);
    spawn_refreshed_videos_apply(videos_rx, state, ui_context);
}

/// Builds and presents the primary application window.
///
/// # Arguments
///
/// * `app` - Active application instance.
/// * `subs_file` - Path to the channel subscription file.
#[allow(clippy::too_many_lines)]
pub fn build_ui(app: &adw::Application, subs_file: PathBuf) {
    let runtime = match Runtime::new() {
        Ok(runtime) => Arc::new(runtime),
        Err(runtime_error) => {
            error!("Failed to create tokio runtime: {runtime_error}");
            return;
        }
    };

    let storage = match Storage::new() {
        Ok(storage) => storage,
        Err(storage_error) => {
            error!("Failed to initialize storage: {storage_error}");
            return;
        }
    };

    let watch_later = storage.load_watch_later();
    let mut videos = storage.load_videos();
    let loaded_feed_video_ids = storage.load_feed_video_ids();
    let should_seed_feed_video_ids = loaded_feed_video_ids.is_none();
    let mut feed_video_ids = loaded_feed_video_ids.unwrap_or_else(|| {
        videos
            .iter()
            .map(|video| video.video_id().to_string())
            .collect()
    });
    let known_video_ids = videos
        .iter()
        .map(|video| video.video_id().to_string())
        .collect::<HashSet<_>>();
    let repaired_watch_later_videos =
        storage.load_missing_watch_later_videos_from_info_json(&watch_later, &known_video_ids);
    if !repaired_watch_later_videos.is_empty() {
        info!(
            "Repaired {} missing watch-later video metadata records",
            repaired_watch_later_videos.len()
        );
        videos.extend(repaired_watch_later_videos);
        if let Err(save_error) = storage.save_videos(&videos) {
            warn!("Failed to persist repaired watch-later video metadata: {save_error}");
        }
    }
    feed_video_ids.retain(|video_id| known_video_ids.contains(video_id));
    if should_seed_feed_video_ids
        && let Err(save_error) = storage.save_feed_video_ids(&feed_video_ids)
    {
        warn!("Failed to persist initial feed video IDs: {save_error}");
    }
    let feed_video_id_set = feed_video_ids.iter().cloned().collect::<HashSet<_>>();

    // Cache pruning walks several directories; keep it off the main thread so
    // startup stays responsive.
    {
        let storage = storage.clone();
        let watch_later = watch_later.clone();
        runtime.spawn(async move {
            let cleanup_result = tokio::task::spawn_blocking(move || {
                storage.cleanup_unreferenced_cache_files(&watch_later, &feed_video_id_set)
            })
            .await;
            match cleanup_result {
                Ok(Ok(removed_count)) if removed_count > 0 => {
                    info!("Removed {removed_count} unreferenced cache artifacts");
                }
                Ok(Ok(_)) => {}
                Ok(Err(cleanup_error)) => {
                    warn!("Failed to clean up unreferenced cache artifacts: {cleanup_error}");
                }
                Err(join_error) => {
                    warn!("Cache cleanup task failed: {join_error}");
                }
            }
        });
    }

    let http_client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(client_error) => {
            error!("Failed to initialize HTTP client: {client_error}");
            return;
        }
    };

    let watch_later_saves = spawn_watch_later_persister(&runtime, storage.clone());
    let state = Rc::new(RefCell::new(AppState::new(
        videos,
        feed_video_ids,
        watch_later,
        storage,
    )));
    let selected_video: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("yt-gtk")
        .default_width(1200)
        .default_height(800)
        .build();
    frogpoints::start_leisure_monitor();

    let css_provider = gtk::CssProvider::new();
    css_provider.load_from_string(include_str!("../style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    // Load static window structure from .ui file
    let builder = gtk::Builder::from_string(include_str!("window.ui"));
    let toolbar_view = builder
        .object::<adw::ToolbarView>("toolbar_view")
        .expect("toolbar_view in window.ui");
    let refresh_button = builder
        .object::<Button>("refresh_button")
        .expect("refresh_button in window.ui");
    let status_label = builder
        .object::<Label>("status_label")
        .expect("status_label in window.ui");
    let spinner = builder
        .object::<Spinner>("spinner")
        .expect("spinner in window.ui");
    let stack = builder
        .object::<adw::ViewStack>("stack")
        .expect("stack in window.ui");
    let watch_later_page = builder
        .object::<adw::ViewStackPage>("watch_later_page")
        .expect("watch_later_page in window.ui");
    let feed_flow = builder
        .object::<FlowBox>("feed_flow")
        .expect("feed_flow in window.ui");
    let search_entry = builder
        .object::<gtk::SearchEntry>("search_entry")
        .expect("search_entry in window.ui");
    let search_button = builder
        .object::<Button>("search_button")
        .expect("search_button in window.ui");
    let search_flow = builder
        .object::<FlowBox>("search_flow")
        .expect("search_flow in window.ui");
    let watch_later_flow = builder
        .object::<FlowBox>("watch_later_flow")
        .expect("watch_later_flow in window.ui");

    window.set_content(Some(&toolbar_view));

    let context_menu = Popover::new();
    let subscription_removals = spawn_subscription_remover(&runtime, subs_file.clone());

    let ui_context = AppContext {
        summary_generator: SummaryGenerator::new(runtime.clone(), http_client.clone()),
        runtime,
        http_client,
        window: window.clone(),
        context_menu: context_menu.clone(),
        stack: stack.clone(),
        watch_later_page,
        feed_flow,
        search_flow,
        watch_later_flow,
        selected_video,
        feed_cards: CardMap::default(),
        search_cards: CardMap::default(),
        watch_later_cards: CardMap::default(),
        watch_later_saves,
        subscription_removals,
        downloads_in_progress: Rc::new(RefCell::new(HashSet::new())),
        videos_playing: Rc::new(RefCell::new(HashSet::new())),
    };
    create_context_menu(&context_menu, state.clone(), &ui_context);

    // Focus the search entry whenever the Search page becomes visible.
    {
        let search_entry = search_entry.clone();
        stack.connect_visible_child_name_notify(move |stack| {
            if stack.visible_child_name().as_deref() == Some(Tab::Search.stack_child_name()) {
                search_entry.grab_focus();
            }
        });
    }

    refresh_video_lists(&state, &ui_context);

    let search_status_ui = StatusUi {
        spinner: spinner.clone(),
        label: status_label.clone(),
        button: search_button.clone(),
    };
    {
        let state = state.clone();
        let ui_context = ui_context.clone();
        let search_status_ui = search_status_ui.clone();
        search_entry.connect_activate(move |entry| {
            start_youtube_search(
                state.clone(),
                ui_context.clone(),
                search_status_ui.clone(),
                &entry.text(),
            );
        });
    }
    {
        let state = state.clone();
        let ui_context = ui_context.clone();
        search_button.connect_clicked(move |_| {
            start_youtube_search(
                state.clone(),
                ui_context.clone(),
                search_status_ui.clone(),
                &search_entry.text(),
            );
        });
    }

    {
        let state = state.clone();
        let ui_context = ui_context.clone();
        let refresh_status_ui = StatusUi {
            spinner,
            label: status_label,
            button: refresh_button.clone(),
        };
        refresh_button.connect_clicked(move |_| {
            start_feed_refresh(
                state.clone(),
                ui_context.clone(),
                refresh_status_ui.clone(),
                &subs_file,
            );
        });
    }

    let startup_thumbnail_completion = {
        let state_ref = state.borrow();
        download_missing_thumbnails(
            state_ref.videos.values(),
            &state_ref.storage,
            ui_context.http_client.clone(),
            &ui_context.runtime,
        )
    };
    spawn_thumbnail_refreshes(&state, &ui_context, startup_thumbnail_completion);

    window.present();
}

fn update_watch_later_badge(page: &adw::ViewStackPage, count: usize) {
    page.set_badge_number(u32::try_from(count).unwrap_or(u32::MAX));
}

#[cfg(test)]
mod tests {
    use super::AppState;
    use crate::cache::Storage;
    use crate::data::{NewVideo, Video};
    use chrono::{TimeZone, Utc};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct TestDirs {
        root: TempDir,
    }

    impl TestDirs {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("test directory must be creatable");
            Self { root }
        }

        fn data_dir(&self) -> PathBuf {
            self.root.path().join("data")
        }

        fn cache_dir(&self) -> PathBuf {
            self.root.path().join("cache")
        }
    }

    fn test_storage(dirs: &TestDirs) -> Storage {
        Storage::new_at(dirs.data_dir(), dirs.cache_dir()).expect("test storage must initialize")
    }

    fn test_video(video_id: &str) -> Video {
        test_video_with_metadata(video_id, "channel-name", &format!("title-{video_id}"))
    }

    fn test_video_with_metadata(video_id: &str, channel_name: &str, title: &str) -> Video {
        let published = Utc
            .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
            .single()
            .expect("valid fixed test timestamp");

        Video::new(NewVideo {
            video_id: video_id.to_string(),
            channel_id: "channel-id".to_string(),
            channel_name: channel_name.to_string(),
            title: title.to_string(),
            published,
            thumbnail_url: "https://example.com/thumb.jpg".to_string(),
            duration_seconds: None,
        })
    }

    fn test_state(videos: Vec<Video>, watch_later: HashSet<String>, dirs: &TestDirs) -> AppState {
        let feed_video_ids = videos
            .iter()
            .map(|video| video.video_id().to_string())
            .collect();
        AppState::new(videos, feed_video_ids, watch_later, test_storage(dirs))
    }

    #[test]
    fn app_state_new_keeps_insertion_order() {
        let dirs = TestDirs::new();
        let state = test_state(
            vec![test_video("a"), test_video("b"), test_video("c")],
            HashSet::new(),
            &dirs,
        );
        let ids = state.videos.keys().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn app_state_set_videos_deduplicates_by_video_id() {
        let dirs = TestDirs::new();
        let mut state = test_state(Vec::new(), HashSet::new(), &dirs);
        state.set_videos(vec![test_video("dup"), test_video("x"), test_video("dup")]);
        let ids = state.videos.keys().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(ids, vec!["dup", "x"]);
        assert_eq!(state.feed_video_ids(), vec!["dup", "x"]);
        assert_eq!(state.videos.len(), 2);
    }

    #[test]
    fn app_state_search_results_do_not_change_feed_ids() {
        let dirs = TestDirs::new();
        let mut state = test_state(
            vec![test_video("feed-a"), test_video("feed-b")],
            HashSet::new(),
            &dirs,
        );

        state.set_search_results(vec![test_video("search-a"), test_video("feed-a")]);

        assert_eq!(state.feed_video_ids(), vec!["feed-a", "feed-b"]);
        assert_eq!(state.search_video_ids(), vec!["search-a", "feed-a"]);
        assert!(state.video_by_id("search-a").is_some());
    }

    #[test]
    fn refreshed_feed_preserves_watch_later_metadata_without_changing_feed_ids() {
        let dirs = TestDirs::new();
        let mut state = test_state(
            vec![test_video("old-watch-later"), test_video("old-feed")],
            HashSet::from(["old-watch-later".to_string()]),
            &dirs,
        );

        state.set_refreshed_feed_videos(vec![test_video("new-feed")]);

        assert_eq!(state.feed_video_ids(), vec!["new-feed"]);
        assert!(state.video_by_id("new-feed").is_some());
        assert!(state.video_by_id("old-watch-later").is_some());
        assert!(state.video_by_id("old-feed").is_none());
    }
}
