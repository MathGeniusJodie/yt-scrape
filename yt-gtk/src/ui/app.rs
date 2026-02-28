use crate::cache::{download_video, fetch_transcript, Storage};
use crate::data::{Tab, Video};
use crate::feed::{fetch_all_feeds, load_channel_ids, FetchProgress};
use crate::gemini::{summarize_video_streaming, StreamingMessage};
use crate::player::play_video;
use crate::ui::dialogs::show_text_dialog;
use crate::ui::video_card::create_video_card;

use futures::stream::{self, StreamExt};
use gio::prelude::*;
use glib::clone;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, FlowBox, HeaderBar, Label, Orientation,
    Popover, ScrolledWindow, Spinner, Stack,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

struct AppState {
    videos: Vec<Video>,
    watch_later: HashSet<String>,
    summaries_in_progress: HashSet<String>,
    current_tab: Tab,
    storage: Storage,
    subs_file: PathBuf,
}

/// Info about the currently selected video (for context menu actions)
#[derive(Clone)]
struct SelectedVideo {
    video_id: String,
    video_title: String,
    video_url: String,
    channel_name: String,
}

fn is_legacy_download(path: &Path) -> bool {
    !matches!(path.extension().and_then(|ext| ext.to_str()), Some("mkv"))
}

fn needs_download_upgrade(storage: &Storage, video_id: &str) -> bool {
    match storage.find_video_path(video_id) {
        Some(path) => is_legacy_download(&path),
        None => true,
    }
}

fn spawn_video_download(runtime: Arc<Runtime>, video_id: String, video_path: PathBuf) {
    std::thread::spawn(move || {
        runtime.block_on(async {
            if let Err(e) = download_video(&video_id, &video_path).await {
                eprintln!("Failed to download video: {}", e);
            }
        });
    });
}

fn resolve_playback_path(
    storage: &Storage,
    runtime: Arc<Runtime>,
    video_id: &str,
    video_title: &str,
) -> Option<PathBuf> {
    let local_path = storage.find_video_path(video_id);
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

pub fn build_ui(app: &Application, subs_file: PathBuf) {
    // Create tokio runtime for async operations
    let runtime = Arc::new(Runtime::new().expect("Failed to create tokio runtime"));

    // Initialize storage
    let storage = Storage::new().expect("Failed to initialize storage");

    // Load cached data
    let videos = storage.load_videos();
    let watch_later = storage.load_watch_later();

    let state = Rc::new(RefCell::new(AppState {
        videos,
        watch_later,
        summaries_in_progress: HashSet::new(),
        current_tab: Tab::Feed,
        storage,
        subs_file,
    }));

    // Selected video for context menu actions
    let selected_video: Rc<RefCell<Option<SelectedVideo>>> = Rc::new(RefCell::new(None));

    // Create main window
    let window = ApplicationWindow::builder()
        .application(app)
        .title("yt-gtk")
        .default_width(1200)
        .default_height(800)
        .build();

    // Apply CSS
    let css_provider = gtk::CssProvider::new();
    if let Err(e) = css_provider.load_from_data(include_bytes!("style.css")) {
        eprintln!("Warning: Failed to load CSS: {}", e);
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

    // Card dimensions for layout calculation
    const CARD_WIDTH: i32 = 320;
    const CARD_SPACING: i32 = 16;
    const GRID_PADDING: i32 = 16;

    // Feed tab
    let feed_scroll = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    feed_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    // Container to center the FlowBox
    let feed_container = GtkBox::new(Orientation::Horizontal, 0);
    feed_container.set_halign(gtk::Align::Center);
    feed_container.set_valign(gtk::Align::Start);

    let feed_flow = FlowBox::new();
    feed_flow.set_widget_name("video-grid");
    feed_flow.set_valign(gtk::Align::Start);
    feed_flow.set_halign(gtk::Align::Center);
    feed_flow.set_max_children_per_line(10);
    feed_flow.set_min_children_per_line(1);
    feed_flow.set_selection_mode(gtk::SelectionMode::Single);
    feed_flow.set_homogeneous(false);
    feed_flow.set_column_spacing(CARD_SPACING as u32);
    feed_flow.set_row_spacing(CARD_SPACING as u32);

    feed_container.pack_start(&feed_flow, false, false, 0);

    // Dynamically adjust FlowBox width based on available space
    let feed_flow_for_resize = feed_flow.clone();
    feed_scroll.connect_size_allocate(move |_widget, allocation| {
        let available_width = allocation.width() - GRID_PADDING * 2;
        let num_columns = ((available_width + CARD_SPACING) / (CARD_WIDTH + CARD_SPACING)).max(1);
        let optimal_width = num_columns * CARD_WIDTH + (num_columns - 1) * CARD_SPACING;
        feed_flow_for_resize.set_size_request(optimal_width, -1);
    });

    feed_scroll.add(&feed_container);
    stack.add_titled(&feed_scroll, "feed", "Feed");

    // Watch Later tab
    let watch_later_scroll = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    watch_later_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    // Container to center the FlowBox
    let watch_later_container = GtkBox::new(Orientation::Horizontal, 0);
    watch_later_container.set_halign(gtk::Align::Center);
    watch_later_container.set_valign(gtk::Align::Start);

    let watch_later_flow = FlowBox::new();
    watch_later_flow.set_widget_name("video-grid");
    watch_later_flow.set_valign(gtk::Align::Start);
    watch_later_flow.set_halign(gtk::Align::Center);
    watch_later_flow.set_max_children_per_line(10);
    watch_later_flow.set_min_children_per_line(1);
    watch_later_flow.set_selection_mode(gtk::SelectionMode::Single);
    watch_later_flow.set_homogeneous(false);
    watch_later_flow.set_column_spacing(CARD_SPACING as u32);
    watch_later_flow.set_row_spacing(CARD_SPACING as u32);

    watch_later_container.pack_start(&watch_later_flow, false, false, 0);

    // Dynamically adjust FlowBox width based on available space
    let watch_later_flow_for_resize = watch_later_flow.clone();
    watch_later_scroll.connect_size_allocate(move |_widget, allocation| {
        let available_width = allocation.width() - GRID_PADDING * 2;
        let num_columns = ((available_width + CARD_SPACING) / (CARD_WIDTH + CARD_SPACING)).max(1);
        let optimal_width = num_columns * CARD_WIDTH + (num_columns - 1) * CARD_SPACING;
        watch_later_flow_for_resize.set_size_request(optimal_width, -1);
    });

    watch_later_scroll.add(&watch_later_container);
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

    // Connect tab buttons to stack
    {
        let stack = stack.clone();
        let watch_later_tab = watch_later_tab.clone();
        let feed_scroll = feed_scroll.clone();
        feed_tab.connect_toggled(move |btn| {
            if btn.is_active() {
                stack.set_visible_child_name("feed");
                watch_later_tab.set_active(false);
                // Trigger column recalculation
                feed_scroll.queue_resize();
            }
        });
    }
    {
        let stack = stack.clone();
        let feed_tab = feed_tab.clone();
        let watch_later_scroll = watch_later_scroll.clone();
        watch_later_tab.connect_toggled(move |btn| {
            if btn.is_active() {
                stack.set_visible_child_name("watch-later");
                feed_tab.set_active(false);
                // Trigger column recalculation
                watch_later_scroll.queue_resize();
            }
        });
    }

    main_box.pack_start(&stack, true, true, 0);
    window.add(&main_box);

    // Create context menu with handlers connected once
    let context_menu = create_context_menu(
        selected_video.clone(),
        state.clone(),
        window.clone(),
        runtime.clone(),
        feed_flow.clone(),
        watch_later_flow.clone(),
        wl_badge.clone(),
    );

    // Set initial badge and populate videos
    update_watch_later_badge(&wl_badge, state.borrow().watch_later.len());
    populate_flow_box(
        &feed_flow,
        &state.borrow(),
        Tab::Feed,
        &context_menu,
        &state,
        &window,
        runtime.clone(),
        feed_flow.clone(),
        watch_later_flow.clone(),
        &selected_video,
        &wl_badge,
    );
    populate_flow_box(
        &watch_later_flow,
        &state.borrow(),
        Tab::WatchLater,
        &context_menu,
        &state,
        &window,
        runtime.clone(),
        feed_flow.clone(),
        watch_later_flow.clone(),
        &selected_video,
        &wl_badge,
    );

    // Track tab changes
    {
        let state = state.clone();
        stack.connect_visible_child_notify(move |stack| {
            let mut state = state.borrow_mut();
            if let Some(name) = stack.visible_child_name() {
                state.current_tab = if name == "watch-later" {
                    Tab::WatchLater
                } else {
                    Tab::Feed
                };
            }
        });
    }

    // Refresh button handler
    {
        let state = state.clone();
        let feed_flow = feed_flow.clone();
        let watch_later_flow = watch_later_flow.clone();
        let status_label = status_label.clone();
        let spinner = spinner.clone();
        let context_menu = context_menu.clone();
        let window = window.clone();
        let runtime = runtime.clone();
        let selected_video = selected_video.clone();
        let wl_badge = wl_badge.clone();

        refresh_button.connect_clicked(move |_| {
            let state_clone = state.clone();
            let feed_flow = feed_flow.clone();
            let watch_later_flow = watch_later_flow.clone();
            let status_label = status_label.clone();
            let spinner = spinner.clone();
            let context_menu = context_menu.clone();
            let window = window.clone();
            let runtime = runtime.clone();
            let selected_video = selected_video.clone();
            let wl_badge = wl_badge.clone();

            spinner.start();
            status_label.set_text("Refreshing...");

            // Get channel IDs
            let subs_file = state.borrow().subs_file.clone();
            let channel_ids = match load_channel_ids(&subs_file) {
                Ok(ids) => ids,
                Err(e) => {
                    spinner.stop();
                    status_label.set_text(&format!("Error: {}", e));
                    return;
                }
            };

            // Create channel for progress updates
            let (tx, mut rx) = mpsc::channel::<FetchProgress>(100);

            // Channel to send fetched videos back to main thread
            #[allow(deprecated)]
            let (videos_tx, videos_rx) = glib::MainContext::channel::<Vec<Video>>(glib::Priority::DEFAULT);

            // Spawn the fetch task
            let runtime_clone = runtime.clone();
            let tx_for_errors = tx.clone();
            std::thread::spawn(move || {
                runtime_clone.block_on(async {
                    match fetch_all_feeds(channel_ids, tx).await {
                        Ok(videos) => {
                            let _ = videos_tx.send(videos);
                        }
                        Err(e) => {
                            let _ = tx_for_errors
                                .send(FetchProgress::Fatal {
                                    error: e.to_string(),
                                })
                                .await;
                        }
                    }
                });
            });

            // Handle progress updates on main thread
            #[allow(deprecated)]
            let (gtx, grx) = glib::MainContext::channel(glib::Priority::DEFAULT);

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    while let Some(progress) = rx.recv().await {
                        let _ = gtx.send(progress);
                    }
                });
            });

            let mut total_channels = 0usize;
            let mut completed_channels = 0usize;
            let mut failed_channels = 0usize;

            grx.attach(None, move |progress| {
                match progress {
                    FetchProgress::Started { total } => {
                        total_channels = total;
                        completed_channels = 0;
                        failed_channels = 0;
                        status_label.set_text(&format!("Fetching feeds (0/{total})..."));
                    }
                    FetchProgress::ChannelComplete { channel, count } => {
                        completed_channels += 1;
                        if failed_channels == 0 {
                            status_label
                                .set_text(&format!("Fetching feeds ({completed_channels}/{total_channels})..."));
                        } else {
                            status_label.set_text(&format!(
                                "Fetching feeds ({completed_channels}/{total_channels}, {failed_channels} failed)..."
                            ));
                        }
                        eprintln!("Fetched {} videos for channel {}", count, channel);
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
                        eprintln!(
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
                        eprintln!("Error fetching {}: {}", channel_id, error);
                    }
                    FetchProgress::Fatal { error } => {
                        spinner.stop();
                        status_label.set_text(&format!("Refresh failed: {}", error));
                        eprintln!("Fatal refresh error: {}", error);
                        return glib::ControlFlow::Break;
                    }
                    FetchProgress::AllComplete {
                        total_videos,
                        successful_channels,
                        failed_channels: final_failed_channels,
                    } => {
                        spinner.stop();
                        if final_failed_channels > 0 {
                            status_label.set_text(&format!(
                                "{} videos loaded ({} channels ok, {} failed)",
                                total_videos, successful_channels, final_failed_channels
                            ));
                        } else {
                            status_label.set_text(&format!("{} videos loaded", total_videos));
                        }
                        return glib::ControlFlow::Break;
                    }
                }
                glib::ControlFlow::Continue
            });

            // Handle fetched videos
            let feed_flow2 = feed_flow.clone();
            let watch_later_flow2 = watch_later_flow.clone();
            let context_menu2 = context_menu.clone();
            let window2 = window.clone();
            let runtime2 = runtime.clone();
            let selected_video2 = selected_video.clone();
            let wl_badge2 = wl_badge.clone();
            videos_rx.attach(None, move |videos| {
                // Save to storage and update state
                let mut videos = videos;
                let mut state = state_clone.borrow_mut();
                merge_cached_video_fields(&mut videos, &state.videos);
                let _ = state.storage.save_videos(&videos);
                state.videos = videos;

                // Start thumbnail downloads
                download_missing_thumbnails(&state.videos, &state.storage, runtime.clone());

                // Repopulate flow boxes
                drop(state);
                let state_ref = state_clone.borrow();
                populate_flow_box(&feed_flow, &state_ref, Tab::Feed, &context_menu, &state_clone, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone(), &selected_video, &wl_badge);
                populate_flow_box(&watch_later_flow, &state_ref, Tab::WatchLater, &context_menu, &state_clone, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone(), &selected_video, &wl_badge);

                // Schedule a refresh after thumbnails have had time to download
                let state_clone2 = state_clone.clone();
                let feed_flow = feed_flow2.clone();
                let watch_later_flow = watch_later_flow2.clone();
                let context_menu = context_menu2.clone();
                let window = window2.clone();
                let runtime = runtime2.clone();
                let selected_video = selected_video2.clone();
                let wl_badge = wl_badge2.clone();
                glib::timeout_add_seconds_local_once(3, move || {
                    let state_ref = state_clone2.borrow();
                    populate_flow_box(&feed_flow, &state_ref, Tab::Feed, &context_menu, &state_clone2, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone(), &selected_video, &wl_badge);
                    populate_flow_box(&watch_later_flow, &state_ref, Tab::WatchLater, &context_menu, &state_clone2, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone(), &selected_video, &wl_badge);
                });

                glib::ControlFlow::Continue
            });
        });
    }

    // Start thumbnail downloads for visible videos
    {
        let state_ref = state.borrow();
        download_missing_thumbnails(&state_ref.videos, &state_ref.storage, runtime.clone());
    }

    window.show_all();
}

fn create_context_menu(
    selected_video: Rc<RefCell<Option<SelectedVideo>>>,
    state_rc: Rc<RefCell<AppState>>,
    window: ApplicationWindow,
    runtime: Arc<Runtime>,
    feed_flow: FlowBox,
    watch_later_flow: FlowBox,
    badge: Label,
) -> Popover {
    let popover = Popover::new(None::<&gtk::Widget>);

    let menu_box = GtkBox::new(Orientation::Vertical, 0);
    menu_box.set_margin_start(8);
    menu_box.set_margin_end(8);
    menu_box.set_margin_top(8);
    menu_box.set_margin_bottom(8);

    let play_button = Button::with_label("Play");
    play_button.set_widget_name("menu-play");
    menu_box.pack_start(&play_button, false, false, 4);

    let watch_later_button = Button::with_label("Toggle Watch Later");
    watch_later_button.set_widget_name("menu-watch-later");
    menu_box.pack_start(&watch_later_button, false, false, 4);

    let summary_button = Button::with_label("AI Summary");
    summary_button.set_widget_name("menu-summary");
    menu_box.pack_start(&summary_button, false, false, 4);

    let transcript_button = Button::with_label("Transcript");
    transcript_button.set_widget_name("menu-transcript");
    menu_box.pack_start(&transcript_button, false, false, 4);

    popover.add(&menu_box);
    menu_box.show_all();

    // Connect handlers once - they read from selected_video
    {
        let selected_video = selected_video.clone();
        let state_rc = state_rc.clone();
        let popover = popover.clone();
        let runtime = runtime.clone();
        play_button.connect_clicked(move |_| {
            if let Some(ref video) = *selected_video.borrow() {
                let state = state_rc.borrow();
                let local_path = resolve_playback_path(
                    &state.storage,
                    runtime.clone(),
                    &video.video_id,
                    &video.video_title,
                );
                if let Err(e) =
                    play_video(&video.video_id, &video.video_title, local_path.as_deref())
                {
                    eprintln!("Failed to play video: {}", e);
                }
            }
            popover.popdown();
        });
    }

    {
        let selected_video = selected_video.clone();
        let state_rc = state_rc.clone();
        let popover_clone = popover.clone();
        let runtime = runtime.clone();
        let feed_flow = feed_flow.clone();
        let watch_later_flow = watch_later_flow.clone();
        let window = window.clone();
        let badge = badge.clone();
        watch_later_button.connect_clicked(move |_| {
            if let Some(ref video) = selected_video.borrow().clone() {
                let mut added_to_watch_later = false;
                {
                    let mut state = state_rc.borrow_mut();
                    if state.watch_later.contains(&video.video_id) {
                        state.watch_later.remove(&video.video_id);
                    } else {
                        state.watch_later.insert(video.video_id.clone());
                        added_to_watch_later = true;

                        // Start download if missing or from the legacy downloader.
                        if needs_download_upgrade(&state.storage, &video.video_id) {
                            let video_path = state
                                .storage
                                .video_path(&video.video_id, &video.video_title);
                            let video_id = video.video_id.clone();
                            spawn_video_download(runtime.clone(), video_id, video_path);
                        }
                    }
                    let _ = state.storage.save_watch_later(&state.watch_later);
                }
                popover_clone.popdown();

                // Update badge and refresh both flow boxes
                let state_ref = state_rc.borrow();
                update_watch_later_badge(&badge, state_ref.watch_later.len());
                populate_flow_box(
                    &feed_flow,
                    &state_ref,
                    Tab::Feed,
                    &popover_clone,
                    &state_rc,
                    &window,
                    runtime.clone(),
                    feed_flow.clone(),
                    watch_later_flow.clone(),
                    &selected_video,
                    &badge,
                );
                populate_flow_box(
                    &watch_later_flow,
                    &state_ref,
                    Tab::WatchLater,
                    &popover_clone,
                    &state_rc,
                    &window,
                    runtime.clone(),
                    feed_flow.clone(),
                    watch_later_flow.clone(),
                    &selected_video,
                    &badge,
                );
                drop(state_ref);

                if added_to_watch_later {
                    maybe_prefetch_summary_for_watch_later(
                        &state_rc,
                        &window,
                        &popover_clone,
                        runtime.clone(),
                        feed_flow.clone(),
                        watch_later_flow.clone(),
                        selected_video.clone(),
                        badge.clone(),
                        video.video_id.clone(),
                        video.video_url.clone(),
                        video.video_title.clone(),
                        video.channel_name.clone(),
                    );
                }
            }
        });
    }

    {
        let selected_video = selected_video.clone();
        let state_rc = state_rc.clone();
        let popover = popover.clone();
        let window = window.clone();
        let runtime = runtime.clone();
        let feed_flow = feed_flow.clone();
        let watch_later_flow = watch_later_flow.clone();
        let badge = badge.clone();
        summary_button.connect_clicked(move |_| {
            if let Some(ref video) = *selected_video.borrow() {
                popover.popdown();
                show_summary_dialog(
                    &window,
                    &state_rc,
                    &popover,
                    runtime.clone(),
                    feed_flow.clone(),
                    watch_later_flow.clone(),
                    selected_video.clone(),
                    badge.clone(),
                    &video.video_id,
                    &video.video_url,
                    &video.video_title,
                    &video.channel_name,
                );
            }
        });
    }

    {
        let selected_video = selected_video.clone();
        let state_rc = state_rc.clone();
        let popover = popover.clone();
        let window = window.clone();
        let runtime = runtime.clone();
        transcript_button.connect_clicked(move |_| {
            if let Some(ref video) = *selected_video.borrow() {
                popover.popdown();
                show_transcript_dialog(
                    &window,
                    &video.video_id,
                    &video.video_title,
                    &state_rc,
                    runtime.clone(),
                );
            }
        });
    }

    popover
}

fn populate_flow_box(
    flow_box: &FlowBox,
    state: &AppState,
    tab: Tab,
    context_menu: &Popover,
    state_rc: &Rc<RefCell<AppState>>,
    _window: &ApplicationWindow,
    runtime: Arc<Runtime>,
    feed_flow: FlowBox,
    watch_later_flow: FlowBox,
    selected_video: &Rc<RefCell<Option<SelectedVideo>>>,
    badge: &Label,
) {
    // Clear existing children
    flow_box.foreach(|child| {
        flow_box.remove(child);
    });

    let videos: Vec<&Video> = match tab {
        Tab::Feed => state.videos.iter().collect(),
        Tab::WatchLater => state
            .videos
            .iter()
            .filter(|v| state.watch_later.contains(&v.video_id))
            .collect(),
    };

    for video in videos {
        let thumbnail_path = state.storage.thumbnail_path(&video.video_id);
        let is_watch_later = state.watch_later.contains(&video.video_id);
        let is_downloaded = state.storage.has_video(&video.video_id);
        let has_ai_summary = has_cached_summary(video);

        let (card, watch_later_toggle, ai_summary_button) = create_video_card(
            video,
            &thumbnail_path,
            is_watch_later,
            is_downloaded,
            has_ai_summary,
        );

        let video_id = video.video_id.clone();
        let video_title = video.title.clone();
        let video_url = video.watch_url();
        let channel_name = video.channel_name.clone();
        let context_menu = context_menu.clone();
        let state_rc = state_rc.clone();
        let selected_video = selected_video.clone();

        // Clone for watch later toggle (before button_press_event moves them)
        let wl_video_id = video_id.clone();
        let wl_video_title = video_title.clone();
        let wl_video_url = video_url.clone();
        let wl_channel_name = channel_name.clone();
        let wl_state_rc = state_rc.clone();
        let wl_runtime = runtime.clone();
        let wl_feed_flow = feed_flow.clone();
        let wl_watch_later_flow = watch_later_flow.clone();
        let wl_context_menu = context_menu.clone();
        let wl_window = _window.clone();
        let wl_selected_video = selected_video.clone();
        let wl_badge = badge.clone();

        if let Some(ai_summary_button) = ai_summary_button {
            let summary_window = _window.clone();
            let summary_state_rc = state_rc.clone();
            let summary_context_menu = context_menu.clone();
            let summary_runtime = runtime.clone();
            let summary_feed_flow = feed_flow.clone();
            let summary_watch_later_flow = watch_later_flow.clone();
            let summary_selected_video = selected_video.clone();
            let summary_badge = badge.clone();
            let summary_video_id = video_id.clone();
            let summary_video_url = video_url.clone();
            let summary_video_title = video_title.clone();
            let summary_channel_name = channel_name.clone();

            ai_summary_button.connect_clicked(move |_| {
                show_summary_dialog(
                    &summary_window,
                    &summary_state_rc,
                    &summary_context_menu,
                    summary_runtime.clone(),
                    summary_feed_flow.clone(),
                    summary_watch_later_flow.clone(),
                    summary_selected_video.clone(),
                    summary_badge.clone(),
                    &summary_video_id,
                    &summary_video_url,
                    &summary_video_title,
                    &summary_channel_name,
                );
            });
        }

        // Double-click to play, right-click for context menu
        card.connect_button_press_event(
            clone!(@strong video_id, @strong video_title, @strong state_rc, @strong runtime => move |widget, event| {
                if event.button() == 1 && event.event_type() == gdk::EventType::DoubleButtonPress {
                    // Play video
                    let state = state_rc.borrow();
                    let local_path = resolve_playback_path(
                        &state.storage,
                        runtime.clone(),
                        &video_id,
                        &video_title,
                    );
                    if let Err(e) = play_video(&video_id, &video_title, local_path.as_deref()) {
                        eprintln!("Failed to play video: {}", e);
                    }
                    return glib::Propagation::Stop;
                }

                if event.button() == 3 {
                    // Right-click - set selected video and show context menu
                    *selected_video.borrow_mut() = Some(SelectedVideo {
                        video_id: video_id.clone(),
                        video_title: video_title.clone(),
                        video_url: video_url.clone(),
                        channel_name: channel_name.clone(),
                    });
                    context_menu.set_relative_to(Some(widget));
                    context_menu.popup();
                    return glib::Propagation::Stop;
                }

                glib::Propagation::Proceed
            }),
        );

        // Watch later toggle button handler
        watch_later_toggle.connect_clicked(move |_| {
            let mut added_to_watch_later = false;
            {
                let mut state = wl_state_rc.borrow_mut();
                if state.watch_later.contains(&wl_video_id) {
                    state.watch_later.remove(&wl_video_id);
                } else {
                    state.watch_later.insert(wl_video_id.clone());
                    added_to_watch_later = true;

                    // Start download if missing or from the legacy downloader.
                    if needs_download_upgrade(&state.storage, &wl_video_id) {
                        let video_path = state.storage.video_path(&wl_video_id, &wl_video_title);
                        let video_id = wl_video_id.clone();
                        spawn_video_download(wl_runtime.clone(), video_id, video_path);
                    }
                }
                let _ = state.storage.save_watch_later(&state.watch_later);
            }

            // Update badge and refresh UI
            let state_ref = wl_state_rc.borrow();
            update_watch_later_badge(&wl_badge, state_ref.watch_later.len());
            populate_flow_box(
                &wl_feed_flow,
                &state_ref,
                Tab::Feed,
                &wl_context_menu,
                &wl_state_rc,
                &wl_window,
                wl_runtime.clone(),
                wl_feed_flow.clone(),
                wl_watch_later_flow.clone(),
                &wl_selected_video,
                &wl_badge,
            );
            populate_flow_box(
                &wl_watch_later_flow,
                &state_ref,
                Tab::WatchLater,
                &wl_context_menu,
                &wl_state_rc,
                &wl_window,
                wl_runtime.clone(),
                wl_feed_flow.clone(),
                wl_watch_later_flow.clone(),
                &wl_selected_video,
                &wl_badge,
            );
            drop(state_ref);

            if added_to_watch_later {
                maybe_prefetch_summary_for_watch_later(
                    &wl_state_rc,
                    &wl_window,
                    &wl_context_menu,
                    wl_runtime.clone(),
                    wl_feed_flow.clone(),
                    wl_watch_later_flow.clone(),
                    wl_selected_video.clone(),
                    wl_badge.clone(),
                    wl_video_id.clone(),
                    wl_video_url.clone(),
                    wl_video_title.clone(),
                    wl_channel_name.clone(),
                );
            }
        });

        flow_box.add(&card);

        // Configure the FlowBoxChild to not expand
        if let Some(child) = card.parent() {
            if let Ok(flow_child) = child.downcast::<gtk::FlowBoxChild>() {
                flow_child.set_hexpand(false);
                flow_child.set_halign(gtk::Align::Start);
            }
        }
    }

    flow_box.show_all();
}

fn update_watch_later_badge(badge: &Label, count: usize) {
    if count > 0 {
        badge.set_text(&count.to_string());
        badge.show();
    } else {
        badge.hide();
    }
}

fn has_cached_summary(video: &Video) -> bool {
    video
        .ai_summary
        .as_ref()
        .map(|summary| !summary.trim().is_empty())
        .unwrap_or(false)
}

fn merge_cached_video_fields(videos: &mut [Video], cached_videos: &[Video]) {
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

#[allow(clippy::too_many_arguments)]
fn refresh_video_lists(
    state_rc: &Rc<RefCell<AppState>>,
    context_menu: &Popover,
    window: &ApplicationWindow,
    runtime: Arc<Runtime>,
    feed_flow: &FlowBox,
    watch_later_flow: &FlowBox,
    selected_video: &Rc<RefCell<Option<SelectedVideo>>>,
    badge: &Label,
) {
    let state_ref = state_rc.borrow();
    update_watch_later_badge(badge, state_ref.watch_later.len());
    populate_flow_box(
        feed_flow,
        &state_ref,
        Tab::Feed,
        context_menu,
        state_rc,
        window,
        runtime.clone(),
        feed_flow.clone(),
        watch_later_flow.clone(),
        selected_video,
        badge,
    );
    populate_flow_box(
        watch_later_flow,
        &state_ref,
        Tab::WatchLater,
        context_menu,
        state_rc,
        window,
        runtime,
        feed_flow.clone(),
        watch_later_flow.clone(),
        selected_video,
        badge,
    );
}

fn download_missing_thumbnails(videos: &[Video], storage: &Storage, runtime: Arc<Runtime>) {
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
                            eprintln!("Thumbnail request failed for {}: {}", url, error);
                            return;
                        }
                    };

                    let response = match response.error_for_status() {
                        Ok(response) => response,
                        Err(error) => {
                            eprintln!("Thumbnail response failed for {}: {}", url, error);
                            return;
                        }
                    };

                    match response.bytes().await {
                        Ok(bytes) => {
                            if let Err(error) = tokio::fs::write(&path, &bytes).await {
                                eprintln!(
                                    "Failed writing thumbnail to {}: {}",
                                    path.display(),
                                    error
                                );
                            }
                        }
                        Err(error) => {
                            eprintln!("Failed reading thumbnail bytes for {}: {}", url, error);
                        }
                    }
                }
            })
            .await;
    });
}

#[allow(deprecated)]
fn spawn_summary_generation(
    runtime: Arc<Runtime>,
    video_id: String,
    video_url: String,
    video_title: String,
    channel_name: String,
    transcripts_work_dir: PathBuf,
) -> glib::Receiver<Result<String, String>> {
    let (gtx, grx) = glib::MainContext::channel::<Result<String, String>>(glib::Priority::DEFAULT);

    std::thread::spawn(move || {
        let result = runtime.block_on(async {
            let (tx, mut rx) = mpsc::unbounded_channel();
            summarize_video_streaming(
                &video_id,
                &video_url,
                &video_title,
                &channel_name,
                &transcripts_work_dir,
                tx,
            )
            .await;

            let mut summary = String::new();
            let mut error: Option<String> = None;

            while let Some(message) = rx.recv().await {
                match message {
                    StreamingMessage::Chunk(text) => summary.push_str(&text),
                    StreamingMessage::Done => {}
                    StreamingMessage::Error(err) => error = Some(err),
                }
            }

            if let Some(err) = error {
                Err(err)
            } else {
                let summary = summary.trim().to_string();
                if summary.is_empty() {
                    Err("Summary was empty".to_string())
                } else {
                    Ok(summary)
                }
            }
        });

        let _ = gtx.send(result);
    });

    grx
}

fn persist_summary_to_cache(
    state_rc: &Rc<RefCell<AppState>>,
    video_id: &str,
    summary: String,
) -> bool {
    let mut state = state_rc.borrow_mut();
    state.summaries_in_progress.remove(video_id);

    let mut updated = false;
    if let Some(video) = state
        .videos
        .iter_mut()
        .find(|video| video.video_id == video_id)
    {
        video.ai_summary = Some(summary);
        updated = true;
    }

    if updated {
        let _ = state.storage.save_videos(&state.videos);
    }

    updated
}

#[allow(clippy::too_many_arguments)]
fn maybe_prefetch_summary_for_watch_later(
    state_rc: &Rc<RefCell<AppState>>,
    window: &ApplicationWindow,
    context_menu: &Popover,
    runtime: Arc<Runtime>,
    feed_flow: FlowBox,
    watch_later_flow: FlowBox,
    selected_video: Rc<RefCell<Option<SelectedVideo>>>,
    badge: Label,
    video_id: String,
    video_url: String,
    video_title: String,
    channel_name: String,
) {
    let should_prefetch = {
        let mut state = state_rc.borrow_mut();
        let has_summary = state
            .videos
            .iter()
            .find(|video| video.video_id == video_id)
            .map(has_cached_summary)
            .unwrap_or(false);

        if has_summary || state.summaries_in_progress.contains(&video_id) {
            false
        } else {
            state.summaries_in_progress.insert(video_id.clone());
            true
        }
    };

    if !should_prefetch {
        return;
    }

    let transcripts_work_dir = state_rc.borrow().storage.transcripts_work_dir().clone();
    let result_rx = spawn_summary_generation(
        runtime.clone(),
        video_id.clone(),
        video_url,
        video_title,
        channel_name,
        transcripts_work_dir,
    );

    let state_for_result = state_rc.clone();
    let window_for_result = window.clone();
    let context_menu_for_result = context_menu.clone();
    let feed_flow_for_result = feed_flow.clone();
    let watch_later_flow_for_result = watch_later_flow.clone();
    let selected_video_for_result = selected_video.clone();
    let badge_for_result = badge.clone();
    let video_id_for_result = video_id.clone();

    result_rx.attach(None, move |result| {
        match result {
            Ok(summary) => {
                if persist_summary_to_cache(&state_for_result, &video_id_for_result, summary) {
                    refresh_video_lists(
                        &state_for_result,
                        &context_menu_for_result,
                        &window_for_result,
                        runtime.clone(),
                        &feed_flow_for_result,
                        &watch_later_flow_for_result,
                        &selected_video_for_result,
                        &badge_for_result,
                    );
                }
            }
            Err(error) => {
                state_for_result
                    .borrow_mut()
                    .summaries_in_progress
                    .remove(&video_id_for_result);
                eprintln!(
                    "Failed to prefetch summary for {}: {}",
                    video_id_for_result, error
                );
            }
        }

        glib::ControlFlow::Break
    });
}

#[allow(clippy::too_many_arguments)]
fn start_summary_generation_for_dialog(
    state_rc: Rc<RefCell<AppState>>,
    window: ApplicationWindow,
    context_menu: Popover,
    runtime: Arc<Runtime>,
    feed_flow: FlowBox,
    watch_later_flow: FlowBox,
    selected_video: Rc<RefCell<Option<SelectedVideo>>>,
    badge: Label,
    video_id: String,
    video_url: String,
    video_title: String,
    channel_name: String,
    buffer: gtk::TextBuffer,
    regenerate_button: Button,
    loading_text: &str,
) {
    buffer.set_text(loading_text);
    regenerate_button.set_sensitive(false);

    state_rc
        .borrow_mut()
        .summaries_in_progress
        .insert(video_id.clone());

    let transcripts_work_dir = state_rc.borrow().storage.transcripts_work_dir().clone();
    let result_rx = spawn_summary_generation(
        runtime.clone(),
        video_id.clone(),
        video_url,
        video_title,
        channel_name,
        transcripts_work_dir,
    );

    let state_for_result = state_rc.clone();
    let window_for_result = window.clone();
    let context_menu_for_result = context_menu.clone();
    let feed_flow_for_result = feed_flow.clone();
    let watch_later_flow_for_result = watch_later_flow.clone();
    let selected_video_for_result = selected_video.clone();
    let badge_for_result = badge.clone();
    let video_id_for_result = video_id.clone();
    let buffer_for_result = buffer.clone();
    let button_for_result = regenerate_button.clone();

    result_rx.attach(None, move |result| {
        button_for_result.set_sensitive(true);

        match result {
            Ok(summary) => {
                buffer_for_result.set_text(&summary);
                if persist_summary_to_cache(&state_for_result, &video_id_for_result, summary) {
                    refresh_video_lists(
                        &state_for_result,
                        &context_menu_for_result,
                        &window_for_result,
                        runtime.clone(),
                        &feed_flow_for_result,
                        &watch_later_flow_for_result,
                        &selected_video_for_result,
                        &badge_for_result,
                    );
                }
            }
            Err(error) => {
                state_for_result
                    .borrow_mut()
                    .summaries_in_progress
                    .remove(&video_id_for_result);
                buffer_for_result.set_text(&format!("Error: {}", error));
            }
        }

        glib::ControlFlow::Break
    });
}

#[allow(clippy::too_many_arguments)]
fn show_summary_dialog(
    window: &ApplicationWindow,
    state_rc: &Rc<RefCell<AppState>>,
    context_menu: &Popover,
    runtime: Arc<Runtime>,
    feed_flow: FlowBox,
    watch_later_flow: FlowBox,
    selected_video: Rc<RefCell<Option<SelectedVideo>>>,
    badge: Label,
    video_id: &str,
    video_url: &str,
    video_title: &str,
    channel_name: &str,
) {
    let dialog = gtk::Dialog::with_buttons(
        Some(&format!("Summary: {}", video_title)),
        Some(window),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[("Close", gtk::ResponseType::Close)],
    );
    dialog.set_default_size(700, 500);

    let content_area = dialog.content_area();

    let controls_row = GtkBox::new(Orientation::Horizontal, 8);
    controls_row.set_margin_start(8);
    controls_row.set_margin_end(8);
    controls_row.set_margin_top(8);

    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    controls_row.pack_start(&spacer, true, true, 0);

    let regenerate_button = Button::with_label("Regenerate Summary");
    controls_row.pack_end(&regenerate_button, false, false, 0);
    content_area.pack_start(&controls_row, false, false, 0);

    let scrolled = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);

    let text_view = gtk::TextView::new();
    text_view.set_editable(false);
    text_view.set_wrap_mode(gtk::WrapMode::Word);
    text_view.set_left_margin(12);
    text_view.set_right_margin(12);
    text_view.set_top_margin(12);
    text_view.set_bottom_margin(12);

    let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
    text_view.set_buffer(Some(&buffer));

    scrolled.add(&text_view);
    content_area.pack_start(&scrolled, true, true, 0);

    let state_rc_for_dialog = state_rc.clone();
    let context_menu_for_dialog = context_menu.clone();
    let window_for_dialog = window.clone();
    let video_id = video_id.to_string();
    let video_url = video_url.to_string();
    let video_title = video_title.to_string();
    let channel_name = channel_name.to_string();

    let cached_summary = {
        let state = state_rc.borrow();
        state
            .videos
            .iter()
            .find(|video| video.video_id == video_id)
            .and_then(|video| video.ai_summary.clone())
            .filter(|summary| !summary.trim().is_empty())
    };

    if let Some(summary) = cached_summary {
        buffer.set_text(&summary);
    } else {
        start_summary_generation_for_dialog(
            state_rc_for_dialog.clone(),
            window_for_dialog.clone(),
            context_menu_for_dialog.clone(),
            runtime.clone(),
            feed_flow.clone(),
            watch_later_flow.clone(),
            selected_video.clone(),
            badge.clone(),
            video_id.clone(),
            video_url.clone(),
            video_title.clone(),
            channel_name.clone(),
            buffer.clone(),
            regenerate_button.clone(),
            "Loading summary...",
        );
    }

    {
        let state_rc_for_click = state_rc_for_dialog.clone();
        let window_for_click = window_for_dialog.clone();
        let context_menu_for_click = context_menu_for_dialog.clone();
        let runtime_for_click = runtime.clone();
        let feed_flow_for_click = feed_flow.clone();
        let watch_later_flow_for_click = watch_later_flow.clone();
        let selected_video_for_click = selected_video.clone();
        let badge_for_click = badge.clone();
        let buffer_for_click = buffer.clone();
        let regenerate_button_for_click = regenerate_button.clone();
        let video_id_for_click = video_id.clone();
        let video_url_for_click = video_url.clone();
        let video_title_for_click = video_title.clone();
        let channel_name_for_click = channel_name.clone();

        regenerate_button.connect_clicked(move |_| {
            start_summary_generation_for_dialog(
                state_rc_for_click.clone(),
                window_for_click.clone(),
                context_menu_for_click.clone(),
                runtime_for_click.clone(),
                feed_flow_for_click.clone(),
                watch_later_flow_for_click.clone(),
                selected_video_for_click.clone(),
                badge_for_click.clone(),
                video_id_for_click.clone(),
                video_url_for_click.clone(),
                video_title_for_click.clone(),
                channel_name_for_click.clone(),
                buffer_for_click.clone(),
                regenerate_button_for_click.clone(),
                "Regenerating summary...",
            );
        });
    }

    dialog.show_all();

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });
}

fn show_transcript_dialog(
    window: &ApplicationWindow,
    video_id: &str,
    video_title: &str,
    state_rc: &Rc<RefCell<AppState>>,
    runtime: Arc<Runtime>,
) {
    // Check if we already have the transcript cached
    {
        let state = state_rc.borrow();
        if let Some(video) = state.videos.iter().find(|v| v.video_id == video_id) {
            if let Some(transcript) = &video.transcript {
                show_text_dialog(window, &format!("Transcript: {}", video_title), transcript);
                return;
            }
        }
    }

    // Need to fetch transcript
    let dialog = gtk::Dialog::with_buttons(
        Some(&format!("Transcript: {}", video_title)),
        Some(window),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[("Close", gtk::ResponseType::Close)],
    );
    dialog.set_default_size(700, 500);

    let content_area = dialog.content_area();

    let scrolled = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);

    let text_view = gtk::TextView::new();
    text_view.set_editable(false);
    text_view.set_wrap_mode(gtk::WrapMode::Word);
    text_view.set_left_margin(12);
    text_view.set_right_margin(12);
    text_view.set_top_margin(12);
    text_view.set_bottom_margin(12);

    let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
    buffer.set_text("Loading transcript...");
    text_view.set_buffer(Some(&buffer));

    scrolled.add(&text_view);
    content_area.pack_start(&scrolled, true, true, 0);

    dialog.show_all();

    // Fetch transcript
    let work_dir = state_rc.borrow().storage.transcripts_work_dir().clone();

    #[allow(deprecated)]
    let (gtx, grx) =
        glib::MainContext::channel::<(String, Result<String, String>)>(glib::Priority::DEFAULT);

    let video_id_for_thread = video_id.to_string();
    std::thread::spawn(move || {
        runtime.block_on(async {
            match fetch_transcript(&video_id_for_thread, &work_dir).await {
                Ok(transcript) => {
                    let _ = gtx.send((video_id_for_thread, Ok(transcript)));
                }
                Err(e) => {
                    let _ = gtx.send((video_id_for_thread, Err(e.to_string())));
                }
            }
        });
    });

    let state_rc = state_rc.clone();
    grx.attach(None, move |result| {
        let (vid, res) = result;
        match res {
            Ok(transcript) => {
                buffer.set_text(&transcript);
                // Save to cache on main thread
                let mut state = state_rc.borrow_mut();
                // Update video transcript
                if let Some(video) = state.videos.iter_mut().find(|v| v.video_id == vid) {
                    video.transcript = Some(transcript);
                }
                // Save to disk
                let _ = state.storage.save_videos(&state.videos);
            }
            Err(e) => {
                buffer.set_text(&format!("Error: {}", e));
            }
        }
        glib::ControlFlow::Continue
    });

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });
}
