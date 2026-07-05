mod cards;
mod comments;
mod summary_generator;

use crate::cache::{Storage, download_video};
use crate::config::Config;
use crate::data::{Tab, Video};
use crate::feed::{
    FeedError, FetchProgress, fetch_all_feeds, fetch_youtube_search, load_channel_ids,
};
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
    /// Downloaded IDs whose only local file is a legacy (non-`.mkv`) container,
    /// so upgrade decisions never rescan the cache directory on the main thread.
    legacy_video_ids: HashSet<String>,
    storage: Storage,
    /// Ordered queue for sidecar writes (see [`spawn_sidecar_persister`]), so
    /// per-video read-modify-write cycles never run on the main thread.
    sidecar_saves: async_channel::Sender<SidecarSave>,
}

/// One persisted sidecar mutation, applied strictly in send order.
enum SidecarSave {
    Watched { video_id: String, watched: bool },
    Transcript { video_id: String, text: String },
    Summary { video_id: String, text: String },
}

#[derive(Debug, Error)]
pub(super) enum CacheVideoError {
    #[error("Video {video_id} is no longer available")]
    MissingVideo { video_id: String },
}

/// Set of video IDs with an operation currently in flight (download, playback,
/// summary generation). Main-thread only; claim/release must be paired.
#[derive(Clone, Default)]
pub(super) struct InFlight {
    ids: Rc<RefCell<HashSet<String>>>,
}

impl InFlight {
    /// Claims the slot for `video_id`; returns `false` when already claimed.
    pub(super) fn try_claim(&self, video_id: &str) -> bool {
        self.ids.borrow_mut().insert(video_id.to_string())
    }

    pub(super) fn release(&self, video_id: &str) {
        self.ids.borrow_mut().remove(video_id);
    }

    pub(super) fn contains(&self, video_id: &str) -> bool {
        self.ids.borrow().contains(video_id)
    }
}

/// Runs `task` on the tokio runtime and yields its output once on the returned
/// channel, replacing the hand-rolled spawn/bounded-channel bridges.
fn run_in_background<T, F>(runtime: &Arc<Runtime>, task: F) -> async_channel::Receiver<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    runtime.spawn(async move {
        let _ = tx.send(task.await).await;
    });
    rx
}

/// Runs blocking `task` on the runtime's blocking pool and yields its output
/// once on the returned channel. If the task panics, the channel just closes.
fn run_blocking_in_background<T, F>(runtime: &Arc<Runtime>, task: F) -> async_channel::Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    runtime.spawn(async move {
        if let Ok(value) = tokio::task::spawn_blocking(task).await {
            let _ = tx.send(value).await;
        }
    });
    rx
}

impl AppState {
    fn new(
        videos: Vec<Video>,
        feed_video_ids: Vec<String>,
        watch_later: HashSet<String>,
        storage: Storage,
        sidecar_saves: async_channel::Sender<SidecarSave>,
    ) -> Self {
        let videos = videos
            .into_iter()
            .map(|video| (video.video_id().to_string(), video))
            .collect::<IndexMap<_, _>>();
        let (downloaded_video_ids, legacy_video_ids) = storage.scan_downloaded_video_ids();
        Self {
            videos,
            feed_video_ids,
            search_result_ids: Vec::new(),
            watch_later,
            downloaded_video_ids,
            legacy_video_ids,
            storage,
            sidecar_saves,
        }
    }

    fn set_videos(&mut self, videos: Vec<Video>) {
        self.videos = videos
            .into_iter()
            .map(|video| (video.video_id().to_string(), video))
            .collect();
        self.feed_video_ids = self.videos.keys().cloned().collect();
    }

    /// Copies session-mutable fields (watched, summary, transcript) from the
    /// in-memory videos onto freshly hydrated incoming ones. In-memory state is
    /// authoritative within a session: a queued sidecar write may not have
    /// flushed yet when `videos` was hydrated from disk.
    fn merge_in_memory_video_state(&self, videos: &mut [Video]) {
        for video in videos {
            if let Some(existing) = self.videos.get(video.video_id()) {
                video.set_watched(existing.is_watched());
                video.set_ai_summary(existing.ai_summary().map(ToString::to_string));
                video.set_transcript(existing.transcript().map(ToString::to_string));
            }
        }
    }

    fn set_refreshed_feed_videos(&mut self, mut videos: Vec<Video>) {
        self.merge_in_memory_video_state(&mut videos);
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

    fn set_search_results(&mut self, mut videos: Vec<Video>) {
        self.merge_in_memory_video_state(&mut videos);
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

    /// Queues a sidecar mutation on the ordered background persister, so the
    /// main thread never performs the sidecar read-modify-write itself.
    fn queue_sidecar_save(&self, save: SidecarSave) {
        if self.sidecar_saves.send_blocking(save).is_err() {
            error!("Sidecar persister is gone; sidecar changes will not be saved");
        }
    }

    /// Queues `save` on the sidecar persister and mirrors the same mutation
    /// onto the in-memory video via `update`.
    fn apply_sidecar_mutation(
        &mut self,
        video_id: &str,
        save: SidecarSave,
        update: impl FnOnce(&mut Video),
    ) -> Result<(), CacheVideoError> {
        self.queue_sidecar_save(save);
        let video =
            self.video_by_id_mut(video_id)
                .ok_or_else(|| CacheVideoError::MissingVideo {
                    video_id: video_id.to_string(),
                })?;
        update(video);
        Ok(())
    }

    fn cache_video_transcript(
        &mut self,
        video_id: &str,
        transcript: String,
    ) -> Result<(), CacheVideoError> {
        let save = SidecarSave::Transcript {
            video_id: video_id.to_string(),
            text: transcript.clone(),
        };
        self.apply_sidecar_mutation(video_id, save, |video| {
            video.set_transcript(Some(transcript));
        })
    }

    fn set_video_watched(&mut self, video_id: &str, watched: bool) -> Result<(), CacheVideoError> {
        let save = SidecarSave::Watched {
            video_id: video_id.to_string(),
            watched,
        };
        self.apply_sidecar_mutation(video_id, save, |video| video.set_watched(watched))
    }

    fn cache_video_ai_summary(
        &mut self,
        video_id: &str,
        ai_summary: String,
    ) -> Result<(), CacheVideoError> {
        let save = SidecarSave::Summary {
            video_id: video_id.to_string(),
            text: ai_summary.clone(),
        };
        self.apply_sidecar_mutation(video_id, save, |video| {
            video.set_ai_summary(Some(ai_summary));
        })
    }

    /// Videos worth persisting to `videos.json`: current feed and Watch Later
    /// entries. Transient search results must never leak into the on-disk cache.
    fn persistable_videos(&self) -> Vec<Video> {
        let feed_ids = self
            .feed_video_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        self.videos
            .values()
            .filter(|video| {
                feed_ids.contains(video.video_id()) || self.watch_later.contains(video.video_id())
            })
            .cloned()
            .collect()
    }
}

type CardMap = Rc<RefCell<TabCards>>;

/// Cheaply cloneable handle to the shared UI context.
///
/// Cards attach several closures each, every one holding a clone; a single
/// `Rc` keeps that at one refcount bump instead of ~15 GObject/Rc bumps.
#[derive(Clone)]
struct AppContext {
    inner: Rc<AppContextInner>,
}

impl std::ops::Deref for AppContext {
    type Target = AppContextInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

struct AppContextInner {
    runtime: Arc<Runtime>,
    http_client: reqwest::Client,
    /// Environment-derived configuration, read once at startup.
    config: Arc<Config>,
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
    downloads_in_progress: InFlight,
    /// Video IDs with a running mpv session, so accidental double-plays never
    /// spawn a second player racing the watched-state detection.
    videos_playing: InFlight,
    /// `true` while a feed refresh or search owns the shared status widgets,
    /// so the two activities can never interleave spinner/label updates.
    activity_in_progress: std::cell::Cell<bool>,
}

impl AppContext {
    /// Claims the shared refresh/search activity slot. Returns `false` when
    /// another activity already owns it.
    fn try_begin_activity(&self) -> bool {
        !self.activity_in_progress.replace(true)
    }

    fn end_activity(&self) {
        self.activity_in_progress.set(false);
    }
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

/// Spawns a background video download, reporting completion (`true`) or failure
/// (`false`) so the UI can always clear the downloading spinner.
fn spawn_video_download(
    runtime: &Arc<Runtime>,
    video_id: String,
    video_path: PathBuf,
) -> async_channel::Receiver<bool> {
    run_in_background(runtime, async move {
        match download_video(&video_id, &video_path).await {
            Ok(()) => true,
            Err(download_error) => {
                error!("Failed to download video {video_id}: {download_error}");
                false
            }
        }
    })
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
    if !ui_context.downloads_in_progress.try_claim(video_id) {
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
        ui_context.downloads_in_progress.release(&video_id);

        let still_wanted = {
            let mut state = state_rc.borrow_mut();
            if succeeded {
                state.downloaded_video_ids.insert(video_id.clone());
                state.legacy_video_ids.remove(&video_id);
                // A legacy-format upgrade leaves the old .mp4/.webm behind;
                // delete it so it can never shadow the fresh .mkv.
                let storage = state.storage.clone();
                let legacy_video_id = video_id.clone();
                persist_in_background(
                    &ui_context.runtime,
                    "legacy video file removal",
                    move || {
                        storage
                            .remove_legacy_video_files(&legacy_video_id)
                            .map(drop)
                    },
                );
            }
            let still_wanted = state.watch_later.contains(&video_id);
            // Removed from Watch Later mid-download: honor the removal now
            // that yt-dlp is no longer writing the files.
            if !still_wanted {
                state.downloaded_video_ids.remove(&video_id);
                state.legacy_video_ids.remove(&video_id);
                let storage = state.storage.clone();
                let removed_video_id = video_id.clone();
                persist_in_background(&ui_context.runtime, "cached video removal", move || {
                    storage
                        .remove_cached_video_files(&removed_video_id)
                        .map(drop)
                });
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

/// Starts a single consumer that applies queued items strictly in send order on
/// the blocking pool, logging failures under `description`. Ordered consumption
/// keeps read-modify-write persistence cycles from interleaving. When
/// `coalesce` is set, queued items are drained so only the newest hits disk.
fn spawn_ordered_persister<T, E, F>(
    runtime: &Arc<Runtime>,
    description: &'static str,
    coalesce: bool,
    apply: F,
) -> async_channel::Sender<T>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: Fn(T) -> Result<(), E> + Clone + Send + Sync + 'static,
{
    let (tx, rx) = async_channel::unbounded::<T>();
    runtime.spawn(async move {
        while let Ok(mut item) = rx.recv().await {
            if coalesce {
                while let Ok(newer_item) = rx.try_recv() {
                    item = newer_item;
                }
            }
            let apply = apply.clone();
            match tokio::task::spawn_blocking(move || apply(item)).await {
                Ok(Ok(())) => {}
                Ok(Err(save_error)) => error!("Failed to persist {description}: {save_error}"),
                Err(join_error) => error!("{description} persistence task failed: {join_error}"),
            }
        }
    });
    tx
}

/// Starts the ordered persister for watch-later snapshots; rapid toggles can
/// never be clobbered by an older snapshot finishing last.
fn spawn_watch_later_persister(
    runtime: &Arc<Runtime>,
    storage: Storage,
) -> async_channel::Sender<HashSet<String>> {
    spawn_ordered_persister(runtime, "watch-later list", true, move |snapshot| {
        storage.save_watch_later(&snapshot)
    })
}

/// Starts the ordered persister that applies sidecar mutations, keeping
/// per-video read-modify-write cycles off the main thread.
fn spawn_sidecar_persister(
    runtime: &Arc<Runtime>,
    storage: Storage,
) -> async_channel::Sender<SidecarSave> {
    spawn_ordered_persister(runtime, "video sidecar", false, move |save| match save {
        SidecarSave::Watched { video_id, watched } => {
            storage.save_video_watched(&video_id, watched)
        }
        SidecarSave::Transcript { video_id, text } => storage.save_transcript(&video_id, &text),
        SidecarSave::Summary { video_id, text } => storage.save_video_summary(&video_id, &text),
    })
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
    // The subs file is primary user data with no cache to rebuild it from; a
    // crash mid-rewrite must never truncate it.
    crate::cache::write_text_atomic(subs_file, &output)
}

/// Starts the ordered persister that rewrites the subscriptions file, so rapid
/// unsubscribes can never interleave their read-modify-write cycles and
/// resurrect a removed channel.
fn spawn_subscription_remover(
    runtime: &Arc<Runtime>,
    subs_file: PathBuf,
) -> async_channel::Sender<String> {
    spawn_ordered_persister(
        runtime,
        "subscription removal",
        false,
        move |channel_id: String| remove_channel_from_subs_file(&subs_file, &channel_id),
    )
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

/// Scans for a local video file on the runtime, keeping the directory walk off
/// the main thread. The receiver yields exactly once.
fn find_video_path_in_background(
    runtime: &Arc<Runtime>,
    storage: Storage,
    video_id: String,
) -> async_channel::Receiver<Option<PathBuf>> {
    run_blocking_in_background(runtime, move || storage.find_video_path(&video_id))
}

/// Decides what to play given the pre-scanned local path, kicking off a
/// background container upgrade for legacy Watch Later downloads.
fn resolve_playback_path(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
    video_title: &str,
    local_path: Option<PathBuf>,
) -> Option<PathBuf> {
    let path = local_path?;
    // Legacy downloads lack embedded chapter/caption metadata. Upgrade in
    // background — but only for Watch Later videos, since the cache policy
    // deletes downloads of anything else — and still play the local file.
    if is_legacy_download(&path) && state_rc.borrow().watch_later.contains(video_id) {
        start_tracked_download(state_rc, ui_context, video_id, video_title);
    }
    Some(path)
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

        // Decided from in-memory download state so the click handler never
        // rescans the cache directory on the main thread.
        let needs_download = added
            && (!state.downloaded_video_ids.contains(video_id)
                || state.legacy_video_ids.contains(video_id));

        // When a download is in flight its completion handler owns the files
        // and performs the deferred deletion; deleting here would race yt-dlp.
        if !added && !ui_context.downloads_in_progress.contains(video_id) {
            state.downloaded_video_ids.remove(video_id);
            state.legacy_video_ids.remove(video_id);
            let storage = state.storage.clone();
            let removed_video_id = video_id.to_string();
            persist_in_background(&ui_context.runtime, "cached video removal", move || {
                storage
                    .remove_cached_video_files(&removed_video_id)
                    .map(drop)
            });
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
            || ui_context.downloads_in_progress.contains(video_id);
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
    ui_context: AppContext,
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
                    // A majority-failed refresh delivered a degraded feed;
                    // give the points back even though results were applied.
                    if final_failed > successful_channels {
                        refund_refresh_frogpoints_in_background();
                        status_label.set_text(&format!(
                            "{total_videos} videos loaded ({successful_channels} channels ok, \
                             {final_failed} failed; frogpoints refunded)"
                        ));
                    } else if final_failed > 0 {
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
        ui_context.end_activity();
    });
}

/// Refreshes card thumbnails on the main loop once background downloads complete.
fn spawn_thumbnail_refreshes(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    completion_rx: async_channel::Receiver<Vec<String>>,
) {
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
            // Sidecar hydration reads one file per video; keep it off the main
            // thread. In-memory state is merged back in `set_refreshed_feed_videos`.
            let storage = state_rc.borrow().storage.clone();
            let hydrated_rx = run_blocking_in_background(&ui_context.runtime, move || {
                storage.hydrate_videos_from_sidecars(&mut videos);
                videos
            });
            let Ok(videos) = hydrated_rx.recv().await else {
                return;
            };
            let mut state = state_rc.borrow_mut();
            let feed_video_ids = videos
                .iter()
                .map(|video| video.video_id().to_string())
                .collect::<Vec<_>>();
            state.set_refreshed_feed_videos(videos);
            let persistable_videos = state.persistable_videos();
            persist_videos(
                &ui_context.runtime,
                state.storage.clone(),
                persistable_videos,
            );
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

    let Some(api_key) = ui_context.config.google_api_key.clone() else {
        status_ui
            .label
            .set_text("Search blocked: GOOGLE_API_KEY is not set.");
        return;
    };

    // Enter in the search entry bypasses the disabled button, and refresh
    // shares the same status widgets: one activity at a time.
    if !ui_context.try_begin_activity() {
        status_ui
            .label
            .set_text("A refresh or search is already running.");
        return;
    }

    status_ui.button.set_sensitive(false);
    status_ui.spinner.start();
    status_ui
        .label
        .set_text(&format!("Searching YouTube for \"{query}\"..."));

    let client = ui_context.http_client.clone();
    let storage = state_rc.borrow().storage.clone();
    let results_rx = run_in_background(&ui_context.runtime, async move {
        let mut search_result = fetch_youtube_search(&client, &api_key, &query).await;
        if let Ok(videos) = search_result.as_mut() {
            // Sidecar hydration is file I/O: do it here, not on the main thread.
            storage.hydrate_videos_from_sidecars(videos);
        }
        search_result
    });

    glib::MainContext::default().spawn_local(async move {
        match results_rx.recv().await {
            Ok(Ok(videos)) => {
                let result_count = videos.len();
                let thumbnail_completion = {
                    let mut state = state_rc.borrow_mut();
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
        ui_context.end_activity();
    });
}

/// Why a refresh could not be prepared (no cost was charged).
#[derive(Debug, Error)]
enum RefreshPrepError {
    #[error("Error: {0}")]
    LoadChannels(#[from] FeedError),
    #[error("Refresh blocked: GOOGLE_API_KEY is not set.")]
    MissingApiKey,
    #[error("Refresh blocked: {0}")]
    Frogpoints(#[from] frogpoints::FrogpointsError),
    #[error("Refresh setup task was dropped.")]
    SetupTaskDropped,
}

/// Reads the subscriptions file and debits the refresh cost, in that order,
/// so a refresh that cannot possibly succeed never costs anything. Runs on a
/// blocking thread: both steps are file I/O that must stay off the main loop.
fn prepare_refresh(
    subs_file: &Path,
    api_key: Option<String>,
) -> Result<(Vec<String>, String, i64), RefreshPrepError> {
    let channel_ids = load_channel_ids(subs_file)?;
    let api_key = api_key.ok_or(RefreshPrepError::MissingApiKey)?;
    let remaining = frogpoints::debit_refresh_frogpoints()?;
    Ok((channel_ids, api_key, remaining))
}

fn start_feed_refresh(
    state: Rc<RefCell<AppState>>,
    ui_context: AppContext,
    status_ui: StatusUi,
    subs_file: PathBuf,
) {
    if !ui_context.try_begin_activity() {
        status_ui
            .label
            .set_text("A refresh or search is already running.");
        return;
    }

    status_ui.button.set_sensitive(false);
    status_ui.spinner.start();
    status_ui.label.set_text("Refreshing...");

    let api_key = ui_context.config.google_api_key.clone();
    let prepared_rx = run_blocking_in_background(&ui_context.runtime, move || {
        prepare_refresh(&subs_file, api_key)
    });

    glib::MainContext::default().spawn_local(async move {
        let prepared = prepared_rx
            .recv()
            .await
            .unwrap_or(Err(RefreshPrepError::SetupTaskDropped));
        let (channel_ids, api_key, remaining) = match prepared {
            Ok(prepared) => prepared,
            Err(prep_error) => {
                status_ui.finish(&prep_error.to_string());
                ui_context.end_activity();
                return;
            }
        };
        status_ui
            .label
            .set_text(&format!("Refreshing... ({remaining} frogpoints remaining)"));

        let (progress_tx, progress_rx) = async_channel::bounded::<FetchProgress>(100);
        let (videos_tx, videos_rx) = async_channel::bounded::<Vec<Video>>(1);

        let progress_tx_for_errors = progress_tx.clone();
        let fetch_client = ui_context.http_client.clone();
        ui_context.runtime.spawn(async move {
            match fetch_all_feeds(&fetch_client, api_key, channel_ids, progress_tx).await {
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

        spawn_refresh_progress_updates(progress_rx, status_ui, ui_context.clone());
        spawn_refreshed_videos_apply(videos_rx, state, ui_context);
    });
}

/// Persisted state loaded (and repaired) at startup.
struct LoadedState {
    videos: Vec<Video>,
    feed_video_ids: Vec<String>,
    watch_later: HashSet<String>,
    /// `false` when any state file exists but failed to parse. A damaged file
    /// is NOT "empty state": cache cleanup keyed off an accidentally-empty set
    /// would delete every download.
    state_files_healthy: bool,
}

/// Loads watch-later, videos, and feed-ID state from disk, repairing missing
/// Watch Later metadata from local `info.json` files.
fn load_persisted_state(storage: &Storage) -> LoadedState {
    let mut state_files_healthy = true;
    let watch_later = storage.load_watch_later().unwrap_or_else(|load_error| {
        error!("Watch-later state file is damaged; continuing without it: {load_error}");
        state_files_healthy = false;
        HashSet::new()
    });
    let mut videos = storage.load_videos().unwrap_or_else(|load_error| {
        error!("Videos cache file is damaged; continuing without it: {load_error}");
        state_files_healthy = false;
        Vec::new()
    });
    let loaded_feed_video_ids = storage.load_feed_video_ids().unwrap_or_else(|load_error| {
        error!("Feed ID cache file is damaged; continuing without it: {load_error}");
        state_files_healthy = false;
        None
    });
    let should_seed_feed_video_ids = state_files_healthy && loaded_feed_video_ids.is_none();
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

    LoadedState {
        videos,
        feed_video_ids,
        watch_later,
        state_files_healthy,
    }
}

/// Presents a bare error window so a fatal startup failure is visible even
/// when the app was not launched from a terminal.
fn present_startup_error(app: &adw::Application, message: &str) {
    error!("{message}");
    let label = Label::new(Some(message));
    label.set_wrap(true);
    label.set_margin_top(24);
    label.set_margin_bottom(24);
    label.set_margin_start(24);
    label.set_margin_end(24);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("yt-gtk — startup error")
        .default_width(500)
        .content(&label)
        .build();
    window.present();
}

/// Builds and presents the primary application window.
///
/// # Arguments
///
/// * `app` - Active application instance.
/// * `subs_file` - Path to the channel subscription file.
#[allow(clippy::too_many_lines)]
pub fn build_ui(app: &adw::Application, subs_file: PathBuf) {
    let config = Arc::new(Config::from_env());

    let runtime = match Runtime::new() {
        Ok(runtime) => Arc::new(runtime),
        Err(runtime_error) => {
            present_startup_error(
                app,
                &format!("Failed to create tokio runtime: {runtime_error}"),
            );
            return;
        }
    };

    let storage = match Storage::new() {
        Ok(storage) => storage,
        Err(storage_error) => {
            present_startup_error(
                app,
                &format!("Failed to initialize storage: {storage_error}"),
            );
            return;
        }
    };

    let loaded = load_persisted_state(&storage);
    let LoadedState {
        videos,
        feed_video_ids,
        watch_later,
        state_files_healthy,
    } = loaded;
    let feed_video_id_set = feed_video_ids.iter().cloned().collect::<HashSet<_>>();

    // Cache pruning walks several directories; keep it off the main thread so
    // startup stays responsive.
    if state_files_healthy {
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
    } else {
        warn!("Skipping cache cleanup: a state file is damaged and cleanup would over-delete");
    }

    let http_client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(client_error) => {
            present_startup_error(
                app,
                &format!("Failed to initialize HTTP client: {client_error}"),
            );
            return;
        }
    };

    let watch_later_saves = spawn_watch_later_persister(&runtime, storage.clone());
    let sidecar_saves = spawn_sidecar_persister(&runtime, storage.clone());
    let state = Rc::new(RefCell::new(AppState::new(
        videos,
        feed_video_ids,
        watch_later,
        storage,
        sidecar_saves,
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
        inner: Rc::new(AppContextInner {
            summary_generator: SummaryGenerator::new(
                runtime.clone(),
                http_client.clone(),
                config.clone(),
            ),
            runtime,
            http_client,
            config,
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
            downloads_in_progress: InFlight::default(),
            videos_playing: InFlight::default(),
            activity_in_progress: std::cell::Cell::new(false),
        }),
    };
    create_context_menu(&context_menu, &state, &ui_context);

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
        let (sidecar_saves, _rx) = async_channel::unbounded();
        AppState::new(
            videos,
            feed_video_ids,
            watch_later,
            test_storage(dirs),
            sidecar_saves,
        )
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

    #[test]
    fn refreshed_feed_preserves_in_memory_watched_state_and_summary() {
        let dirs = TestDirs::new();
        let mut state = test_state(
            vec![test_video("a"), test_video("b")],
            HashSet::new(),
            &dirs,
        );
        state
            .video_by_id_mut("a")
            .expect("video a must exist")
            .set_watched(true);
        state
            .video_by_id_mut("a")
            .expect("video a must exist")
            .set_ai_summary(Some("existing summary".to_string()));

        state.set_refreshed_feed_videos(vec![test_video("a"), test_video("b")]);

        let refreshed_a = state.video_by_id("a").expect("video a must exist");
        assert!(refreshed_a.is_watched());
        assert_eq!(refreshed_a.ai_summary(), Some("existing summary"));

        let refreshed_b = state.video_by_id("b").expect("video b must exist");
        assert!(!refreshed_b.is_watched());
        assert_eq!(refreshed_b.ai_summary(), None);
    }

    #[test]
    fn search_results_preserve_in_memory_watched_state() {
        let dirs = TestDirs::new();
        let mut state = test_state(vec![test_video("feed-a")], HashSet::new(), &dirs);
        state
            .video_by_id_mut("feed-a")
            .expect("video feed-a must exist")
            .set_watched(true);

        state.set_search_results(vec![test_video("feed-a")]);

        let video = state
            .video_by_id("feed-a")
            .expect("video feed-a must exist");
        assert!(video.is_watched());
    }
}
