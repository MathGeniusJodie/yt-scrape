use super::{AppContext, AppState};
use crate::feed::fetch_youtube_comments;
use crate::ui::dialogs::create_text_dialog;

use gtk::glib;
use gtk::prelude::*;
use log::error;
use std::cell::RefCell;
use std::rc::Rc;

/// Shows a comments dialog for the selected video.
///
/// # Arguments
///
/// * `state_rc` - Shared application state used to read the video title.
/// * `ui_context` - UI and runtime handles used to create the dialog and fetch comments.
/// * `video_id` - `YouTube` video ID whose comments should be displayed.
pub(super) fn show_comments_dialog(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let video_title = {
        let state = state_rc.borrow();
        let Some(video) = state.video_by_id(video_id) else {
            error!("Cannot open comments dialog for missing video {video_id}");
            return;
        };
        video.title().to_string()
    };

    let (_dialog, buffer) = create_text_dialog(
        &ui_context.window,
        &format!("Comments: {video_title}"),
        "Loading comments...",
        |_| {},
    );

    let Some(api_key) = ui_context.config.google_api_key.clone() else {
        buffer.set_text("GOOGLE_API_KEY is not set. Set it before loading comments.");
        return;
    };

    let client = ui_context.http_client.clone();
    let video_id_for_task = video_id.to_string();
    let rx = super::run_in_background(&ui_context.runtime, async move {
        fetch_youtube_comments(&client, &api_key, &video_id_for_task).await
    });

    glib::MainContext::default().spawn_local(async move {
        if let Ok(result) = rx.recv().await {
            match result {
                Ok(comments) => buffer.set_text(&comments),
                Err(comment_error) => buffer.set_text(&format!("Error: {comment_error}")),
            }
        }
    });
}
