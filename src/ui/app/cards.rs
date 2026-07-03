use super::comments::show_comments_dialog;
use super::summary_generator::{show_summary_dialog, show_transcript_dialog};
use super::{AppContext, AppState, apply_watch_later_action, resolve_playback_path};
use crate::cache::Storage;
use crate::data::{Tab, Video};
use crate::player::{PlaybackEnd, play_video};

use chrono::Utc;
use chrono_humanize::{Accuracy, HumanTime, Tense};
use futures::stream::{self, StreamExt};
use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, FlowBox, Label, Orientation, Picture, Popover, Spinner};
use gtk::{gdk, graphene, pango};
use log::{error, warn};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

fn play_selected_video(state_rc: &Rc<RefCell<AppState>>, ui_context: &AppContext, video_id: &str) {
    if !ui_context
        .videos_playing
        .borrow_mut()
        .insert(video_id.to_string())
    {
        return;
    }

    let video_title = {
        let state = state_rc.borrow();
        state
            .video_by_id(video_id)
            .map(|current_video| current_video.title().to_string())
    };
    let Some(video_title) = video_title else {
        error!("Cannot play missing video {video_id}");
        ui_context.videos_playing.borrow_mut().remove(video_id);
        return;
    };
    let local_path = resolve_playback_path(state_rc, ui_context, video_id, &video_title);

    let playback_end_rx = match play_video(video_id, &video_title, local_path.as_deref()) {
        Ok(playback_end_rx) => playback_end_rx,
        Err(play_error) => {
            error!("Failed to play video {video_id}: {play_error}");
            ui_context.videos_playing.borrow_mut().remove(video_id);
            return;
        }
    };

    // Mark watched optimistically for a responsive badge; revert if mpv fails
    // to actually start playback.
    if let Err(e) = state_rc.borrow_mut().set_video_watched(video_id, true) {
        error!("Failed to mark video {video_id} as watched: {e}");
    } else {
        refresh_video_watched_badge(ui_context, video_id, true);
    }

    let state_rc = state_rc.clone();
    let ui_context = ui_context.clone();
    let video_id = video_id.to_string();
    glib::MainContext::default().spawn_local(async move {
        let playback_end = playback_end_rx.recv().await;
        ui_context.videos_playing.borrow_mut().remove(&video_id);
        if playback_end == Ok(PlaybackEnd::FailedImmediately) {
            error!("mpv failed to play {video_id}; reverting watched state");
            if let Err(e) = state_rc.borrow_mut().set_video_watched(&video_id, false) {
                error!("Failed to unmark video {video_id} as watched: {e}");
            } else {
                refresh_video_watched_badge(&ui_context, &video_id, false);
            }
        }
    });
}

/// Handler invoked with the selected video when a context-menu entry is clicked.
type MenuAction = fn(&Rc<RefCell<AppState>>, &AppContext, &str);

fn copy_watch_url(_state_rc: &Rc<RefCell<AppState>>, ui_context: &AppContext, video_id: &str) {
    // GDK's clipboard abstraction handles both X11 and Wayland
    ui_context
        .window
        .clipboard()
        .set_text(&crate::urls::watch_url(video_id));
}

pub(super) fn create_context_menu(
    popover: &Popover,
    state_rc: Rc<RefCell<AppState>>,
    ui_context: &AppContext,
) {
    const MENU_ACTIONS: [(&str, MenuAction); 7] = [
        ("play_button", play_selected_video),
        ("watch_later_button", apply_watch_later_action),
        ("copy_url_button", copy_watch_url),
        ("summary_button", show_summary_dialog),
        ("transcript_button", show_transcript_dialog),
        ("comments_button", show_comments_dialog),
        ("unsub_button", super::unsubscribe_channel),
    ];

    let builder = gtk::Builder::from_string(include_str!("context_menu.ui"));
    let menu_box = builder
        .object::<GtkBox>("menu_box")
        .expect("menu_box in context_menu.ui");
    popover.set_child(Some(&menu_box));
    popover.set_has_arrow(false);
    // Anchored to the stack so repopulating card grids never destroys the open popover.
    popover.set_parent(&ui_context.stack);

    for (button_id, action) in MENU_ACTIONS {
        let button = builder
            .object::<Button>(button_id)
            .expect("menu button in context_menu.ui");
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        button.connect_clicked(move |_| {
            ui_context.context_menu.popdown();
            let selected_video_id = ui_context.selected_video.borrow().clone();
            if let Some(video_id) = selected_video_id {
                action(&state_rc, &ui_context, &video_id);
            }
        });
    }
}

/// Cards currently shown on one tab, in display order.
///
/// `order` mirrors the `FlowBox` child order so repopulation can be skipped
/// when the video list is unchanged.
#[derive(Default)]
pub(super) struct TabCards {
    order: Vec<String>,
    cards: HashMap<String, VideoCardWidgets>,
}

pub(super) fn flow_for_tab(ui_context: &AppContext, tab: Tab) -> &FlowBox {
    match tab {
        Tab::Feed => &ui_context.feed_flow,
        Tab::Search => &ui_context.search_flow,
        Tab::WatchLater => &ui_context.watch_later_flow,
    }
}

const fn card_map_for_tab(ui_context: &AppContext, tab: Tab) -> &Rc<RefCell<TabCards>> {
    match tab {
        Tab::Feed => &ui_context.feed_cards,
        Tab::Search => &ui_context.search_cards,
        Tab::WatchLater => &ui_context.watch_later_cards,
    }
}

fn video_ids_for_tab(state: &AppState, tab: Tab) -> Vec<String> {
    match tab {
        Tab::Feed => state.feed_video_ids().to_vec(),
        Tab::Search => state.search_video_ids().to_vec(),
        Tab::WatchLater => state
            .videos
            .keys()
            .filter(|video_id| state.watch_later.contains(video_id.as_str()))
            .cloned()
            .collect(),
    }
}

/// Opens the shared context menu pointing at click coordinates local to `widget`.
fn open_context_menu(ui_context: &AppContext, widget: &gtk::Widget, x: f64, y: f64) {
    #[allow(clippy::cast_possible_truncation)]
    let click_point = widget
        .compute_point(&ui_context.stack, &graphene::Point::new(x as f32, y as f32))
        .unwrap_or_else(|| graphene::Point::new(x as f32, y as f32));
    #[allow(clippy::cast_possible_truncation)]
    let anchor = gdk::Rectangle::new(click_point.x() as i32, click_point.y() as i32, 1, 1);
    ui_context.context_menu.set_pointing_to(Some(&anchor));
    ui_context.context_menu.popup();
}

fn connect_card_handlers(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
    card_widgets: &VideoCardWidgets,
) {
    let video_id = video_id.to_string();

    {
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        let video_id = video_id.clone();
        card_widgets.summary_button().connect_clicked(move |_| {
            show_summary_dialog(&state_rc, &ui_context, &video_id);
        });
    }

    let double_click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_PRIMARY)
        .build();
    {
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        let video_id = video_id.clone();
        double_click.connect_pressed(move |_, n_press, _, _| {
            if n_press == 2 {
                play_selected_video(&state_rc, &ui_context, &video_id);
            }
        });
    }
    card_widgets.root().add_controller(double_click);

    let right_click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    {
        let ui_context = ui_context.clone();
        let video_id = video_id.clone();
        right_click.connect_pressed(move |gesture, _, x, y| {
            *ui_context.selected_video.borrow_mut() = Some(video_id.clone());
            open_context_menu(
                &ui_context,
                &gesture.widget().expect("gesture widget"),
                x,
                y,
            );
        });
    }
    card_widgets.root().add_controller(right_click);

    {
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        let video_id = video_id.clone();
        card_widgets.watch_later_toggle().connect_clicked(move |_| {
            apply_watch_later_action(&state_rc, &ui_context, &video_id);
        });
    }

    {
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        card_widgets.watched_button().connect_clicked(move |_| {
            if let Err(e) = state_rc.borrow_mut().set_video_watched(&video_id, false) {
                error!("Failed to unmark video {video_id} as watched: {e}");
            } else {
                refresh_video_watched_badge(&ui_context, &video_id, false);
            }
        });
    }
}

/// Derives all card badge flags from application state, so cards can be built
/// or refreshed at any time without losing transient download status.
fn card_flags_for_video(state: &AppState, ui_context: &AppContext, video: &Video) -> CardFlags {
    CardFlags {
        is_watch_later: state.watch_later.contains(video.video_id()),
        is_downloaded: state.downloaded_video_ids.contains(video.video_id()),
        is_downloading: ui_context
            .downloads_in_progress
            .borrow()
            .contains(video.video_id()),
        has_ai_summary: video.has_ai_summary(),
        is_watched: video.is_watched(),
    }
}

fn build_video_card(
    video_id: &str,
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
) -> Option<VideoCardWidgets> {
    let card_widgets = {
        let state = state_rc.borrow();
        let video = state.video_by_id(video_id)?;
        let thumbnail_path = state.storage.thumbnail_path(video.video_id());
        let flags = card_flags_for_video(&state, ui_context, video);
        create_video_card(video, &thumbnail_path, &flags)
    };
    connect_card_handlers(state_rc, ui_context, video_id, &card_widgets);
    Some(card_widgets)
}

fn add_card_to_flow(flow_box: &FlowBox, card_widgets: &VideoCardWidgets, position: Option<usize>) {
    match position {
        Some(position) => flow_box.insert(
            card_widgets.root(),
            i32::try_from(position).unwrap_or(i32::MAX),
        ),
        None => flow_box.append(card_widgets.root()),
    }
}

/// Reapplies state-derived badge visibility to every card on a tab.
fn refresh_cards_from_state(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    card_map: &Rc<RefCell<TabCards>>,
) {
    let state = state_rc.borrow();
    for (video_id, card) in &card_map.borrow().cards {
        let Some(video) = state.video_by_id(video_id) else {
            continue;
        };
        card.apply_flags(&card_flags_for_video(&state, ui_context, video));
    }
}

pub(super) fn populate_flow_box(
    tab: Tab,
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
) {
    let flow_box = flow_for_tab(ui_context, tab);
    let card_map = card_map_for_tab(ui_context, tab);

    let video_ids = {
        let state = state_rc.borrow();
        video_ids_for_tab(&state, tab)
    };

    // Same videos in the same order: refresh badges in place instead of
    // rebuilding every card, preserving scroll position and download spinners.
    if card_map.borrow().order == video_ids {
        refresh_cards_from_state(state_rc, ui_context, card_map);
        return;
    }

    flow_box.remove_all();
    {
        let mut tab_cards = card_map.borrow_mut();
        tab_cards.cards.clear();
        tab_cards.order.clear();
    }

    for video_id in video_ids {
        let Some(card_widgets) = build_video_card(&video_id, state_rc, ui_context) else {
            continue;
        };
        add_card_to_flow(flow_box, &card_widgets, None);
        let mut tab_cards = card_map.borrow_mut();
        tab_cards.order.push(video_id.clone());
        tab_cards.cards.insert(video_id, card_widgets);
    }
}

pub(super) fn for_each_card_matching<F>(ui_context: &AppContext, video_id: &str, mut action: F)
where
    F: FnMut(&VideoCardWidgets),
{
    for card_map in [
        &ui_context.feed_cards,
        &ui_context.search_cards,
        &ui_context.watch_later_cards,
    ] {
        if let Some(card) = card_map.borrow().cards.get(video_id).cloned() {
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
    let (in_watch_later, insert_position) = {
        let state = state_rc.borrow();
        (
            state.watch_later.contains(video_id),
            watch_later_insert_position(&state, video_id),
        )
    };

    let mut watch_later_cards = ui_context.watch_later_cards.borrow_mut();
    if !in_watch_later {
        if let Some(card) = watch_later_cards.cards.remove(video_id)
            && let Some(flow_child) = card.root().parent()
        {
            ui_context.watch_later_flow.remove(&flow_child);
        }
        watch_later_cards.order.retain(|id| id != video_id);
        return;
    }

    if watch_later_cards.cards.contains_key(video_id) {
        return;
    }

    let Some(card_widgets) = build_video_card(video_id, state_rc, ui_context) else {
        return;
    };
    add_card_to_flow(&ui_context.watch_later_flow, &card_widgets, insert_position);
    let order_position = insert_position
        .unwrap_or(watch_later_cards.order.len())
        .min(watch_later_cards.order.len());
    watch_later_cards
        .order
        .insert(order_position, video_id.to_string());
    watch_later_cards
        .cards
        .insert(video_id.to_string(), card_widgets);
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

pub(super) fn refresh_video_watched_badge(
    ui_context: &AppContext,
    video_id: &str,
    is_watched: bool,
) {
    for_each_card_matching(ui_context, video_id, |card| {
        card.set_watched(is_watched);
    });
}

pub(super) fn refresh_video_downloading_badge(ui_context: &AppContext, video_id: &str) {
    for_each_card_matching(ui_context, video_id, |card| {
        card.set_downloading();
    });
}

pub(super) fn refresh_video_downloaded_badge(ui_context: &AppContext, video_id: &str) {
    for_each_card_matching(ui_context, video_id, |card| {
        card.set_downloaded();
    });
}

pub(super) fn refresh_video_download_failed_badge(ui_context: &AppContext, video_id: &str) {
    for_each_card_matching(ui_context, video_id, |card| {
        card.clear_download_badges();
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
    runtime: &Arc<tokio::runtime::Runtime>,
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
                                warn!("Thumbnail request failed for {url}: {error}");
                                return;
                            }
                        };

                        let response = match response.error_for_status() {
                            Ok(response) => response,
                            Err(error) => {
                                warn!("Thumbnail response failed for {url}: {error}");
                                return;
                            }
                        };

                        match response.bytes().await {
                            Ok(bytes) => {
                                if let Some(parent) = path.parent()
                                    && let Err(error) = tokio::fs::create_dir_all(parent).await
                                {
                                    warn!(
                                        "Failed creating thumbnail directory {}: {}",
                                        parent.display(),
                                        error
                                    );
                                    return;
                                }

                                if let Err(error) = tokio::fs::write(&path, &bytes).await {
                                    warn!(
                                        "Failed writing thumbnail to {}: {}",
                                        path.display(),
                                        error
                                    );
                                }
                            }
                            Err(error) => {
                                warn!("Failed reading thumbnail bytes for {url}: {error}");
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
// Video card widget
// ---------------------------------------------------------------------------

pub(super) const CARD_WIDTH: i32 = 320;
// 16:9 thumbnail height derived from card width
const THUMBNAIL_HEIGHT: i32 = CARD_WIDTH * 9 / 16;

/// Widget handles for an individual video card.
#[derive(Clone)]
pub(super) struct VideoCardWidgets {
    root: GtkBox,
    watch_later_toggle: Button,
    summary_button: Button,
    watched_button: Button,
    downloaded_badge: GtkBox,
    download_spinner: Spinner,
    thumbnail: Picture,
}

impl VideoCardWidgets {
    /// Returns the card root widget.
    pub const fn root(&self) -> &GtkBox {
        &self.root
    }

    /// Returns the Watch Later toggle button.
    pub const fn watch_later_toggle(&self) -> &Button {
        &self.watch_later_toggle
    }

    /// Returns the AI summary button.
    pub const fn summary_button(&self) -> &Button {
        &self.summary_button
    }

    /// Shows or hides the AI summary button.
    pub fn set_summary_available(&self, has_summary: bool) {
        self.summary_button.set_visible(has_summary);
    }

    /// Returns the watched button.
    pub const fn watched_button(&self) -> &Button {
        &self.watched_button
    }

    /// Shows or hides the watched checkmark badge.
    pub fn set_watched(&self, is_watched: bool) {
        self.watched_button.set_visible(is_watched);
    }

    /// Shows an active spinner, hiding the floppy badge.
    pub fn set_downloading(&self) {
        self.downloaded_badge.set_visible(false);
        self.download_spinner.start();
        self.download_spinner.set_visible(true);
    }

    /// Stops the spinner and shows the floppy badge.
    pub fn set_downloaded(&self) {
        self.download_spinner.stop();
        self.download_spinner.set_visible(false);
        self.downloaded_badge.set_visible(true);
    }

    /// Stops the spinner and hides the floppy badge: the "no local download"
    /// state, shown after a failed download or for never-downloaded videos.
    pub fn clear_download_badges(&self) {
        self.download_spinner.stop();
        self.download_spinner.set_visible(false);
        self.downloaded_badge.set_visible(false);
    }

    /// Reloads the thumbnail image from disk.
    pub fn refresh_thumbnail(&self, thumbnail_path: &Path) {
        set_thumbnail_file(&self.thumbnail, thumbnail_path);
    }

    /// Applies state-derived badge visibility without rebuilding the card.
    pub fn apply_flags(&self, flags: &CardFlags) {
        set_watch_later_toggle_state(&self.watch_later_toggle, flags.is_watch_later);
        self.set_summary_available(flags.has_ai_summary);
        self.set_watched(flags.is_watched);
        if flags.is_downloading {
            self.set_downloading();
        } else if flags.is_downloaded {
            self.set_downloaded();
        } else {
            self.clear_download_badges();
        }
    }
}

fn set_thumbnail_file(thumbnail: &Picture, thumbnail_path: &Path) {
    if thumbnail_path.exists() {
        thumbnail.set_filename(Some(thumbnail_path));
    }
}

#[allow(clippy::struct_excessive_bools)]
pub(super) struct CardFlags {
    is_watch_later: bool,
    is_downloaded: bool,
    is_downloading: bool,
    has_ai_summary: bool,
    is_watched: bool,
}

/// Widgets composing a card's bottom status row.
struct StatusRowWidgets {
    status_box: GtkBox,
    watch_later_toggle: Button,
    watched_button: Button,
    download_spinner: Spinner,
    downloaded_badge: GtkBox,
    summary_button: Button,
}

fn build_status_row(flags: &CardFlags) -> StatusRowWidgets {
    let status_box = GtkBox::new(Orientation::Horizontal, 4);

    let watch_later_toggle = Button::new();
    set_watch_later_toggle_state(&watch_later_toggle, flags.is_watch_later);
    status_box.append(&watch_later_toggle);

    let watched_button = Button::from_icon_name("object-select-symbolic");
    watched_button.set_widget_name("watched-button");
    watched_button.set_tooltip_text(Some("Watched — click to unmark"));
    watched_button.set_has_frame(false);
    watched_button.set_can_focus(false);
    watched_button.set_visible(flags.is_watched);
    status_box.append(&watched_button);

    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    status_box.append(&spacer);

    let summary_button = Button::with_label("AI");
    summary_button.set_widget_name("status-ai-summary-button");
    summary_button.set_has_frame(false);
    summary_button.set_can_focus(false);
    summary_button.set_tooltip_text(Some("Show cached AI summary"));
    summary_button.set_visible(flags.has_ai_summary);
    status_box.append(&summary_button);

    let downloaded_badge = GtkBox::new(Orientation::Horizontal, 0);
    downloaded_badge.set_widget_name("status-downloaded");
    downloaded_badge.set_tooltip_text(Some("Downloaded"));
    downloaded_badge.append(&gtk::Image::from_icon_name("media-floppy-symbolic"));
    downloaded_badge.set_visible(flags.is_downloaded);
    status_box.append(&downloaded_badge);

    let download_spinner = Spinner::new();
    download_spinner.set_tooltip_text(Some("Downloading…"));
    download_spinner.set_visible(false);
    status_box.append(&download_spinner);

    StatusRowWidgets {
        status_box,
        watch_later_toggle,
        watched_button,
        download_spinner,
        downloaded_badge,
        summary_button,
    }
}

fn create_video_card(video: &Video, thumbnail_path: &Path, flags: &CardFlags) -> VideoCardWidgets {
    let card = GtkBox::new(Orientation::Vertical, 0);
    card.set_widget_name("video-card");
    card.add_css_class("card");
    // Clip the thumbnail to the card's rounded corners.
    card.set_overflow(gtk::Overflow::Hidden);
    card.set_size_request(CARD_WIDTH, -1);
    card.set_halign(Align::Center);

    let thumbnail = Picture::new();
    thumbnail.set_widget_name("thumbnail");
    // Cover keeps 16:9 by center-cropping the source image.
    thumbnail.set_content_fit(gtk::ContentFit::Cover);
    thumbnail.set_size_request(-1, THUMBNAIL_HEIGHT);
    set_thumbnail_file(&thumbnail, thumbnail_path);
    card.append(&thumbnail);

    let content_box = GtkBox::new(Orientation::Vertical, 4);
    content_box.set_margin_start(8);
    content_box.set_margin_end(8);
    content_box.set_margin_top(8);
    content_box.set_margin_bottom(8);

    let title_label = Label::new(Some(video.title()));
    title_label.set_widget_name("video-title");
    title_label.set_wrap(true);
    title_label.set_wrap_mode(pango::WrapMode::WordChar);
    title_label.set_lines(2);
    title_label.set_ellipsize(pango::EllipsizeMode::End);
    title_label.set_xalign(0.0);
    title_label.set_yalign(0.0);
    title_label.set_width_chars(36);
    title_label.set_max_width_chars(36);
    content_box.append(&title_label);

    let meta_box = GtkBox::new(Orientation::Horizontal, 8);
    meta_box.set_widget_name("video-meta");

    let channel_label = Label::new(Some(video.channel_name()));
    channel_label.set_widget_name("channel-name");
    channel_label.set_ellipsize(pango::EllipsizeMode::End);
    channel_label.set_xalign(0.0);
    channel_label.set_hexpand(true);
    meta_box.append(&channel_label);

    let time_ago = format_time_ago(video.published());
    let time_and_duration = match video.duration_seconds() {
        Some(seconds) => format!("{} • {}", time_ago, format_video_duration(seconds)),
        None => time_ago,
    };
    let time_label = Label::new(Some(&time_and_duration));
    time_label.set_widget_name("time-ago");
    meta_box.append(&time_label);

    content_box.append(&meta_box);

    let status_row = build_status_row(flags);
    content_box.append(&status_row.status_box);

    card.append(&content_box);

    let card_widgets = VideoCardWidgets {
        root: card,
        watch_later_toggle: status_row.watch_later_toggle,
        summary_button: status_row.summary_button,
        watched_button: status_row.watched_button,
        downloaded_badge: status_row.downloaded_badge,
        download_spinner: status_row.download_spinner,
        thumbnail,
    };
    if flags.is_downloading {
        card_widgets.set_downloading();
    }
    card_widgets
}

/// Update the Watch Later toggle visuals for a card button.
///
/// # Arguments
///
/// * `button` - Toggle button rendered inside a video card.
/// * `is_watch_later` - `true` when the associated video is in Watch Later.
fn set_watch_later_toggle_state(button: &Button, is_watch_later: bool) {
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

fn format_time_ago(dt: chrono::DateTime<Utc>) -> String {
    HumanTime::from(dt).to_text_en(Accuracy::Rough, Tense::Past)
}

fn format_video_duration(total_seconds: u32) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::{format_video_duration, watch_later_insert_position};
    use crate::cache::Storage;
    use crate::data::{NewVideo, Video};
    use chrono::{TimeZone, Utc};
    use std::collections::HashSet;

    #[test]
    fn format_video_duration_omits_hours_when_under_an_hour() {
        assert_eq!(format_video_duration(0), "0:00");
        assert_eq!(format_video_duration(59), "0:59");
        assert_eq!(format_video_duration(61), "1:01");
        assert_eq!(format_video_duration(3599), "59:59");
    }

    #[test]
    fn format_video_duration_includes_zero_padded_hours() {
        assert_eq!(format_video_duration(3600), "1:00:00");
        assert_eq!(format_video_duration(3661), "1:01:01");
        assert_eq!(format_video_duration(7 * 3600 + 9 * 60 + 5), "7:09:05");
    }

    fn test_video(video_id: &str) -> Video {
        let published = Utc
            .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
            .single()
            .expect("valid fixed test timestamp");
        Video::new(NewVideo {
            video_id: video_id.to_string(),
            channel_id: "channel-id".to_string(),
            channel_name: "channel-name".to_string(),
            title: format!("title-{video_id}"),
            published,
            thumbnail_url: "https://example.com/thumb.jpg".to_string(),
            duration_seconds: None,
        })
    }

    fn test_state(
        video_ids: &[&str],
        watch_later: &[&str],
    ) -> (super::AppState, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("test directory must be creatable");
        let storage = Storage::new_at(root.path().join("data"), root.path().join("cache"))
            .expect("test storage must initialize");
        let videos: Vec<Video> = video_ids.iter().map(|id| test_video(id)).collect();
        let feed_video_ids = video_ids.iter().map(ToString::to_string).collect();
        let watch_later: HashSet<String> = watch_later.iter().map(ToString::to_string).collect();
        (
            super::AppState::new(videos, feed_video_ids, watch_later, storage),
            root,
        )
    }

    #[test]
    fn watch_later_insert_position_orders_by_video_map_order() {
        let (state, _root) = test_state(&["a", "b", "c", "d"], &["b", "d"]);

        assert_eq!(watch_later_insert_position(&state, "b"), Some(0));
        assert_eq!(watch_later_insert_position(&state, "d"), Some(1));
    }

    #[test]
    fn watch_later_insert_position_is_none_for_videos_not_in_watch_later() {
        let (state, _root) = test_state(&["a", "b"], &["b"]);

        assert_eq!(watch_later_insert_position(&state, "a"), None);
        assert_eq!(watch_later_insert_position(&state, "missing"), None);
    }
}
