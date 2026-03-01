mod cards;
mod refresh;
mod summary;
mod summary_generator;

use crate::cache::{download_video, Storage};
use crate::data::Video;
use crate::feed::{fetch_all_feeds, load_channel_ids, FetchProgress};
use crate::ui::video_card::VideoCardWidgets;

use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, FlowBox, HeaderBar, Label, Orientation,
    Popover, ScrolledWindow, Spinner, Stack,
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

use cards::create_context_menu;
use refresh::{
    download_missing_thumbnails, refresh_video_lists, refresh_video_thumbnails,
    sync_watch_later_card, update_watch_later_toggles,
};
use summary::maybe_prefetch_summary_for_watch_later;
use summary_generator::SummaryGenerator;

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
        source: anyhow::Error,
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
            .save_video_transcript(video_id, &transcript)
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
            .save_video_ai_summary(video_id, &ai_summary)
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

#[derive(Clone)]
struct AsyncContext {
    runtime: Arc<Runtime>,
    http_client: reqwest::Client,
    summary_generator: SummaryGenerator,
}

#[derive(Clone)]
struct WidgetContext {
    window: ApplicationWindow,
    context_menu: Popover,
    feed_flow: FlowBox,
    watch_later_flow: FlowBox,
    selected_video: Rc<RefCell<Option<String>>>,
    badge: Label,
    feed_cards: Rc<RefCell<HashMap<String, VideoCardWidgets>>>,
    watch_later_cards: Rc<RefCell<HashMap<String, VideoCardWidgets>>>,
}

#[derive(Clone)]
struct UiContext {
    async_ctx: AsyncContext,
    widgets: WidgetContext,
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

fn configure_video_flow(flow: &FlowBox) {
    flow.set_widget_name("video-grid");
    flow.set_valign(gtk::Align::Start);
    flow.set_halign(gtk::Align::Center);
    flow.set_max_children_per_line(10);
    flow.set_min_children_per_line(1);
    flow.set_selection_mode(gtk::SelectionMode::Single);
    flow.set_homogeneous(false);
    flow.set_column_spacing(CARD_SPACING as u32);
    flow.set_row_spacing(CARD_SPACING as u32);
}

fn update_flow_width(flow: &FlowBox, viewport_width: i32) {
    let available_width = (viewport_width - GRID_PADDING * 2).max(1);
    let num_columns = ((available_width + CARD_SPACING) / (CARD_WIDTH + CARD_SPACING)).max(1);
    let optimal_width = num_columns * CARD_WIDTH + (num_columns - 1) * CARD_SPACING;
    flow.set_size_request(optimal_width, -1);
}

fn connect_tab_button_to_stack(
    tab_button: &gtk::ToggleButton,
    stack: &Stack,
    visible_child_name: &'static str,
    active_flow: &FlowBox,
    inactive_tabs: &[gtk::ToggleButton],
) {
    let stack = stack.clone();
    let active_flow = active_flow.clone();
    let inactive_tabs = inactive_tabs.to_vec();

    tab_button.connect_toggled(move |button| {
        if !button.is_active() {
            return;
        }

        stack.set_visible_child_name(visible_child_name);
        for tab in &inactive_tabs {
            tab.set_active(false);
        }
        update_flow_width(&active_flow, stack.allocated_width());
    });
}

fn create_video_grid() -> (ScrolledWindow, FlowBox) {
    let scroll = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    let container = GtkBox::new(Orientation::Horizontal, 0);
    container.set_halign(gtk::Align::Center);
    container.set_valign(gtk::Align::Start);

    let flow = FlowBox::new();
    configure_video_flow(&flow);
    container.pack_start(&flow, false, false, 0);

    let flow_for_resize = flow.clone();
    scroll.connect_size_allocate(move |_widget, allocation| {
        update_flow_width(&flow_for_resize, allocation.width());
    });

    scroll.add(&container);
    (scroll, flow)
}

fn toggle_watch_later_and_download(
    state_rc: &Rc<RefCell<AppState>>,
    runtime: &Arc<Runtime>,
    video_id: &str,
    video_title: &str,
) -> bool {
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

    if let Err(save_error) = state.storage.save_watch_later(&state.watch_later) {
        error!("Failed to persist watch-later list: {}", save_error);
    }

    added
}

fn apply_watch_later_action(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
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

    let added = toggle_watch_later_and_download(
        state_rc,
        &ui_context.async_ctx.runtime,
        &video_id,
        &video_title,
    );
    update_watch_later_toggles(ui_context, &video_id, added);
    sync_watch_later_card(state_rc, ui_context, &video_id);
    if added {
        maybe_prefetch_summary_for_watch_later(state_rc, ui_context, &video_id);
    }
}

fn spawn_refresh_progress_updates(
    progress_rx: async_channel::Receiver<FetchProgress>,
    spinner: Spinner,
    status_label: Label,
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
    });
}

fn spawn_refreshed_videos_apply(
    videos_rx: async_channel::Receiver<Vec<Video>>,
    state_rc: Rc<RefCell<AppState>>,
    ui_context: UiContext,
) {
    glib::MainContext::default().spawn_local(async move {
        if let Ok(mut videos) = videos_rx.recv().await {
            let mut state = state_rc.borrow_mut();
            state.storage.hydrate_videos_from_sidecars(&mut videos);
            if let Err(save_error) = state.storage.save_videos(&videos) {
                error!("Failed to persist refreshed videos cache: {}", save_error);
            }
            state.set_videos(videos);

            let thumbnail_completion = download_missing_thumbnails(
                state.videos.values(),
                &state.storage,
                ui_context.async_ctx.http_client.clone(),
                ui_context.async_ctx.runtime.clone(),
            );
            drop(state);

            refresh_video_lists(&state_rc, &ui_context);

            if let Some(thumbnail_completion) = thumbnail_completion {
                let state2 = state_rc.clone();
                let ui2 = ui_context.clone();
                glib::MainContext::default().spawn_local(async move {
                    if let Ok(video_ids) = thumbnail_completion.recv().await {
                        refresh_video_thumbnails(&state2, &ui2, &video_ids);
                    }
                });
            }
        }
    });
}

fn start_feed_refresh(
    state: Rc<RefCell<AppState>>,
    ui_context: UiContext,
    spinner: Spinner,
    status_label: Label,
    subs_file: PathBuf,
) {
    spinner.start();
    status_label.set_text("Refreshing...");

    let channel_ids = match load_channel_ids(&subs_file) {
        Ok(ids) => ids,
        Err(error) => {
            spinner.stop();
            status_label.set_text(&format!("Error: {}", error));
            return;
        }
    };

    let (progress_tx, progress_rx) = async_channel::bounded::<FetchProgress>(100);
    let (videos_tx, videos_rx) = async_channel::bounded::<Vec<Video>>(1);

    let progress_tx_for_errors = progress_tx.clone();
    let fetch_client = ui_context.async_ctx.http_client.clone();
    ui_context.async_ctx.runtime.spawn(async move {
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

    spawn_refresh_progress_updates(progress_rx, spinner, status_label);
    spawn_refreshed_videos_apply(videos_rx, state, ui_context);
}

/// Builds and presents the primary GTK application window.
///
/// # Arguments
///
/// * `app` - Active GTK application instance.
/// * `subs_file` - Path to the channel subscription file.
pub fn build_ui(app: &Application, subs_file: PathBuf) {
    // Create tokio runtime for async operations
    let runtime = match Runtime::new() {
        Ok(runtime) => Arc::new(runtime),
        Err(runtime_error) => {
            error!("Failed to create tokio runtime: {}", runtime_error);
            return;
        }
    };

    // Initialize storage
    let storage = match Storage::new() {
        Ok(storage) => storage,
        Err(storage_error) => {
            error!("Failed to initialize storage: {}", storage_error);
            return;
        }
    };

    // Load cached data
    let videos = storage.load_videos();
    let watch_later = storage.load_watch_later();
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

    // Selected video for context menu actions
    let selected_video: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Create main window
    let window = ApplicationWindow::builder()
        .application(app)
        .title("yt-gtk")
        .default_width(1200)
        .default_height(800)
        .build();

    // Apply CSS
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

    // Header bar
    let header = HeaderBar::new();
    header.set_show_close_button(true);
    header.set_title(Some("yt-gtk"));

    // Refresh button with icon
    let refresh_button =
        Button::from_icon_name(Some("view-refresh-symbolic"), gtk::IconSize::Button);
    refresh_button.set_widget_name("refresh-button");
    refresh_button.set_tooltip_text(Some("Refresh feeds"));
    header.pack_start(&refresh_button);

    // Status label
    let status_label = Label::new(None);
    status_label.set_widget_name("status-label");
    header.pack_end(&status_label);

    // Spinner for loading
    let spinner = Spinner::new();
    header.pack_end(&spinner);

    window.set_titlebar(Some(&header));

    // Main layout
    let main_box = GtkBox::new(Orientation::Vertical, 0);

    // Stack for tabs
    let stack = Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);

    let (feed_scroll, feed_flow) = create_video_grid();
    stack.add_titled(&feed_scroll, "feed", "Feed");

    let (watch_later_scroll, watch_later_flow) = create_video_grid();
    stack.add_titled(&watch_later_scroll, "watch-later", "Watch Later");

    // Custom tab bar with stylish badge
    let tab_bar = GtkBox::new(Orientation::Horizontal, 0);
    tab_bar.set_widget_name("tab-bar");
    tab_bar.style_context().add_class("linked");

    // Feed tab button
    let feed_tab = gtk::ToggleButton::with_label("Feed");
    feed_tab.set_widget_name("tab-feed");
    feed_tab.set_active(true);
    tab_bar.pack_start(&feed_tab, false, false, 0);

    // Watch Later tab button with badge
    let watch_later_tab = gtk::ToggleButton::new();
    watch_later_tab.set_widget_name("tab-watch-later");

    let wl_tab_box = GtkBox::new(Orientation::Horizontal, 6);
    let wl_tab_label = Label::new(Some("Watch Later"));
    wl_tab_box.pack_start(&wl_tab_label, false, false, 0);

    let wl_badge = Label::new(None);
    wl_badge.set_widget_name("watch-later-badge");
    wl_tab_box.pack_start(&wl_badge, false, false, 0);

    watch_later_tab.add(&wl_tab_box);
    tab_bar.pack_start(&watch_later_tab, false, false, 0);

    header.set_custom_title(Some(&tab_bar));

    {
        let feed_flow = feed_flow.clone();
        let watch_later_flow = watch_later_flow.clone();
        stack.connect_size_allocate(move |_stack, allocation| {
            update_flow_width(&feed_flow, allocation.width());
            update_flow_width(&watch_later_flow, allocation.width());
        });
    }

    // Connect tab buttons to stack
    connect_tab_button_to_stack(
        &feed_tab,
        &stack,
        "feed",
        &feed_flow,
        std::slice::from_ref(&watch_later_tab),
    );
    connect_tab_button_to_stack(
        &watch_later_tab,
        &stack,
        "watch-later",
        &watch_later_flow,
        std::slice::from_ref(&feed_tab),
    );

    main_box.pack_start(&stack, true, true, 0);
    window.add(&main_box);

    let feed_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));
    let watch_later_cards = Rc::new(RefCell::new(HashMap::<String, VideoCardWidgets>::new()));

    // Create context menu with handlers connected once
    let context_menu = Popover::new(None::<&gtk::Widget>);

    let async_ctx = AsyncContext {
        summary_generator: SummaryGenerator::new(runtime.clone(), http_client.clone()),
        runtime: runtime.clone(),
        http_client: http_client.clone(),
    };
    let widgets = WidgetContext {
        window: window.clone(),
        context_menu: context_menu.clone(),
        feed_flow: feed_flow.clone(),
        watch_later_flow: watch_later_flow.clone(),
        selected_video: selected_video.clone(),
        badge: wl_badge.clone(),
        feed_cards: feed_cards.clone(),
        watch_later_cards: watch_later_cards.clone(),
    };
    let ui_context = UiContext { async_ctx, widgets };
    create_context_menu(&context_menu, state.clone(), &ui_context);

    // Set initial badge and populate videos
    refresh_video_lists(&state, &ui_context);

    // Refresh button handler
    {
        let state = state.clone();
        let status_label = status_label.clone();
        let spinner = spinner.clone();
        let ui_context = ui_context.clone();
        let subs_file = subs_file.clone();

        refresh_button.connect_clicked(move |_| {
            start_feed_refresh(
                state.clone(),
                ui_context.clone(),
                spinner.clone(),
                status_label.clone(),
                subs_file.clone(),
            );
        });
    }

    // Start thumbnail downloads for visible videos
    let startup_thumbnail_completion = {
        let state_ref = state.borrow();
        download_missing_thumbnails(
            state_ref.videos.values(),
            &state_ref.storage,
            ui_context.async_ctx.http_client.clone(),
            ui_context.async_ctx.runtime.clone(),
        )
    };
    if let Some(startup_thumbnail_completion) = startup_thumbnail_completion {
        let state_for_startup = state.clone();
        let ui_context_for_startup = ui_context.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Ok(video_ids) = startup_thumbnail_completion.recv().await {
                refresh_video_thumbnails(&state_for_startup, &ui_context_for_startup, &video_ids);
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
