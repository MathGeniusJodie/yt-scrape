use super::summary::{show_summary_dialog, show_transcript_dialog};
use super::{apply_watch_later_action, resolve_playback_path, AppState, SelectedVideo, UiContext};
use crate::data::{Tab, Video};
use crate::player::play_video;
use crate::ui::video_card::create_video_card;

use glib::clone;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, FlowBox, Orientation, Popover};
use log::error;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

pub(super) fn create_context_menu(
    popover: &Popover,
    state_rc: Rc<RefCell<AppState>>,
    ui_context: &UiContext,
) {
    let popover = popover.clone();
    let selected_video = ui_context.selected_video.clone();
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
    {
        let selected_video = selected_video.clone();
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        play_button.connect_clicked(move |_| {
            if let Some(video) = selected_video.borrow().clone() {
                let playback = {
                    let state = state_rc.borrow();
                    state.video_by_id(&video.video_id).map(|current_video| {
                        let video_title = current_video.title().to_string();
                        let local_path = state.storage.find_video_path(&video.video_id);
                        let local_path = resolve_playback_path(
                            &state.storage,
                            ui_context.runtime.clone(),
                            &video.video_id,
                            &video_title,
                            local_path,
                        );
                        (video_title, local_path)
                    })
                };

                if let Some((video_title, local_path)) = playback {
                    if let Err(play_error) =
                        play_video(&video.video_id, &video_title, local_path.as_deref())
                    {
                        error!("Failed to play video {}: {}", video.video_id, play_error);
                    }
                } else {
                    error!("Cannot play missing video {}", video.video_id);
                }
            }
            ui_context.context_menu.popdown();
        });
    }

    {
        let selected_video = selected_video.clone();
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        watch_later_button.connect_clicked(move |_| {
            if let Some(video) = selected_video.borrow().clone() {
                ui_context.context_menu.popdown();
                apply_watch_later_action(&state_rc, &ui_context, video);
            }
        });
    }

    {
        let selected_video = selected_video.clone();
        let ui_context = ui_context.clone();
        copy_url_button.connect_clicked(move |_| {
            if let Some(video) = selected_video.borrow().clone() {
                // GTK3's clipboard abstraction handles both X11 and Wayland via GDK
                gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD)
                    .set_text(&crate::urls::watch_url(&video.video_id));
            }
            ui_context.context_menu.popdown();
        });
    }

    {
        let selected_video = selected_video.clone();
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        summary_button.connect_clicked(move |_| {
            if let Some(ref video) = *selected_video.borrow() {
                ui_context.context_menu.popdown();
                show_summary_dialog(&state_rc, &ui_context, video);
            }
        });
    }

    {
        let selected_video = selected_video.clone();
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        transcript_button.connect_clicked(move |_| {
            if let Some(ref video) = *selected_video.borrow() {
                ui_context.context_menu.popdown();
                show_transcript_dialog(&state_rc, &ui_context, &video.video_id);
            }
        });
    }
}

pub(super) fn populate_flow_box(
    flow_box: &FlowBox,
    tab: Tab,
    downloaded_video_ids: &HashSet<String>,
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
) {
    // Clear existing children
    flow_box.foreach(|child| {
        flow_box.remove(child);
    });
    if tab == Tab::Feed {
        ui_context.card_button_index.borrow_mut().feed.clear();
    }

    let state = state_rc.borrow();
    let videos: Vec<&Video> = match tab {
        Tab::Feed => state.videos.iter().collect(),
        Tab::WatchLater => state
            .videos
            .iter()
            .filter(|video| state.watch_later.contains(video.video_id()))
            .collect(),
    };

    for video in videos {
        let thumbnail_path = state.storage.thumbnail_path(video.video_id());
        let is_watch_later = state.watch_later.contains(video.video_id());
        let is_downloaded = downloaded_video_ids.contains(video.video_id());

        let (card, watch_later_toggle, ai_summary_button) = create_video_card(
            video,
            &thumbnail_path,
            is_watch_later,
            is_downloaded,
            video.has_ai_summary(),
        );
        if tab == Tab::Feed {
            ui_context
                .card_button_index
                .borrow_mut()
                .feed
                .insert(video.video_id().to_string(), watch_later_toggle.clone());
        }

        let video_ref = SelectedVideo {
            video_id: video.video_id().to_string(),
        };
        let state_rc = state_rc.clone();
        let runtime = ui_context.runtime.clone();
        let selected_video = ui_context.selected_video.clone();
        let card_ui_context = ui_context.clone();

        let wl_state_rc = state_rc.clone();
        let wl_ui_context = ui_context.clone();
        let wl_ref = video_ref.clone();

        if let Some(ai_summary_button) = ai_summary_button {
            let summary_state_rc = state_rc.clone();
            let summary_ui_context = ui_context.clone();
            let summary_ref = video_ref.clone();

            ai_summary_button.connect_clicked(move |_| {
                show_summary_dialog(&summary_state_rc, &summary_ui_context, &summary_ref);
            });
        }

        // Double-click to play, right-click for context menu
        card.connect_button_press_event(
            clone!(@strong video_ref, @strong state_rc, @strong runtime => move |widget, event| {
                if event.button() == 1 && event.event_type() == gdk::EventType::DoubleButtonPress {
                    let video_id = video_ref.video_id.clone();
                    let playback = {
                        let state = state_rc.borrow();
                        state.video_by_id(&video_id).map(|current_video| {
                            let video_title = current_video.title().to_string();
                            let local_path = state.storage.find_video_path(&video_id);
                            let local_path = resolve_playback_path(
                                &state.storage,
                                runtime.clone(),
                                &video_id,
                                &video_title,
                                local_path,
                            );
                            (video_title, local_path)
                        })
                    };

                    if let Some((video_title, local_path)) = playback {
                        if let Err(play_error) = play_video(&video_id, &video_title, local_path.as_deref()) {
                            error!("Failed to play video {}: {}", video_id, play_error);
                        }
                    } else {
                        error!("Cannot play missing video {}", video_id);
                    }
                    return glib::Propagation::Stop;
                }

                if event.button() == 3 {
                    *selected_video.borrow_mut() = Some(video_ref.clone());
                    card_ui_context.context_menu.set_relative_to(Some(widget));
                    card_ui_context.context_menu.popup();
                    return glib::Propagation::Stop;
                }

                glib::Propagation::Proceed
            }),
        );

        watch_later_toggle.connect_clicked(move |_| {
            apply_watch_later_action(&wl_state_rc, &wl_ui_context, wl_ref.clone());
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
