use super::summary::{show_summary_dialog, show_transcript_dialog};
use super::{apply_watch_later_action, resolve_playback_path, AppState, UiContext};
use crate::data::{Tab, Video};
use crate::player::play_video;
use crate::ui::video_card::{create_video_card, set_watch_later_toggle_state, VideoCardWidgets};

use glib::clone;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, FlowBox, Orientation, Popover};
use log::error;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
        if let Some(video_id) = selected_video.borrow().clone() {
            action(video_id);
        }
        context_menu.popdown();
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
    ui_context: &UiContext,
) {
    let popover = popover.clone();
    let selected_video = ui_context.widgets.selected_video.clone();
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

    let copy_url_button = Button::with_label("Copy URL");
    copy_url_button.set_widget_name("menu-copy-url");
    menu_box.pack_start(&copy_url_button, false, false, 4);

    let summary_button = Button::with_label("AI Summary");
    summary_button.set_widget_name("menu-summary");
    menu_box.pack_start(&summary_button, false, false, 4);

    let transcript_button = Button::with_label("Transcript");
    transcript_button.set_widget_name("menu-transcript");
    menu_box.pack_start(&transcript_button, false, false, 4);

    popover.add(&menu_box);
    menu_box.show_all();

    let ui_context = ui_context.clone();

    // Connect handlers once - they read from selected_video
    on_menu_action(
        &play_button,
        selected_video.clone(),
        ui_context.widgets.context_menu.clone(),
        {
            let state_rc = state_rc.clone();
            let runtime = ui_context.async_ctx.runtime.clone();
            move |video_id| {
                play_selected_video(&state_rc, &runtime, &video_id);
            }
        },
    );
    on_menu_action(
        &watch_later_button,
        selected_video.clone(),
        ui_context.widgets.context_menu.clone(),
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
        ui_context.widgets.context_menu.clone(),
        |video_id| {
            // GTK3's clipboard abstraction handles both X11 and Wayland via GDK
            gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD)
                .set_text(&crate::urls::watch_url(&video_id));
        },
    );
    on_menu_action(
        &summary_button,
        selected_video.clone(),
        ui_context.widgets.context_menu.clone(),
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
        selected_video,
        ui_context.widgets.context_menu.clone(),
        {
            let state_rc = state_rc.clone();
            let ui_context = ui_context.clone();
            move |video_id| {
                show_transcript_dialog(&state_rc, &ui_context, &video_id);
            }
        },
    );
}

fn flow_for_tab(ui_context: &UiContext, tab: Tab) -> &FlowBox {
    match tab {
        Tab::Feed => &ui_context.widgets.feed_flow,
        Tab::WatchLater => &ui_context.widgets.watch_later_flow,
    }
}

fn card_map_for_tab(
    ui_context: &UiContext,
    tab: Tab,
) -> &Rc<RefCell<HashMap<String, VideoCardWidgets>>> {
    match tab {
        Tab::Feed => &ui_context.widgets.feed_cards,
        Tab::WatchLater => &ui_context.widgets.watch_later_cards,
    }
}

fn videos_for_tab(state: &AppState, tab: Tab) -> Vec<Video> {
    match tab {
        Tab::Feed => state.videos.values().cloned().collect(),
        Tab::WatchLater => state
            .videos
            .values()
            .filter(|video| state.watch_later.contains(video.video_id()))
            .cloned()
            .collect(),
    }
}

fn connect_card_handlers(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
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
                play_selected_video(&state_rc, &ui_context.async_ctx.runtime, &video_id);
                return glib::Propagation::Stop;
            }

            if event.button() == 3 {
                *ui_context.widgets.selected_video.borrow_mut() = Some(video_id.clone());
                ui_context.widgets.context_menu.set_relative_to(Some(widget));
                ui_context.widgets.context_menu.popup();
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
    video: &Video,
    downloaded_video_ids: &HashSet<String>,
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
) -> VideoCardWidgets {
    let state = state_rc.borrow();
    let thumbnail_path = state.storage.thumbnail_path(video.video_id());
    let is_watch_later = state.watch_later.contains(video.video_id());
    let is_downloaded = downloaded_video_ids.contains(video.video_id());
    drop(state);

    let card_widgets = create_video_card(
        video,
        &thumbnail_path,
        is_watch_later,
        is_downloaded,
        video.has_ai_summary(),
    );
    connect_card_handlers(state_rc, ui_context, video.video_id(), &card_widgets);
    card_widgets
}

pub(super) fn populate_flow_box(
    tab: Tab,
    downloaded_video_ids: &HashSet<String>,
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
) {
    let flow_box = flow_for_tab(ui_context, tab);
    let card_map = card_map_for_tab(ui_context, tab);

    // Clear existing children
    flow_box.foreach(|child| {
        flow_box.remove(child);
    });
    card_map.borrow_mut().clear();

    let videos = {
        let state = state_rc.borrow();
        videos_for_tab(&state, tab)
    };

    for video in videos {
        let card_widgets = build_video_card(&video, downloaded_video_ids, state_rc, ui_context);
        add_card_to_flow(flow_box, &card_widgets, None);
        card_map
            .borrow_mut()
            .insert(video.video_id().to_string(), card_widgets);
    }

    flow_box.show_all();
}

pub(super) fn update_watch_later_toggles(
    ui_context: &UiContext,
    video_id: &str,
    is_watch_later: bool,
) {
    for card_map in [
        &ui_context.widgets.feed_cards,
        &ui_context.widgets.watch_later_cards,
    ] {
        if let Some(card) = card_map.borrow().get(video_id).cloned() {
            set_watch_later_toggle_state(card.watch_later_toggle(), is_watch_later);
        }
    }
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
    ui_context: &UiContext,
    video_id: &str,
) {
    let (in_watch_later, video, insert_position, is_downloaded) = {
        let state = state_rc.borrow();
        let in_watch_later = state.watch_later.contains(video_id);
        let video = state.video_by_id(video_id).cloned();
        let insert_position = watch_later_insert_position(&state, video_id);
        let is_downloaded = state.storage.find_video_path(video_id).is_some();
        (in_watch_later, video, insert_position, is_downloaded)
    };

    let mut watch_later_cards = ui_context.widgets.watch_later_cards.borrow_mut();
    if !in_watch_later {
        if let Some(card) = watch_later_cards.remove(video_id) {
            ui_context.widgets.watch_later_flow.remove(card.root());
        }
        return;
    }

    if watch_later_cards.contains_key(video_id) {
        return;
    }

    let Some(video) = video else {
        return;
    };

    let downloaded_video_ids = if is_downloaded {
        HashSet::from([video_id.to_string()])
    } else {
        HashSet::new()
    };
    let card_widgets = build_video_card(&video, &downloaded_video_ids, state_rc, ui_context);
    add_card_to_flow(
        &ui_context.widgets.watch_later_flow,
        &card_widgets,
        insert_position,
    );
    watch_later_cards.insert(video_id.to_string(), card_widgets);
    ui_context.widgets.watch_later_flow.show_all();
}

pub(super) fn refresh_video_summary_badges(
    ui_context: &UiContext,
    video_id: &str,
    has_summary: bool,
) {
    for card_map in [
        &ui_context.widgets.feed_cards,
        &ui_context.widgets.watch_later_cards,
    ] {
        if let Some(card) = card_map.borrow().get(video_id).cloned() {
            card.set_summary_available(has_summary);
        }
    }
}

pub(super) fn refresh_video_thumbnail(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
    video_id: &str,
) {
    let thumbnail_path = {
        let state = state_rc.borrow();
        state.storage.thumbnail_path(video_id)
    };

    for card_map in [
        &ui_context.widgets.feed_cards,
        &ui_context.widgets.watch_later_cards,
    ] {
        if let Some(card) = card_map.borrow().get(video_id).cloned() {
            card.refresh_thumbnail(&thumbnail_path);
        }
    }
}
