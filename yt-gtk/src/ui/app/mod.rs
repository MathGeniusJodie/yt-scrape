mod cards;
mod summary_generator;

use crate::cache::{download_video, Storage, StorageError};
use crate::data::{Tab, Video};
use crate::feed::{fetch_all_feeds, load_channel_ids, FetchProgress};
use cards::VideoCardWidgets;

use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Button, FlowBox, Label, Popover, ScrolledWindow, Spinner, Stack,
};
use indexmap::IndexMap;
use log::{error, info, warn};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;
use tokio::runtime::Runtime;

use cards::{
    create_context_menu, download_missing_thumbnails, populate_flow_box, sync_watch_later_card,
    update_watch_later_toggles,
};
use summary_generator::{maybe_prefetch_summary_for_watch_later, SummaryGenerator};

struct AppState {
    videos: IndexMap<String, Video>,
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
        Self {
            videos: videos
                .into_iter()
                .map(|video| (video.video_id().to_string(), video))
                .collect(),
            watch_later,
            storage,
        }
    }

    fn set_videos(&mut self, videos: Vec<Video>) {
        self.videos = videos
            .into_iter()
            .map(|video| (video.video_id().to_string(), video))
            .collect();
    }

    fn video_by_id(&self, video_id: &str) -> Option<&Video> {
        self.videos.get(video_id)
    }

    fn video_by_id_mut(&mut self, video_id: &str) -> Option<&mut Video> {
        self.videos.get_mut(video_id)
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
    watch_later_flow: FlowBox,
    selected_video: Rc<RefCell<Option<String>>>,
    badge: Label,
    feed_cards: CardMap,
    watch_later_cards: CardMap,
    subs_file: PathBuf,
}

const CARD_WIDTH: i32 = 320;
const CARD_SPACING: i32 = 16;
const GRID_PADDING: i32 = 16;

fn is_legacy_download(path: &Path) -> bool {
    !matches!(path.extension().and_then(|ext| ext.to_str()), Some("mkv"))
}

fn needs_download_upgrade(local_path: Option<&Path>) -> bool {
    local_path.map(is_legacy_download).unwrap_or(true)
}

fn spawn_video_download(runtime: Arc<Runtime>, video_id: String, video_path: PathBuf) {
    runtime.spawn(async move {
        if let Err(download_error) = download_video(&video_id, &video_path).await {
            error!("Failed to download video {}: {}", video_id, download_error);
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
            let lines: Vec<&str> = content
                .lines()
                .filter(|line| {
                    let trimmed = line.trim();
                    // Keep comments, blank lines, and lines not matching this channel
                    trimmed.starts_with('#') || trimmed.is_empty() || trimmed != channel_id
                })
                .collect();
            let output = if content.ends_with('\n') {
                lines.join("\n") + "\n"
            } else {
                lines.join("\n")
            };
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
            spawn_video_download(runtime, video_id.to_string(), upgraded_path);
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
) -> bool {
    let (added, storage, watch_later_snapshot) = {
        let mut state = state_rc.borrow_mut();
        let added = !state.watch_later.remove(video_id);
        if added {
            state.watch_later.insert(video_id.to_string());
        }

        let local_path = state.storage.find_video_path(video_id);
        if added && needs_download_upgrade(local_path.as_deref()) {
            let video_path = state.storage.video_path(video_id, video_title);
            spawn_video_download(runtime.clone(), video_id.to_string(), video_path);
        }
        if !added {
            if let Err(remove_error) = state.storage.remove_cached_video_files(video_id) {
                error!(
                    "Failed to remove cached video {}: {}",
                    video_id, remove_error
                );
            }
        }

        (added, state.storage.clone(), state.watch_later.clone())
    };

    persist_watch_later(runtime.clone(), storage, watch_later_snapshot);
    added
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

    let added =
        toggle_watch_later_and_download(state_rc, &ui_context.runtime, &video_id, &video_title);
    update_watch_later_toggles(ui_context, &video_id, added);
    update_watch_later_badge(&ui_context.badge, state_rc.borrow().watch_later.len());
    sync_watch_later_card(state_rc, ui_context, &video_id);
    if added {
        maybe_prefetch_summary_for_watch_later(state_rc, ui_context, &video_id);
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
    populate_flow_box(Tab::WatchLater, &downloaded_video_ids, state_rc, ui_context);
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
    match storage.prune_cached_videos_not_in_watch_later(&watch_later) {
        Ok(removed_count) if removed_count > 0 => {
            info!("Removed {} cached videos not in watch-later", removed_count);
        }
        Ok(_) => {}
        Err(cleanup_error) => {
            warn!(
                "Failed to clean up cached videos from watch-later state: {}",
                cleanup_error
            );
        }
    }
    let videos = storage.load_videos();
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

    // Tab toggle buttons switch the stack page and update sibling active state
    {
        let stack = stack.clone();
        let watch_later_tab = watch_later_tab.clone();
        let feed_flow = feed_flow.clone();
        feed_tab.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            stack.set_visible_child_name("feed");
            watch_later_tab.set_active(false);
            update_flow_width(&feed_flow, stack.allocated_width());
        });
    }
    {
        let stack = stack.clone();
        let feed_tab = feed_tab.clone();
        let watch_later_flow = watch_later_flow.clone();
        watch_later_tab.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            stack.set_visible_child_name("watch-later");
            feed_tab.set_active(false);
            update_flow_width(&watch_later_flow, stack.allocated_width());
        });
    }

    let feed_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let watch_later_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let context_menu = Popover::new(None::<&gtk::Widget>);

    let ui_context = AppContext {
        summary_generator: SummaryGenerator::new(runtime.clone(), http_client.clone()),
        runtime: runtime.clone(),
        http_client: http_client.clone(),
        window: window.clone(),
        context_menu: context_menu.clone(),
        feed_flow: feed_flow.clone(),
        watch_later_flow: watch_later_flow.clone(),
        selected_video: selected_video.clone(),
        badge: badge.clone(),
        feed_cards: feed_cards.clone(),
        watch_later_cards: watch_later_cards.clone(),
        subs_file: subs_file.clone(),
    };
    create_context_menu(&context_menu, state.clone(), &ui_context);

    refresh_video_lists(&state, &ui_context);

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
    use super::AppState;
    use crate::cache::Storage;
    use crate::data::Video;
    use chrono::{TimeZone, Utc};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR_ID: AtomicU64 = AtomicU64::new(0);

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

    fn test_video(video_id: &str) -> Video {
        let published = Utc
            .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
            .single()
            .expect("valid fixed test timestamp");

        Video::new(
            video_id.to_string(),
            "channel-id".to_string(),
            "channel-name".to_string(),
            format!("title-{video_id}"),
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
        assert_eq!(state.videos.len(), 2);
    }
}
