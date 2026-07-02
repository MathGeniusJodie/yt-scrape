mod cards;
mod comments;
mod summary_generator;

use crate::cache::{download_video, Storage, StorageError};
use crate::data::{Tab, Video};
use crate::feed::{fetch_all_feeds, fetch_youtube_search, load_channel_ids, FetchProgress};
use cards::VideoCardWidgets;

use adw::prelude::*;
use gtk::{gdk, glib};
use gtk::{Button, FlowBox, Label, Popover, Spinner};
use indexmap::IndexMap;
use log::{error, info, warn};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
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
    subs_file: PathBuf,
}

const FROGPOINTS_REFRESH_COST: i64 = 10;
const FROGPOINTS_LEISURE_COST: i64 = 1;
const FROGPOINTS_LEISURE_IDLE_SECONDS: u64 = 120;
const FROGPOINTS_LEISURE_INTERVAL_SECONDS: u32 = 60;
const FROGPOINTS_RELATIVE_PATH: &[&str] = &["Desktop", "RemoteVault", "frogpoints.md"];
const SVG_TEMPLATE_RELATIVE_PATH: &[&str] = &["Desktop", "allfiles", "templates"];
const INKSCAPE_CACHE_RELATIVE_PATH: &[&str] = &[".cache", "inkscape"];
static FROGPOINTS_LEISURE_MONITOR_STARTED: AtomicBool = AtomicBool::new(false);

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
    local_path.is_none_or(is_legacy_download)
}

fn frogpoints_path() -> Result<PathBuf, FrogpointsError> {
    home_relative_path(FROGPOINTS_RELATIVE_PATH)
}

fn home_relative_path(relative: &[&str]) -> Result<PathBuf, FrogpointsError> {
    let mut path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(FrogpointsError::MissingHome)?;
    path.extend(relative);
    Ok(path)
}

/// Directories scanned for recent SVG edits that mark active (non-leisure) work.
fn svg_watch_dirs() -> Result<[PathBuf; 2], FrogpointsError> {
    Ok([
        home_relative_path(SVG_TEMPLATE_RELATIVE_PATH)?,
        home_relative_path(INKSCAPE_CACHE_RELATIVE_PATH)?,
    ])
}

fn read_frogpoints(path: &Path) -> Result<i64, FrogpointsError> {
    fs::read_to_string(path)
        .map_err(FrogpointsError::Read)?
        .trim()
        .parse::<i64>()
        .map_err(FrogpointsError::InvalidNumber)
}

fn debit_frogpoints(path: &Path, cost: i64) -> Result<i64, FrogpointsError> {
    let current = read_frogpoints(path)?;

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

fn decrement_frogpoints(path: &Path, cost: i64) -> Result<i64, FrogpointsError> {
    let remaining = read_frogpoints(path)? - cost;
    fs::write(path, format!("{remaining}\n")).map_err(FrogpointsError::Write)?;
    Ok(remaining)
}

fn debit_refresh_frogpoints() -> Result<i64, FrogpointsError> {
    let path = frogpoints_path()?;
    debit_frogpoints(&path, FROGPOINTS_REFRESH_COST)
}

fn has_recent_svg_modification(
    template_dir: &Path,
    idle_duration: Duration,
) -> Result<bool, std::io::Error> {
    let cutoff = SystemTime::now()
        .checked_sub(idle_duration)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut pending_dirs = vec![template_dir.to_path_buf()];

    while let Some(dir) = pending_dirs.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending_dirs.push(entry.path());
                continue;
            }

            let is_svg = entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"));
            if is_svg && entry.metadata()?.modified()? > cutoff {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn mpv_window_exists() -> bool {
    match Command::new("xdotool")
        .args(["search", "--class", "mpv"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(error) => {
            warn!("Failed to query mpv windows with xdotool: {error}");
            false
        }
    }
}

fn charge_leisure_frogpoint_if_needed() {
    let watch_dirs = match svg_watch_dirs() {
        Ok(dirs) => dirs,
        Err(error) => {
            warn!("Failed to locate SVG watch directories: {error}");
            return;
        }
    };
    let idle_duration = Duration::from_secs(FROGPOINTS_LEISURE_IDLE_SECONDS);
    let recent_svg_modification = watch_dirs.iter().any(|dir| {
        match has_recent_svg_modification(dir, idle_duration) {
            Ok(recent) => recent,
            // A missing directory simply means no recent edits there.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                warn!(
                    "Failed to inspect SVG directory {}: {}",
                    dir.display(),
                    error
                );
                false
            }
        }
    });

    if !mpv_window_exists() {
        return;
    }

    if recent_svg_modification {
        info!("Recent SVG modification detected; no leisure mpv minute charged");
        return;
    }

    let path = match frogpoints_path() {
        Ok(path) => path,
        Err(error) => {
            warn!("Failed to locate frogpoints file: {error}");
            return;
        }
    };

    match decrement_frogpoints(&path, FROGPOINTS_LEISURE_COST) {
        Ok(remaining) => info!("Leisure mpv minute charged; {remaining} frogpoints remaining"),
        Err(error) => warn!("Failed to charge leisure frogpoint: {error}"),
    }
}

fn start_frogpoints_leisure_monitor() {
    if FROGPOINTS_LEISURE_MONITOR_STARTED
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    glib::timeout_add_seconds_local(FROGPOINTS_LEISURE_INTERVAL_SECONDS, || {
        charge_leisure_frogpoint_if_needed();
        glib::ControlFlow::Continue
    });
}

fn spawn_video_download(
    runtime: &Arc<Runtime>,
    video_id: String,
    video_path: PathBuf,
) -> async_channel::Receiver<String> {
    let (tx, rx) = async_channel::bounded(1);
    runtime.spawn(async move {
        if let Err(download_error) = download_video(&video_id, &video_path).await {
            error!("Failed to download video {video_id}: {download_error}");
        } else {
            let _ = tx.send(video_id.clone()).await;
        }
    });
    rx
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

fn persist_watch_later(runtime: &Arc<Runtime>, storage: Storage, watch_later: HashSet<String>) {
    persist_in_background(runtime, "watch-later list", move || {
        storage.save_watch_later(&watch_later)
    });
}

fn persist_unsubscribe(runtime: &Arc<Runtime>, subs_file: PathBuf, channel_id: String) {
    persist_in_background(runtime, "subscription removal", move || {
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
    });
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
    storage: &Storage,
    runtime: &Arc<Runtime>,
    video_id: &str,
    video_title: &str,
    local_path: Option<PathBuf>,
) -> Option<PathBuf> {
    match local_path {
        Some(path) if is_legacy_download(&path) => {
            // Legacy downloads lack embedded chapter/caption metadata. Upgrade in background
            // but still play the local file.
            let upgraded_path = storage.video_path(video_id, video_title);
            spawn_video_download(runtime, video_id.to_string(), upgraded_path);
            Some(path)
        }
        other => other,
    }
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
            Some(spawn_video_download(
                runtime,
                video_id.to_string(),
                video_path,
            ))
        } else {
            None
        };

        if !added {
            if let Err(remove_error) = state.storage.remove_cached_video_files(video_id) {
                error!("Failed to remove cached video {video_id}: {remove_error}");
            }
        }

        (
            added,
            download_rx,
            state.storage.clone(),
            state.watch_later.clone(),
        )
    };

    persist_watch_later(runtime, storage, watch_later_snapshot);
    (added, download_rx)
}

fn apply_watch_later_action(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let video_title = {
        let state = state_rc.borrow();
        state
            .video_by_id(video_id)
            .map(|video| video.title().to_string())
    };
    let Some(video_title) = video_title else {
        error!("Cannot toggle watch-later for missing video {video_id}");
        return;
    };

    let (added, download_rx) =
        toggle_watch_later_and_download(state_rc, &ui_context.runtime, video_id, &video_title);
    update_watch_later_toggles(ui_context, video_id, added);
    update_watch_later_badge(
        &ui_context.watch_later_page,
        state_rc.borrow().watch_later.len(),
    );
    sync_watch_later_card(state_rc, ui_context, video_id);
    if added {
        maybe_prefetch_summary_for_watch_later(state_rc, ui_context, video_id);
        if let Some(rx) = download_rx {
            refresh_video_downloading_badge(ui_context, video_id);
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

    let runtime = ui_context.runtime.clone();
    let subs_file = ui_context.subs_file.clone();
    dialog.connect_response(Some("unsubscribe"), move |_, _| {
        // Only persist the change — videos disappear from the feed on next refresh.
        persist_unsubscribe(&runtime, subs_file.clone(), channel_id.clone());
    });
    dialog.present(Some(&ui_context.window));
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
                    status_label.set_text(&format!("Refresh failed: {error}"));
                    error!("Fatal refresh error: {error}");
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
    let state_ref = state_rc.borrow();
    let downloaded_video_ids = state_ref.storage.cached_video_ids();
    update_watch_later_badge(&ui_context.watch_later_page, state_ref.watch_later.len());
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
    query: &str,
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
                        &ui_context.runtime,
                    );
                    state.set_search_results(videos);
                    completion
                };

                refresh_search_results(&state_rc, &ui_context);
                status_label.set_text(&format!("{result_count} search results"));
                spawn_thumbnail_refreshes(&state_rc, &ui_context, thumbnail_completion);
            }
            Ok(Err(error)) => {
                status_label.set_text(&format!("Search failed: {error}"));
                error!("YouTube search failed: {error}");
            }
            Err(error) => {
                status_label.set_text("Search failed");
                error!("YouTube search result channel closed: {error}");
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
    subs_file: &Path,
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

    let channel_ids = match load_channel_ids(subs_file) {
        Ok(ids) => ids,
        Err(error) => {
            spinner.stop();
            status_label.set_text(&format!("Error: {error}"));
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
    if should_seed_feed_video_ids {
        if let Err(save_error) = storage.save_feed_video_ids(&feed_video_ids) {
            warn!("Failed to persist initial feed video IDs: {save_error}");
        }
    }
    let feed_video_id_set = feed_video_ids.iter().cloned().collect::<HashSet<_>>();

    match storage.cleanup_unreferenced_cache_files(&watch_later, &feed_video_id_set) {
        Ok(removed_count) if removed_count > 0 => {
            info!("Removed {removed_count} unreferenced cache artifacts");
        }
        Ok(_) => {}
        Err(cleanup_error) => {
            warn!("Failed to clean up unreferenced cache artifacts: {cleanup_error}");
        }
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

    feed_video_ids.retain(|video_id| known_video_ids.contains(video_id));
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
    start_frogpoints_leisure_monitor();

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

    let feed_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let search_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let watch_later_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let context_menu = Popover::new();

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
        feed_cards,
        search_cards,
        watch_later_cards,
        subs_file: subs_file.clone(),
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
                &entry.text(),
            );
        });
    }
    {
        let state = state.clone();
        let ui_context = ui_context.clone();
        let spinner = spinner.clone();
        let status_label = status_label.clone();
        search_button.connect_clicked(move |button| {
            start_youtube_search(
                state.clone(),
                ui_context.clone(),
                spinner.clone(),
                status_label.clone(),
                button.clone(),
                &search_entry.text(),
            );
        });
    }

    {
        let state = state.clone();
        let ui_context = ui_context.clone();
        let refresh_button_for_handler = refresh_button.clone();
        refresh_button.connect_clicked(move |_| {
            start_feed_refresh(
                state.clone(),
                ui_context.clone(),
                spinner.clone(),
                status_label.clone(),
                refresh_button_for_handler.clone(),
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
    use super::{
        debit_frogpoints, decrement_frogpoints, has_recent_svg_modification, AppState,
        FrogpointsError, FROGPOINTS_REFRESH_COST,
    };
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

    #[test]
    fn decrement_frogpoints_allows_negative_balances() {
        let path = temporary_frogpoints_path("decrement");
        std::fs::write(&path, "0").expect("write test frogpoints file");

        let remaining = decrement_frogpoints(&path, 1).expect("decrement frogpoints");

        assert_eq!(remaining, -1);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read updated frogpoints file"),
            "-1\n"
        );

        std::fs::remove_file(path).expect("remove test frogpoints file");
    }

    #[test]
    fn has_recent_svg_modification_ignores_non_svg_files() {
        let dirs = TestDirs::new();
        std::fs::write(dirs.root.join("not-svg.txt"), "fresh").expect("write non-svg file");

        let has_recent_svg =
            has_recent_svg_modification(&dirs.root, std::time::Duration::from_secs(120))
                .expect("scan temp directory");

        assert!(!has_recent_svg);
    }

    #[test]
    fn has_recent_svg_modification_finds_nested_svg_files_case_insensitively() {
        let dirs = TestDirs::new();
        let nested_dir = dirs.root.join("nested");
        std::fs::create_dir_all(&nested_dir).expect("create nested test directory");
        std::fs::write(nested_dir.join("work.SVG"), "<svg />").expect("write svg file");

        let has_recent_svg =
            has_recent_svg_modification(&dirs.root, std::time::Duration::from_secs(120))
                .expect("scan temp directory");

        assert!(has_recent_svg);
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
            title,
            published,
            "https://example.com/thumb.jpg".to_string(),
            None,
        )
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
