use super::refresh::refresh_video_lists;
use super::summary_generator::{
    StartSummaryGenerationError, SummaryGenerationMode, SummaryGenerator,
};
use super::{create_readonly_text_scroller, AppState, UiContext};
use crate::cache::fetch_transcript;
use crate::ui::dialogs::show_text_dialog;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Orientation};
use log::error;
use std::cell::RefCell;
use std::rc::Rc;

pub(super) fn maybe_prefetch_summary_for_watch_later(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
    video_id: &str,
) {
    let summary_generator =
        SummaryGenerator::new(ui_context.runtime.clone(), ui_context.http_client.clone());
    let generation_task =
        match summary_generator.start(state_rc, video_id, SummaryGenerationMode::Prefetch) {
            Ok(task) => task,
            Err(StartSummaryGenerationError::MissingVideo) => {
                error!("Cannot prefetch summary for missing video {}", video_id);
                return;
            }
            Err(
                StartSummaryGenerationError::AlreadyCached
                | StartSummaryGenerationError::AlreadyInProgress,
            ) => {
                return;
            }
        };

    let state_for_result = state_rc.clone();
    let ui_context_for_result = ui_context.clone();
    let summary_generator_for_result = summary_generator.clone();

    glib::MainContext::default().spawn_local(async move {
        let video_id_for_result = generation_task.video_id().to_string();
        match generation_task.collect().await {
            Err(generation_error) => {
                summary_generator_for_result
                    .clear_in_progress(&state_for_result, &video_id_for_result);
                error!(
                    "Failed to prefetch summary for {}: {}",
                    video_id_for_result, generation_error
                );
            }
            Ok(summary) => {
                if let Err(cache_error) = summary_generator_for_result.persist_summary(
                    &state_for_result,
                    &video_id_for_result,
                    summary,
                ) {
                    error!(
                        "Failed to cache prefetched summary for {}: {}",
                        video_id_for_result, cache_error
                    );
                    return;
                }

                refresh_video_lists(&state_for_result, &ui_context_for_result);
            }
        }
    });
}

fn insert_stream_chunk(buffer: &gtk::TextBuffer, text: &str) {
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, text);
}

fn start_summary_generation_for_dialog(
    state_rc: Rc<RefCell<AppState>>,
    ui_context: UiContext,
    video_id: String,
    buffer: gtk::TextBuffer,
    regenerate_button: Button,
    loading_text: &str,
) {
    buffer.set_text(loading_text);
    regenerate_button.set_sensitive(false);

    let summary_generator =
        SummaryGenerator::new(ui_context.runtime.clone(), ui_context.http_client.clone());
    let generation_task =
        match summary_generator.start(&state_rc, &video_id, SummaryGenerationMode::Interactive) {
            Ok(task) => task,
            Err(StartSummaryGenerationError::MissingVideo) => {
                buffer.set_text("Error: Video is no longer available.");
                regenerate_button.set_sensitive(true);
                error!("Cannot generate summary for missing video {}", video_id);
                return;
            }
            Err(start_error) => {
                buffer.set_text(&format!("Error: {}", start_error));
                regenerate_button.set_sensitive(true);
                return;
            }
        };

    let state_for_result = state_rc.clone();
    let ui_context_for_result = ui_context.clone();
    let buffer_for_result = buffer.clone();
    let button_for_result = regenerate_button.clone();
    let summary_generator_for_result = summary_generator.clone();

    glib::MainContext::default().spawn_local(async move {
        let video_id_for_result = generation_task.video_id().to_string();
        let mut received_chunk = false;
        let generation_result = generation_task
            .collect_with_chunks(|text| {
                if !received_chunk {
                    buffer_for_result.set_text("");
                    received_chunk = true;
                }
                insert_stream_chunk(&buffer_for_result, text);
            })
            .await;

        button_for_result.set_sensitive(true);

        match generation_result {
            Err(generation_error) => {
                summary_generator_for_result
                    .clear_in_progress(&state_for_result, &video_id_for_result);
                buffer_for_result.set_text(&format!("Error: {}", generation_error));
            }
            Ok(summary) => {
                if let Err(cache_error) = summary_generator_for_result.persist_summary(
                    &state_for_result,
                    &video_id_for_result,
                    summary,
                ) {
                    buffer_for_result.set_text(&format!("Error: {}", cache_error));
                    error!(
                        "Failed to cache interactive summary for {}: {}",
                        video_id_for_result, cache_error
                    );
                    return;
                }

                refresh_video_lists(&state_for_result, &ui_context_for_result);
            }
        }
    });
}

pub(super) fn show_summary_dialog(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
    video_id: &str,
) {
    let (video_title, cached_summary) = {
        let state = state_rc.borrow();
        let Some(video) = state.video_by_id(video_id) else {
            error!("Cannot open summary dialog for missing video {}", video_id);
            return;
        };
        (
            video.title().to_string(),
            video
                .ai_summary()
                .map(ToString::to_string)
                .filter(|summary| !summary.trim().is_empty()),
        )
    };

    let dialog = gtk::Dialog::with_buttons(
        Some(&format!("Summary: {}", video_title)),
        Some(&ui_context.window),
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

    let (scrolled, buffer) = create_readonly_text_scroller("");
    content_area.pack_start(&scrolled, true, true, 0);

    let state_rc_for_dialog = state_rc.clone();
    let ui_context_for_dialog = ui_context.clone();
    let video_id = video_id.to_string();

    if let Some(summary) = cached_summary {
        buffer.set_text(&summary);
    } else {
        start_summary_generation_for_dialog(
            state_rc_for_dialog.clone(),
            ui_context_for_dialog.clone(),
            video_id.clone(),
            buffer.clone(),
            regenerate_button.clone(),
            "Loading summary...",
        );
    }

    {
        let state_rc_for_click = state_rc_for_dialog.clone();
        let ui_context_for_click = ui_context_for_dialog.clone();
        let buffer_for_click = buffer.clone();
        let regenerate_button_for_click = regenerate_button.clone();
        let video_id_for_click = video_id.clone();

        regenerate_button.connect_clicked(move |_| {
            start_summary_generation_for_dialog(
                state_rc_for_click.clone(),
                ui_context_for_click.clone(),
                video_id_for_click.clone(),
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

pub(super) fn show_transcript_dialog(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
    video_id: &str,
) {
    let (video_title, cached_transcript) = {
        let state = state_rc.borrow();
        let Some(video) = state.video_by_id(video_id) else {
            error!(
                "Cannot open transcript dialog for missing video {}",
                video_id
            );
            return;
        };
        (
            video.title().to_string(),
            video.transcript().map(ToString::to_string),
        )
    };

    // Check if we already have the transcript cached
    if let Some(transcript) = cached_transcript {
        show_text_dialog(
            &ui_context.window,
            &format!("Transcript: {}", video_title),
            &transcript,
        );
        return;
    }

    // Need to fetch transcript
    let dialog = gtk::Dialog::with_buttons(
        Some(&format!("Transcript: {}", video_title)),
        Some(&ui_context.window),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[("Close", gtk::ResponseType::Close)],
    );
    dialog.set_default_size(700, 500);

    let content_area = dialog.content_area();
    let (scrolled, buffer) = create_readonly_text_scroller("Loading transcript...");
    content_area.pack_start(&scrolled, true, true, 0);

    dialog.show_all();

    // Fetch transcript
    let work_dir = state_rc
        .borrow()
        .storage
        .transcripts_work_dir()
        .to_path_buf();

    let (tx, rx) = async_channel::bounded::<Result<String, String>>(1);

    let video_id_for_thread = video_id.to_string();
    let runtime = ui_context.runtime.clone();
    runtime.spawn(async move {
        match fetch_transcript(&video_id_for_thread, &work_dir).await {
            Ok(transcript) => {
                let _ = tx.send(Ok(transcript)).await;
            }
            Err(transcript_error) => {
                let _ = tx.send(Err(transcript_error.to_string())).await;
            }
        }
    });

    let video_id = video_id.to_string();
    let state_rc = state_rc.clone();
    glib::MainContext::default().spawn_local(async move {
        if let Ok(result) = rx.recv().await {
            match result {
                Ok(transcript) => {
                    buffer.set_text(&transcript);
                    let mut state = state_rc.borrow_mut();
                    if let Err(cache_error) = state.cache_video_transcript(&video_id, transcript) {
                        error!(
                            "Failed to cache transcript for {}: {}",
                            video_id, cache_error
                        );
                    }
                }
                Err(transcript_error) => {
                    buffer.set_text(&format!("Error: {}", transcript_error));
                }
            }
        }
    });

    dialog.connect_response(|dialog, _| {
        dialog.close();
    });
}
