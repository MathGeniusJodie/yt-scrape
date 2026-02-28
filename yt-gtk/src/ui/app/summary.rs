use super::refresh::refresh_video_lists;
use super::{create_readonly_text_scroller, AppState, SelectedVideo, UiContext};
use crate::cache::fetch_transcript;
use crate::gemini::{summarize_video_streaming, StreamingMessage};
use crate::ui::dialogs::show_text_dialog;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Orientation};
use log::error;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

fn spawn_summary_generation(
    runtime: Arc<Runtime>,
    video_id: String,
    video_url: String,
    video_title: String,
    channel_name: String,
    transcripts_work_dir: PathBuf,
) -> async_channel::Receiver<Result<String, String>> {
    let (tx, rx) = async_channel::bounded::<Result<String, String>>(1);

    std::thread::spawn(move || {
        let result = runtime.block_on(async {
            let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel();
            summarize_video_streaming(
                &video_id,
                &video_url,
                &video_title,
                &channel_name,
                &transcripts_work_dir,
                chunk_tx,
            )
            .await;

            let mut summary = String::new();
            let mut error: Option<String> = None;

            while let Some(message) = chunk_rx.recv().await {
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

        let _ = tx.send_blocking(result);
    });

    rx
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

pub(super) fn maybe_prefetch_summary_for_watch_later(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
    request: SelectedVideo,
) {
    let should_prefetch = {
        let mut state = state_rc.borrow_mut();
        let has_summary = state
            .videos
            .iter()
            .any(|video| video.video_id == request.video_id && video.has_ai_summary());

        if has_summary || state.summaries_in_progress.contains(&request.video_id) {
            false
        } else {
            state.summaries_in_progress.insert(request.video_id.clone());
            true
        }
    };

    if !should_prefetch {
        return;
    }

    let transcripts_work_dir = state_rc
        .borrow()
        .storage
        .transcripts_work_dir()
        .to_path_buf();
    let result_rx = spawn_summary_generation(
        ui_context.runtime.clone(),
        request.video_id.clone(),
        request.video_url,
        request.video_title,
        request.channel_name,
        transcripts_work_dir,
    );

    let state_for_result = state_rc.clone();
    let ui_context_for_result = ui_context.clone();
    let video_id_for_result = request.video_id.clone();

    glib::MainContext::default().spawn_local(async move {
        if let Ok(result) = result_rx.recv().await {
            match result {
                Ok(summary) => {
                    if persist_summary_to_cache(&state_for_result, &video_id_for_result, summary) {
                        refresh_video_lists(&state_for_result, &ui_context_for_result);
                    }
                }
                Err(summary_error) => {
                    state_for_result
                        .borrow_mut()
                        .summaries_in_progress
                        .remove(&video_id_for_result);
                    error!(
                        "Failed to prefetch summary for {}: {}",
                        video_id_for_result, summary_error
                    );
                }
            }
        }
    });
}

fn start_summary_generation_for_dialog(
    state_rc: Rc<RefCell<AppState>>,
    ui_context: UiContext,
    request: SelectedVideo,
    buffer: gtk::TextBuffer,
    regenerate_button: Button,
    loading_text: &str,
) {
    buffer.set_text(loading_text);
    regenerate_button.set_sensitive(false);

    state_rc
        .borrow_mut()
        .summaries_in_progress
        .insert(request.video_id.clone());

    let transcripts_work_dir = state_rc
        .borrow()
        .storage
        .transcripts_work_dir()
        .to_path_buf();
    let result_rx = spawn_summary_generation(
        ui_context.runtime.clone(),
        request.video_id.clone(),
        request.video_url,
        request.video_title,
        request.channel_name,
        transcripts_work_dir,
    );

    let state_for_result = state_rc.clone();
    let ui_context_for_result = ui_context.clone();
    let video_id_for_result = request.video_id.clone();
    let buffer_for_result = buffer.clone();
    let button_for_result = regenerate_button.clone();

    glib::MainContext::default().spawn_local(async move {
        if let Ok(result) = result_rx.recv().await {
            button_for_result.set_sensitive(true);

            match result {
                Ok(summary) => {
                    buffer_for_result.set_text(&summary);
                    if persist_summary_to_cache(&state_for_result, &video_id_for_result, summary) {
                        refresh_video_lists(&state_for_result, &ui_context_for_result);
                    }
                }
                Err(summary_error) => {
                    state_for_result
                        .borrow_mut()
                        .summaries_in_progress
                        .remove(&video_id_for_result);
                    buffer_for_result.set_text(&format!("Error: {}", summary_error));
                }
            }
        }
    });
}

pub(super) fn show_summary_dialog(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
    request: &SelectedVideo,
) {
    let dialog = gtk::Dialog::with_buttons(
        Some(&format!("Summary: {}", request.video_title)),
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
    let request = request.clone();

    let cached_summary = {
        let state = state_rc.borrow();
        state
            .videos
            .iter()
            .find(|video| video.video_id == request.video_id)
            .and_then(|video| video.ai_summary.clone())
            .filter(|summary| !summary.trim().is_empty())
    };

    if let Some(summary) = cached_summary {
        buffer.set_text(&summary);
    } else {
        start_summary_generation_for_dialog(
            state_rc_for_dialog.clone(),
            ui_context_for_dialog.clone(),
            request.clone(),
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
        let request_for_click = request.clone();

        regenerate_button.connect_clicked(move |_| {
            start_summary_generation_for_dialog(
                state_rc_for_click.clone(),
                ui_context_for_click.clone(),
                request_for_click.clone(),
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
    video_title: &str,
) {
    // Check if we already have the transcript cached
    {
        let state = state_rc.borrow();
        if let Some(video) = state.videos.iter().find(|video| video.video_id == video_id) {
            if let Some(transcript) = &video.transcript {
                show_text_dialog(
                    &ui_context.window,
                    &format!("Transcript: {}", video_title),
                    transcript,
                );
                return;
            }
        }
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
    std::thread::spawn(move || {
        runtime.block_on(async {
            match fetch_transcript(&video_id_for_thread, &work_dir).await {
                Ok(transcript) => {
                    let _ = tx.send(Ok(transcript)).await;
                }
                Err(transcript_error) => {
                    let _ = tx.send(Err(transcript_error.to_string())).await;
                }
            }
        });
    });

    let video_id = video_id.to_string();
    let state_rc = state_rc.clone();
    glib::MainContext::default().spawn_local(async move {
        if let Ok(result) = rx.recv().await {
            match result {
                Ok(transcript) => {
                    buffer.set_text(&transcript);
                    let mut state = state_rc.borrow_mut();
                    if let Some(video) = state.videos.iter_mut().find(|v| v.video_id == video_id) {
                        video.transcript = Some(transcript);
                    }
                    let _ = state.storage.save_videos(&state.videos);
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
