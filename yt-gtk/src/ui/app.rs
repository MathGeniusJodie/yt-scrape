use crate::cache::{download_video, fetch_transcript, Storage};
use crate::data::{Tab, Video};
use crate::feed::{fetch_all_feeds, load_channel_ids, FetchProgress};
use crate::gemini::{summarize_video_streaming, StreamingMessage};
use crate::player::play_video;
use crate::ui::dialogs::show_text_dialog;
use crate::ui::video_card::create_video_card;

use gio::prelude::*;
use glib::clone;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, FlowBox, HeaderBar, Label,
    Orientation, Popover, ScrolledWindow, Spinner, Stack, StackSwitcher,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

struct AppState {
    videos: Vec<Video>,
    watch_later: HashSet<String>,
    current_tab: Tab,
    storage: Storage,
    subs_file: PathBuf,
    #[allow(dead_code)]
    selected_video_id: Option<String>,
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
        current_tab: Tab::Feed,
        storage,
        subs_file,
        selected_video_id: None,
    }));

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

    // Refresh button
    let refresh_button = Button::with_label("Refresh");
    refresh_button.set_widget_name("refresh-button");
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

    // Feed tab
    let feed_scroll = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    feed_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    let feed_flow = FlowBox::new();
    feed_flow.set_widget_name("video-grid");
    feed_flow.set_valign(gtk::Align::Start);
    feed_flow.set_max_children_per_line(10);
    feed_flow.set_min_children_per_line(1);
    feed_flow.set_selection_mode(gtk::SelectionMode::Single);
    feed_flow.set_homogeneous(true);
    feed_flow.set_column_spacing(8);
    feed_flow.set_row_spacing(8);

    feed_scroll.add(&feed_flow);
    stack.add_titled(&feed_scroll, "feed", "Feed");

    // Watch Later tab
    let watch_later_scroll = ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    watch_later_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    let watch_later_flow = FlowBox::new();
    watch_later_flow.set_widget_name("video-grid");
    watch_later_flow.set_valign(gtk::Align::Start);
    watch_later_flow.set_max_children_per_line(10);
    watch_later_flow.set_min_children_per_line(1);
    watch_later_flow.set_selection_mode(gtk::SelectionMode::Single);
    watch_later_flow.set_homogeneous(true);
    watch_later_flow.set_column_spacing(8);
    watch_later_flow.set_row_spacing(8);

    watch_later_scroll.add(&watch_later_flow);
    stack.add_titled(&watch_later_scroll, "watch-later", "Watch Later");

    // Stack switcher
    let stack_switcher = StackSwitcher::new();
    stack_switcher.set_stack(Some(&stack));
    header.set_custom_title(Some(&stack_switcher));

    main_box.pack_start(&stack, true, true, 0);
    window.add(&main_box);

    // Create context menu
    let context_menu = create_context_menu();

    // Populate initial videos
    populate_flow_box(&feed_flow, &state.borrow(), Tab::Feed, &context_menu, &state, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone());
    populate_flow_box(&watch_later_flow, &state.borrow(), Tab::WatchLater, &context_menu, &state, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone());

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

        refresh_button.connect_clicked(move |_| {
            let state_clone = state.clone();
            let feed_flow = feed_flow.clone();
            let watch_later_flow = watch_later_flow.clone();
            let status_label = status_label.clone();
            let spinner = spinner.clone();
            let context_menu = context_menu.clone();
            let window = window.clone();
            let runtime = runtime.clone();

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
            std::thread::spawn(move || {
                runtime_clone.block_on(async {
                    match fetch_all_feeds(channel_ids, tx).await {
                        Ok(videos) => {
                            let _ = videos_tx.send(videos);
                        }
                        Err(e) => {
                            eprintln!("Fetch error: {}", e);
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

            grx.attach(None, move |progress| {
                match progress {
                    FetchProgress::Started { total } => {
                        status_label.set_text(&format!("Fetching {} channels...", total));
                    }
                    FetchProgress::ChannelComplete { channel: _, count: _ } => {
                        // Could show per-channel progress
                    }
                    FetchProgress::Error { channel_id, error } => {
                        eprintln!("Error fetching {}: {}", channel_id, error);
                    }
                    FetchProgress::AllComplete { total_videos } => {
                        spinner.stop();
                        status_label.set_text(&format!("{} videos loaded", total_videos));
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
            videos_rx.attach(None, move |videos| {
                // Save to storage and update state
                let mut state = state_clone.borrow_mut();
                let _ = state.storage.save_videos(&videos);
                state.videos = videos;

                // Start thumbnail downloads
                download_missing_thumbnails(&state.videos, &state.storage);

                // Repopulate flow boxes
                drop(state);
                let state_ref = state_clone.borrow();
                populate_flow_box(&feed_flow, &state_ref, Tab::Feed, &context_menu, &state_clone, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone());
                populate_flow_box(&watch_later_flow, &state_ref, Tab::WatchLater, &context_menu, &state_clone, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone());

                // Schedule a refresh after thumbnails have had time to download
                let state_clone2 = state_clone.clone();
                let feed_flow = feed_flow2.clone();
                let watch_later_flow = watch_later_flow2.clone();
                let context_menu = context_menu2.clone();
                let window = window2.clone();
                let runtime = runtime2.clone();
                glib::timeout_add_seconds_local_once(3, move || {
                    let state_ref = state_clone2.borrow();
                    populate_flow_box(&feed_flow, &state_ref, Tab::Feed, &context_menu, &state_clone2, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone());
                    populate_flow_box(&watch_later_flow, &state_ref, Tab::WatchLater, &context_menu, &state_clone2, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone());
                });

                glib::ControlFlow::Continue
            });
        });
    }

    // Start thumbnail downloads for visible videos
    {
        let state_ref = state.borrow();
        download_missing_thumbnails(&state_ref.videos, &state_ref.storage);
    }

    window.show_all();
}

fn create_context_menu() -> Popover {
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

    popover
}

fn populate_flow_box(
    flow_box: &FlowBox,
    state: &AppState,
    tab: Tab,
    context_menu: &Popover,
    state_rc: &Rc<RefCell<AppState>>,
    window: &ApplicationWindow,
    runtime: Arc<Runtime>,
    feed_flow: FlowBox,
    watch_later_flow: FlowBox,
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

        let card = create_video_card(video, &thumbnail_path, is_watch_later, is_downloaded);

        let video_id = video.video_id.clone();
        let video_title = video.title.clone();
        let video_url = video.watch_url();
        let channel_name = video.channel_name.clone();
        let context_menu = context_menu.clone();
        let state_rc = state_rc.clone();
        let window = window.clone();
        let runtime = runtime.clone();
        let feed_flow_for_handler = feed_flow.clone();
        let watch_later_flow_for_handler = watch_later_flow.clone();

        // Double-click to play
        card.connect_button_press_event(clone!(@strong video_id, @strong video_title, @strong state_rc => move |widget, event| {
            if event.button() == 1 && event.event_type() == gdk::EventType::DoubleButtonPress {
                // Play video
                let state = state_rc.borrow();
                let local_path = state.storage.find_video_path(&video_id);
                if let Err(e) = play_video(&video_id, &video_title, local_path.as_deref()) {
                    eprintln!("Failed to play video: {}", e);
                }
                return glib::Propagation::Stop;
            }

            if event.button() == 3 {
                // Right-click - show context menu
                context_menu.set_relative_to(Some(widget));
                context_menu.popup();

                // Wire up menu actions for this video
                let menu_box = context_menu.child().unwrap().downcast::<GtkBox>().unwrap();
                let children: Vec<_> = menu_box.children();

                // Play button
                if let Some(play_btn) = children.get(0) {
                    let play_btn = play_btn.clone().downcast::<Button>().unwrap();
                    let video_id = video_id.clone();
                    let video_title = video_title.clone();
                    let state_rc = state_rc.clone();
                    let context_menu = context_menu.clone();

                    // Disconnect old handlers
                    play_btn.connect_clicked(move |_| {
                        let state = state_rc.borrow();
                        let local_path = state.storage.find_video_path(&video_id);
                        if let Err(e) = play_video(&video_id, &video_title, local_path.as_deref()) {
                            eprintln!("Failed to play video: {}", e);
                        }
                        context_menu.popdown();
                    });
                }

                // Watch Later button
                if let Some(wl_btn) = children.get(1) {
                    let wl_btn = wl_btn.clone().downcast::<Button>().unwrap();
                    let video_id = video_id.clone();
                    let video_title = video_title.clone();
                    let state_rc = state_rc.clone();
                    let context_menu = context_menu.clone();
                    let runtime = runtime.clone();
                    let feed_flow = feed_flow_for_handler.clone();
                    let watch_later_flow = watch_later_flow_for_handler.clone();
                    let window = window.clone();

                    wl_btn.connect_clicked(move |_| {
                        {
                            let mut state = state_rc.borrow_mut();
                            if state.watch_later.contains(&video_id) {
                                state.watch_later.remove(&video_id);
                            } else {
                                state.watch_later.insert(video_id.clone());

                                // Start download if not already downloaded
                                if !state.storage.has_video(&video_id) {
                                    let video_path = state.storage.video_path(&video_id, &video_title);
                                    let video_id = video_id.clone();
                                    let runtime = runtime.clone();

                                    std::thread::spawn(move || {
                                        runtime.block_on(async {
                                            if let Err(e) = download_video(&video_id, &video_path).await {
                                                eprintln!("Failed to download video: {}", e);
                                            }
                                        });
                                    });
                                }
                            }
                            let _ = state.storage.save_watch_later(&state.watch_later);
                        }
                        context_menu.popdown();

                        // Refresh both flow boxes to show updated status
                        let state_ref = state_rc.borrow();
                        populate_flow_box(&feed_flow, &state_ref, Tab::Feed, &context_menu, &state_rc, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone());
                        populate_flow_box(&watch_later_flow, &state_ref, Tab::WatchLater, &context_menu, &state_rc, &window, runtime.clone(), feed_flow.clone(), watch_later_flow.clone());
                    });
                }

                // Summary button
                if let Some(summary_btn) = children.get(2) {
                    let summary_btn = summary_btn.clone().downcast::<Button>().unwrap();
                    let video_url = video_url.clone();
                    let video_title = video_title.clone();
                    let channel_name = channel_name.clone();
                    let window = window.clone();
                    let context_menu = context_menu.clone();
                    let runtime = runtime.clone();

                    summary_btn.connect_clicked(move |_| {
                        context_menu.popdown();
                        show_summary_dialog(&window, &video_url, &video_title, &channel_name, runtime.clone());
                    });
                }

                // Transcript button
                if let Some(transcript_btn) = children.get(3) {
                    let transcript_btn = transcript_btn.clone().downcast::<Button>().unwrap();
                    let video_id = video_id.clone();
                    let video_title = video_title.clone();
                    let state_rc = state_rc.clone();
                    let window = window.clone();
                    let context_menu = context_menu.clone();
                    let runtime = runtime.clone();

                    transcript_btn.connect_clicked(move |_| {
                        context_menu.popdown();
                        show_transcript_dialog(&window, &video_id, &video_title, &state_rc, runtime.clone());
                    });
                }

                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        }));

        flow_box.add(&card);
    }

    flow_box.show_all();
}

fn download_missing_thumbnails(videos: &[Video], storage: &Storage) {
    let client = reqwest::Client::new();

    for video in videos.iter().take(50) {
        let thumbnail_path = storage.thumbnail_path(&video.video_id);
        if thumbnail_path.exists() {
            continue;
        }

        let url = video.thumbnail_url.clone();
        let path = thumbnail_path.clone();
        let client = client.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                if let Ok(response) = client.get(&url).send().await {
                    if let Ok(bytes) = response.bytes().await {
                        let _ = std::fs::write(&path, &bytes);
                    }
                }
            });
        });
    }
}

fn show_summary_dialog(
    window: &ApplicationWindow,
    video_url: &str,
    video_title: &str,
    channel_name: &str,
    runtime: Arc<Runtime>,
) {
    let dialog = gtk::Dialog::with_buttons(
        Some(&format!("Summary: {}", video_title)),
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
    buffer.set_text("Loading summary...");
    text_view.set_buffer(Some(&buffer));

    scrolled.add(&text_view);
    content_area.pack_start(&scrolled, true, true, 0);

    dialog.show_all();

    // Start streaming summary
    let (tx, mut rx) = mpsc::unbounded_channel();
    let video_url = video_url.to_string();
    let video_title = video_title.to_string();
    let channel_name = channel_name.to_string();

    std::thread::spawn(move || {
        runtime.block_on(async {
            summarize_video_streaming(&video_url, &video_title, &channel_name, tx).await;
        });
    });

    // Handle streaming updates
    #[allow(deprecated)]
    let (gtx, grx) = glib::MainContext::channel(glib::Priority::DEFAULT);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            while let Some(msg) = rx.recv().await {
                let _ = gtx.send(msg);
            }
        });
    });

    let accumulated = Rc::new(RefCell::new(String::new()));

    grx.attach(None, move |msg| {
        match msg {
            StreamingMessage::Chunk(text) => {
                accumulated.borrow_mut().push_str(&text);
                buffer.set_text(&accumulated.borrow());
            }
            StreamingMessage::Done => {
                // Summary complete
            }
            StreamingMessage::Error(e) => {
                buffer.set_text(&format!("Error: {}", e));
            }
        }
        glib::ControlFlow::Continue
    });

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
    let (gtx, grx) = glib::MainContext::channel::<(String, Result<String, String>)>(glib::Priority::DEFAULT);

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
