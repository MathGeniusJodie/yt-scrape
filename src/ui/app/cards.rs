use super::summary_generator::{show_summary_dialog, show_transcript_dialog};
use super::{apply_watch_later_action, resolve_playback_path, AppContext, AppState};
use crate::cache::Storage;
use crate::data::{Tab, Video};
use crate::player::play_video;

use chrono::Utc;
use chrono_humanize::{Accuracy, HumanTime, Tense};
use futures::stream::{self, StreamExt};
use gdk_pixbuf::Pixbuf;
use glib::clone;
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, DrawingArea, EventBox, FlowBox, Label, Orientation, Popover,
};
use log::{error, warn};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tokio::runtime::Runtime;

fn on_menu_action<F>(
    button: &Button,
    selected_video: Rc<RefCell<Option<String>>>,
    context_menu: Popover,
    action: F,
) where
    F: Fn(String) + 'static,
{
    button.connect_clicked(move |_| {
        context_menu.popdown();
        if let Some(video_id) = selected_video.borrow().clone() {
            action(video_id);
        }
    });
}

fn play_selected_video(state_rc: &Rc<RefCell<AppState>>, runtime: &Arc<Runtime>, video_id: &str) {
    let playback = {
        let state = state_rc.borrow();
        state.video_by_id(video_id).map(|current_video| {
            let video_title = current_video.title().to_string();
            let local_path = state.storage.find_video_path(video_id);
            let local_path = resolve_playback_path(
                &state.storage,
                runtime.clone(),
                video_id,
                &video_title,
                local_path,
            );
            (video_title, local_path)
        })
    };

    if let Some((video_title, local_path)) = playback {
        if let Err(play_error) = play_video(video_id, &video_title, local_path.as_deref()) {
            error!("Failed to play video {}: {}", video_id, play_error);
        }
    } else {
        error!("Cannot play missing video {}", video_id);
    }
}

pub(super) fn create_context_menu(
    popover: &Popover,
    state_rc: Rc<RefCell<AppState>>,
    ui_context: &AppContext,
) {
    let popover = popover.clone();
    let selected_video = ui_context.selected_video.clone();

    let builder = gtk::Builder::from_string(include_str!("context_menu.ui"));
    let menu_box = builder
        .object::<GtkBox>("menu_box")
        .expect("menu_box in context_menu.ui");
    let play_button = builder
        .object::<Button>("play_button")
        .expect("play_button in context_menu.ui");
    let watch_later_button = builder
        .object::<Button>("watch_later_button")
        .expect("watch_later_button in context_menu.ui");
    let copy_url_button = builder
        .object::<Button>("copy_url_button")
        .expect("copy_url_button in context_menu.ui");
    let summary_button = builder
        .object::<Button>("summary_button")
        .expect("summary_button in context_menu.ui");
    let transcript_button = builder
        .object::<Button>("transcript_button")
        .expect("transcript_button in context_menu.ui");
    let unsub_button = builder
        .object::<Button>("unsub_button")
        .expect("unsub_button in context_menu.ui");

    popover.add(&menu_box);
    menu_box.show_all();

    let ui_context = ui_context.clone();

    // Connect handlers once - they read from selected_video
    on_menu_action(
        &play_button,
        selected_video.clone(),
        ui_context.context_menu.clone(),
        {
            let state_rc = state_rc.clone();
            let runtime = ui_context.runtime.clone();
            move |video_id| {
                play_selected_video(&state_rc, &runtime, &video_id);
            }
        },
    );
    on_menu_action(
        &watch_later_button,
        selected_video.clone(),
        ui_context.context_menu.clone(),
        {
            let state_rc = state_rc.clone();
            let ui_context = ui_context.clone();
            move |video_id| {
                apply_watch_later_action(&state_rc, &ui_context, video_id);
            }
        },
    );
    on_menu_action(
        &copy_url_button,
        selected_video.clone(),
        ui_context.context_menu.clone(),
        |video_id| {
            // GTK3's clipboard abstraction handles both X11 and Wayland via GDK
            gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD)
                .set_text(&crate::urls::watch_url(&video_id));
        },
    );
    on_menu_action(
        &summary_button,
        selected_video.clone(),
        ui_context.context_menu.clone(),
        {
            let state_rc = state_rc.clone();
            let ui_context = ui_context.clone();
            move |video_id| {
                show_summary_dialog(&state_rc, &ui_context, &video_id);
            }
        },
    );
    on_menu_action(
        &transcript_button,
        selected_video.clone(),
        ui_context.context_menu.clone(),
        {
            let state_rc = state_rc.clone();
            let ui_context = ui_context.clone();
            move |video_id| {
                show_transcript_dialog(&state_rc, &ui_context, &video_id);
            }
        },
    );

    on_menu_action(
        &unsub_button,
        selected_video,
        ui_context.context_menu.clone(),
        {
            let state_rc = state_rc.clone();
            let ui_context = ui_context.clone();
            move |video_id| {
                super::unsubscribe_channel(&state_rc, &ui_context, video_id);
            }
        },
    );
}

fn flow_for_tab(ui_context: &AppContext, tab: Tab) -> &FlowBox {
    match tab {
        Tab::Feed => &ui_context.feed_flow,
        Tab::WatchLater => &ui_context.watch_later_flow,
    }
}

fn card_map_for_tab(
    ui_context: &AppContext,
    tab: Tab,
) -> &Rc<RefCell<HashMap<String, VideoCardWidgets>>> {
    match tab {
        Tab::Feed => &ui_context.feed_cards,
        Tab::WatchLater => &ui_context.watch_later_cards,
    }
}

fn video_ids_for_tab(state: &AppState, tab: Tab) -> Vec<String> {
    match tab {
        Tab::Feed => state.videos.keys().cloned().collect(),
        Tab::WatchLater => state
            .videos
            .keys()
            .filter(|video_id| state.watch_later.contains(video_id.as_str()))
            .cloned()
            .collect(),
    }
}

fn connect_card_handlers(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
    card_widgets: &VideoCardWidgets,
) {
    let video_id = video_id.to_string();
    card_widgets.summary_button().connect_clicked(
        clone!(@strong state_rc, @strong ui_context, @strong video_id => move |_| {
            show_summary_dialog(&state_rc, &ui_context, &video_id);
        }),
    );

    card_widgets.root().connect_button_press_event(
        clone!(@strong video_id, @strong state_rc, @strong ui_context => move |widget, event| {
            if event.button() == 1 && event.event_type() == gdk::EventType::DoubleButtonPress {
                play_selected_video(&state_rc, &ui_context.runtime, &video_id);
                return glib::Propagation::Stop;
            }

            if event.button() == 3 {
                *ui_context.selected_video.borrow_mut() = Some(video_id.clone());
                ui_context.context_menu.set_relative_to(Some(widget));
                ui_context.context_menu.popup();
                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        }),
    );

    card_widgets.watch_later_toggle().connect_clicked(
        clone!(@strong state_rc, @strong ui_context, @strong video_id => move |_| {
            apply_watch_later_action(&state_rc, &ui_context, video_id.clone());
        }),
    );
}

fn add_card_to_flow(flow_box: &FlowBox, card_widgets: &VideoCardWidgets, position: Option<usize>) {
    match position {
        Some(position) => flow_box.insert(card_widgets.root(), position as i32),
        None => flow_box.add(card_widgets.root()),
    }

    if let Some(parent) = card_widgets.root().parent() {
        if let Ok(flow_child) = parent.downcast::<gtk::FlowBoxChild>() {
            flow_child.set_hexpand(false);
            flow_child.set_halign(gtk::Align::Start);
        }
    }
}

fn build_video_card(
    video_id: &str,
    downloaded_video_ids: &HashSet<String>,
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
) -> Option<VideoCardWidgets> {
    let card_widgets = {
        let state = state_rc.borrow();
        let video = state.video_by_id(video_id)?;
        let thumbnail_path = state.storage.thumbnail_path(video.video_id());
        let is_watch_later = state.watch_later.contains(video.video_id());
        let is_downloaded = downloaded_video_ids.contains(video.video_id());
        create_video_card(
            video,
            &thumbnail_path,
            is_watch_later,
            is_downloaded,
            video.has_ai_summary(),
        )
    };
    connect_card_handlers(state_rc, ui_context, video_id, &card_widgets);
    Some(card_widgets)
}

pub(super) fn populate_flow_box(
    tab: Tab,
    downloaded_video_ids: &HashSet<String>,
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
) {
    let flow_box = flow_for_tab(ui_context, tab);
    let card_map = card_map_for_tab(ui_context, tab);

    // Clear existing children
    flow_box.foreach(|child| {
        flow_box.remove(child);
    });
    card_map.borrow_mut().clear();

    let video_ids = {
        let state = state_rc.borrow();
        video_ids_for_tab(&state, tab)
    };

    for video_id in video_ids {
        let Some(card_widgets) =
            build_video_card(&video_id, downloaded_video_ids, state_rc, ui_context)
        else {
            continue;
        };
        add_card_to_flow(flow_box, &card_widgets, None);
        card_map.borrow_mut().insert(video_id, card_widgets);
    }

    flow_box.show_all();
}

pub(super) fn for_each_card_matching<F>(ui_context: &AppContext, video_id: &str, mut action: F)
where
    F: FnMut(&VideoCardWidgets),
{
    for card_map in [&ui_context.feed_cards, &ui_context.watch_later_cards] {
        if let Some(card) = card_map.borrow().get(video_id).cloned() {
            action(&card);
        }
    }
}

pub(super) fn update_watch_later_toggles(
    ui_context: &AppContext,
    video_id: &str,
    is_watch_later: bool,
) {
    for_each_card_matching(ui_context, video_id, |card| {
        set_watch_later_toggle_state(card.watch_later_toggle(), is_watch_later);
    });
}

fn watch_later_insert_position(state: &AppState, video_id: &str) -> Option<usize> {
    state
        .videos
        .values()
        .filter(|video| state.watch_later.contains(video.video_id()))
        .position(|video| video.video_id() == video_id)
}

pub(super) fn sync_watch_later_card(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let (in_watch_later, insert_position, is_downloaded) = {
        let state = state_rc.borrow();
        let in_watch_later = state.watch_later.contains(video_id);
        let insert_position = watch_later_insert_position(&state, video_id);
        let is_downloaded = state.storage.find_video_path(video_id).is_some();
        (in_watch_later, insert_position, is_downloaded)
    };

    let mut watch_later_cards = ui_context.watch_later_cards.borrow_mut();
    if !in_watch_later {
        if let Some(card) = watch_later_cards.remove(video_id) {
            ui_context.watch_later_flow.remove(card.root());
        }
        return;
    }

    if watch_later_cards.contains_key(video_id) {
        return;
    }

    let downloaded_video_ids = if is_downloaded {
        HashSet::from([video_id.to_string()])
    } else {
        HashSet::new()
    };
    let Some(card_widgets) =
        build_video_card(video_id, &downloaded_video_ids, state_rc, ui_context)
    else {
        return;
    };
    add_card_to_flow(&ui_context.watch_later_flow, &card_widgets, insert_position);
    watch_later_cards.insert(video_id.to_string(), card_widgets);
    ui_context.watch_later_flow.show_all();
}

pub(super) fn refresh_video_summary_badges(
    ui_context: &AppContext,
    video_id: &str,
    has_summary: bool,
) {
    for_each_card_matching(ui_context, video_id, |card| {
        card.set_summary_available(has_summary);
    });
}

pub(super) fn refresh_video_thumbnail(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let thumbnail_path = {
        let state = state_rc.borrow();
        state.storage.thumbnail_path(video_id)
    };

    for_each_card_matching(ui_context, video_id, |card| {
        card.refresh_thumbnail(&thumbnail_path);
    });
}

/// Downloads thumbnails missing from local storage.
///
/// # Arguments
///
/// * `videos` - Videos whose thumbnails should be present locally.
/// * `storage` - Storage backend used to resolve thumbnail paths.
/// * `client` - HTTP client for fetching thumbnail images.
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

// ---------------------------------------------------------------------------
// Video card widget (formerly src/ui/video_card.rs)
// ---------------------------------------------------------------------------

const CARD_WIDTH: i32 = 320;
// 16:9 thumbnail height derived from card width
const THUMBNAIL_HEIGHT: i32 = CARD_WIDTH * 9 / 16;

/// Widget handles for an individual video card.
#[derive(Clone)]
pub(super) struct VideoCardWidgets {
    root: EventBox,
    watch_later_toggle: gtk::Button,
    summary_button: gtk::Button,
    thumbnail: DrawingArea,
    thumbnail_pixbuf: Rc<RefCell<Option<Pixbuf>>>,
}

impl VideoCardWidgets {
    /// Returns the card root widget.
    pub fn root(&self) -> &EventBox {
        &self.root
    }

    /// Returns the Watch Later toggle button.
    pub fn watch_later_toggle(&self) -> &gtk::Button {
        &self.watch_later_toggle
    }

    /// Returns the AI summary button.
    pub fn summary_button(&self) -> &gtk::Button {
        &self.summary_button
    }

    /// Shows or hides the AI summary button.
    pub fn set_summary_available(&self, has_summary: bool) {
        self.summary_button.set_visible(has_summary);
    }

    /// Reloads the thumbnail image from disk and redraws the card.
    pub fn refresh_thumbnail(&self, thumbnail_path: &Path) {
        *self.thumbnail_pixbuf.borrow_mut() = load_thumbnail(thumbnail_path);
        self.thumbnail.queue_draw();
    }
}

fn load_thumbnail(thumbnail_path: &Path) -> Option<Pixbuf> {
    if !thumbnail_path.exists() {
        return None;
    }

    Pixbuf::from_file(thumbnail_path)
        .ok()
        .map(|pixbuf| crop_to_16_9(&pixbuf))
}

/// Create a video card widget
///
/// # Arguments
///
/// * `video` - Video metadata to display.
/// * `thumbnail_path` - Local thumbnail file path.
/// * `is_watch_later` - Whether the item is already in Watch Later.
/// * `is_downloaded` - Whether the video exists in local cache.
/// * `has_ai_summary` - Whether a cached AI summary exists.
///
/// # Returns
///
/// Widget handles for the created card.
fn create_video_card(
    video: &Video,
    thumbnail_path: &Path,
    is_watch_later: bool,
    is_downloaded: bool,
    has_ai_summary: bool,
) -> VideoCardWidgets {
    let event_box = EventBox::new();
    event_box.set_above_child(false);
    event_box.set_hexpand(false);
    event_box.set_halign(Align::Start);

    let card = gtk::Box::new(Orientation::Vertical, 0);
    card.set_widget_name("video-card");
    card.set_size_request(CARD_WIDTH, -1);
    card.set_hexpand(false);
    card.set_halign(Align::Start);

    // Thumbnail - 16:9 aspect ratio, fills card width using DrawingArea
    let thumbnail = DrawingArea::new();
    thumbnail.set_size_request(-1, THUMBNAIL_HEIGHT); // Let width be determined by card
    thumbnail.set_widget_name("thumbnail");
    thumbnail.set_hexpand(true); // Expand to fill card width
    thumbnail.set_vexpand(false);

    // Load the cropped 16:9 pixbuf
    let pixbuf: Rc<RefCell<Option<Pixbuf>>> = Rc::new(RefCell::new(load_thumbnail(thumbnail_path)));

    let pixbuf_for_draw = pixbuf.clone();
    thumbnail.connect_draw(move |widget, cr| {
        let width = widget.allocated_width() as f64;
        let height = widget.allocated_height() as f64;
        let radius = 8.0; // Corner radius matching the card

        // Create clipping path with rounded top corners
        cr.new_path();
        cr.arc(
            radius,
            radius,
            radius,
            std::f64::consts::PI,
            1.5 * std::f64::consts::PI,
        );
        cr.arc(
            width - radius,
            radius,
            radius,
            1.5 * std::f64::consts::PI,
            2.0 * std::f64::consts::PI,
        );
        cr.line_to(width, height);
        cr.line_to(0.0, height);
        cr.close_path();
        cr.clip();

        if let Some(ref pb) = *pixbuf_for_draw.borrow() {
            // Scale uniformly to cover the area (both image and area are 16:9)
            let scale = (width / pb.width() as f64).max(height / pb.height() as f64);
            let scaled_w = pb.width() as f64 * scale;
            let scaled_h = pb.height() as f64 * scale;

            // Center the scaled image
            let x_offset = (width - scaled_w) / 2.0;
            let y_offset = (height - scaled_h) / 2.0;

            cr.translate(x_offset, y_offset);
            cr.scale(scale, scale);
            cr.set_source_pixbuf(pb, 0.0, 0.0);
            cr.paint().ok();
        } else {
            // Draw placeholder background
            cr.set_source_rgb(0.2, 0.2, 0.2);
            cr.rectangle(0.0, 0.0, width, height);
            cr.fill().ok();
        }

        glib::Propagation::Stop
    });

    card.pack_start(&thumbnail, false, false, 0);

    // Content box for text below thumbnail (with padding)
    let content_box = gtk::Box::new(Orientation::Vertical, 4);
    content_box.set_margin_start(8);
    content_box.set_margin_end(8);
    content_box.set_margin_top(8);
    content_box.set_margin_bottom(8);
    content_box.set_hexpand(false);
    content_box.set_size_request(CARD_WIDTH - 16, -1); // card width minus 8px start/end margins

    // Title: wrap naturally and clamp to two lines using Pango layout.
    let title_label = Label::new(Some(video.title()));
    title_label.set_widget_name("video-title");
    title_label.set_line_wrap(true);
    title_label.set_line_wrap_mode(pango::WrapMode::WordChar);
    title_label.set_lines(2);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    title_label.set_xalign(0.0);
    title_label.set_yalign(0.0);
    // Keep FlowBox card widths stable by constraining the label's natural width.
    // This is a layout hint only; wrapping/ellipsizing still comes from Pango.
    title_label.set_width_chars(36);
    title_label.set_max_width_chars(36);
    content_box.pack_start(&title_label, false, false, 0);

    // Channel name and time
    let meta_box = gtk::Box::new(Orientation::Horizontal, 8);
    meta_box.set_widget_name("video-meta");

    let channel_label = Label::new(Some(video.channel_name()));
    channel_label.set_widget_name("channel-name");
    channel_label.set_ellipsize(pango::EllipsizeMode::End);
    channel_label.set_xalign(0.0);
    channel_label.set_hexpand(true);
    meta_box.pack_start(&channel_label, true, true, 0);

    let time_ago = format_time_ago(video.published());
    let time_and_duration = match video.duration_seconds() {
        Some(seconds) => format!("{} • {}", time_ago, format_video_duration(seconds)),
        None => time_ago,
    };
    let time_label = Label::new(Some(&time_and_duration));
    time_label.set_widget_name("time-ago");
    meta_box.pack_end(&time_label, false, false, 0);

    content_box.pack_start(&meta_box, false, false, 0);

    // Status row with toggle and indicators
    let status_box = gtk::Box::new(Orientation::Horizontal, 4);
    status_box.set_halign(Align::Fill);

    // Watch later toggle button
    let watch_later_toggle = gtk::Button::new();
    set_watch_later_toggle_state(&watch_later_toggle, is_watch_later);
    status_box.pack_start(&watch_later_toggle, false, false, 0);

    // Spacer
    let spacer = gtk::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    status_box.pack_start(&spacer, true, true, 0);

    if is_downloaded {
        let downloaded_badge = gtk::Box::new(Orientation::Horizontal, 0);
        downloaded_badge.set_widget_name("status-downloaded");
        downloaded_badge.set_tooltip_text(Some("Downloaded"));

        let downloaded_icon =
            gtk::Image::from_icon_name(Some("media-floppy-symbolic"), gtk::IconSize::Menu);
        downloaded_badge.pack_start(&downloaded_icon, false, false, 0);

        status_box.pack_end(&downloaded_badge, false, false, 0);
    }

    let summary_button = gtk::Button::with_label("AI");
    summary_button.set_widget_name("status-ai-summary-button");
    summary_button.set_relief(gtk::ReliefStyle::None);
    summary_button.set_can_focus(false);
    summary_button.set_tooltip_text(Some("Show cached AI summary"));
    status_box.pack_end(&summary_button, false, false, 0);
    summary_button.set_visible(has_ai_summary);

    content_box.pack_start(&status_box, false, false, 0);

    card.pack_start(&content_box, false, false, 0);

    event_box.add(&card);
    VideoCardWidgets {
        root: event_box,
        watch_later_toggle,
        summary_button,
        thumbnail,
        thumbnail_pixbuf: pixbuf,
    }
}

/// Update the Watch Later toggle visuals for a card button.
///
/// # Arguments
///
/// * `button` - Toggle button rendered inside a video card.
/// * `is_watch_later` - `true` when the associated video is in Watch Later.
fn set_watch_later_toggle_state(button: &gtk::Button, is_watch_later: bool) {
    button.set_widget_name(if is_watch_later {
        "watch-later-toggle-active"
    } else {
        "watch-later-toggle"
    });
    button.set_label(if is_watch_later { "x" } else { "+" });
    button.set_tooltip_text(Some(if is_watch_later {
        "Remove from Watch Later"
    } else {
        "Add to Watch Later"
    }));
}

/// Crop a pixbuf to 16:9 aspect ratio (center crop)
fn crop_to_16_9(pixbuf: &Pixbuf) -> Pixbuf {
    let width = pixbuf.width();
    let height = pixbuf.height();

    // Target 16:9 aspect ratio
    let target_ratio = 16.0 / 9.0;
    let current_ratio = width as f64 / height as f64;

    let (crop_x, crop_y, crop_width, crop_height) = if current_ratio > target_ratio {
        // Image is wider than 16:9, crop sides
        let new_width = (height as f64 * target_ratio) as i32;
        let x_offset = (width - new_width) / 2;
        (x_offset, 0, new_width, height)
    } else {
        // Image is taller than 16:9, crop top/bottom
        let new_height = (width as f64 / target_ratio) as i32;
        let y_offset = (height - new_height) / 2;
        (0, y_offset, width, new_height)
    };

    pixbuf.new_subpixbuf(crop_x, crop_y, crop_width, crop_height)
}

fn format_time_ago(dt: chrono::DateTime<Utc>) -> String {
    HumanTime::from(dt).to_text_en(Accuracy::Rough, Tense::Past)
}

fn format_video_duration(total_seconds: u32) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{}:{:02}", minutes, seconds)
    }
}
