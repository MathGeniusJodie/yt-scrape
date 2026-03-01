use super::refresh::refresh_video_lists;
use super::{create_readonly_text_scroller, AppState, SelectedVideo, UiContext};
use crate::cache::fetch_transcript;
use crate::data::Video;
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

#[derive(Clone)]
struct SummaryGenerationRequest {
    video_id: String,
    video_url: String,
    video_title: String,
    channel_name: String,
}

impl SummaryGenerationRequest {
    fn from_video(video: &Video) -> Self {
        Self {
            video_id: video.video_id().to_string(),
            video_url: video.watch_url(),
            video_title: video.title().to_string(),
            channel_name: video.channel_name().to_string(),
        }
    }
}

fn summary_generation_request(
    state: &AppState,
    video_id: &str,
) -> Option<SummaryGenerationRequest> {
    state
        .video_by_id(video_id)
        .map(SummaryGenerationRequest::from_video)
}

fn spawn_buffered_summary_generation(
    runtime: Arc<Runtime>,
    client: reqwest::Client,
    video_id: String,
    video_url: String,
    video_title: String,
    channel_name: String,
    transcripts_work_dir: PathBuf,
) -> async_channel::Receiver<Result<String, String>> {
    let (tx, rx) = async_channel::bounded::<Result<String, String>>(1);

    runtime.spawn(async move {
        let result = {
            let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel();
            summarize_video_streaming(
                client.clone(),
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
        };

        let _ = tx.send(result).await;
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

    if let Err(save_error) = state.storage.save_video_ai_summary(video_id, &summary) {
        error!(
            "Failed to persist summary sidecar for {}: {}",
            video_id, save_error
        );
    }

    let mut updated = false;
    if let Some(video) = state.video_by_id_mut(video_id) {
        video.set_ai_summary(Some(summary));
        updated = true;
    }

    updated
}

pub(super) fn maybe_prefetch_summary_for_watch_later(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &UiContext,
    video_id: &str,
) {
    let summary_request = {
        let mut state = state_rc.borrow_mut();
        let Some(video) = state.video_by_id(video_id) else {
            error!("Cannot prefetch summary for missing video {}", video_id);
            return;
        };
        let has_summary = video.has_ai_summary();
        let request = SummaryGenerationRequest::from_video(video);

        if has_summary || state.summaries_in_progress.contains(video_id) {
            None
        } else {
            state.summaries_in_progress.insert(video_id.to_string());
            Some(request)
        }
    };
    let Some(request) = summary_request else {
        return;
    };

    let (transcripts_work_dir, summary_client) = {
        let state = state_rc.borrow();
        (
            state.storage.transcripts_work_dir().to_path_buf(),
            state.http_client(),
        )
    };
    let result_rx = spawn_buffered_summary_generation(
        ui_context.runtime.clone(),
        summary_client,
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

    let summary_request = {
        let mut state = state_rc.borrow_mut();
        let request_data = summary_generation_request(&state, &request.video_id);
        if request_data.is_some() {
            state.summaries_in_progress.insert(request.video_id.clone());
        }
        request_data
    };
    let Some(summary_request) = summary_request else {
        buffer.set_text("Error: Video is no longer available.");
        regenerate_button.set_sensitive(true);
        error!(
            "Cannot generate summary for missing video {}",
            request.video_id
        );
        return;
    };

    let (transcripts_work_dir, summary_client) = {
        let state = state_rc.borrow();
        (
            state.storage.transcripts_work_dir().to_path_buf(),
            state.http_client(),
        )
    };
    let result_rx = spawn_buffered_summary_generation(
        ui_context.runtime.clone(),
        summary_client,
        summary_request.video_id.clone(),
        summary_request.video_url,
        summary_request.video_title,
        summary_request.channel_name,
        transcripts_work_dir,
    );

    let state_for_result = state_rc.clone();
    let ui_context_for_result = ui_context.clone();
    let video_id_for_result = summary_request.video_id.clone();
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
    let (video_title, cached_summary) = {
        let state = state_rc.borrow();
        let Some(video) = state.video_by_id(&request.video_id) else {
            error!(
                "Cannot open summary dialog for missing video {}",
                request.video_id
            );
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
    let request = request.clone();

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
                    if let Err(save_error) =
                        state.storage.save_video_transcript(&video_id, &transcript)
                    {
                        error!(
                            "Failed to persist transcript sidecar for {}: {}",
                            video_id, save_error
                        );
                    }
                    if let Some(video) = state.video_by_id_mut(&video_id) {
                        video.set_transcript(Some(transcript));
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
