use super::cards::refresh_video_summary_badges;
use super::{AppContext, AppState, CacheVideoError};
use crate::cache::fetch_transcript;
use crate::data::Video;
use crate::summary::{summarize_video_streaming, StreamingMessage};
use crate::ui::dialogs::{create_text_dialog, show_text_dialog};
use glib::clone;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Orientation};
use log::error;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use thiserror::Error;
use tokio::runtime::Runtime;

/// Configures request guards for starting summary generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SummaryGenerationMode {
    /// Skip generation if a summary exists or one is already running.
    Prefetch,
    /// Always start a new generation for explicit user actions.
    Interactive,
}

/// Start-time validation errors for summary generation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(super) enum StartSummaryGenerationError {
    #[error("Video is no longer available.")]
    MissingVideo,
    #[error("Summary is already cached.")]
    AlreadyCached,
    #[error("Summary generation is already in progress.")]
    AlreadyInProgress,
}

/// Stream-consumption errors for summary generation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(super) enum SummaryGenerationError {
    #[error("{0}")]
    Stream(String),
    #[error("Summary was empty")]
    EmptySummary,
}

/// Service responsible for summary generation orchestration.
#[derive(Clone)]
pub(super) struct SummaryGenerator {
    runtime: Arc<Runtime>,
    http_client: reqwest::Client,
    summaries_in_progress: Arc<RwLock<HashSet<String>>>,
}

impl SummaryGenerator {
    /// Creates a generator backed by the application's async runtime.
    pub(super) fn new(runtime: Arc<Runtime>, http_client: reqwest::Client) -> Self {
        Self {
            runtime,
            http_client,
            summaries_in_progress: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Validates inputs and starts a background summary stream.
    ///
    /// # Errors
    ///
    /// Returns [`StartSummaryGenerationError`] when generation should not start.
    pub(super) fn start(
        &self,
        state_rc: &Rc<RefCell<AppState>>,
        video_id: &str,
        mode: SummaryGenerationMode,
    ) -> Result<SummaryGenerationTask, StartSummaryGenerationError> {
        if matches!(mode, SummaryGenerationMode::Prefetch)
            && self
                .summaries_in_progress
                .read()
                .expect("summary in-progress rwlock must not be poisoned")
                .contains(video_id)
        {
            return Err(StartSummaryGenerationError::AlreadyInProgress);
        }

        let video = {
            let state = state_rc.borrow();
            let mut summaries_in_progress = self
                .summaries_in_progress
                .write()
                .expect("summary in-progress rwlock must not be poisoned");
            prepare_summary_generation_video(&state, &mut summaries_in_progress, video_id, mode)?
        };

        let transcripts_work_dir = {
            let state = state_rc.borrow();
            state.storage.transcripts_work_dir().to_path_buf()
        };

        let task_video_id = video.video_id().to_string();
        let (tx, result_rx) = async_channel::unbounded::<StreamingMessage>();
        let http_client = self.http_client.clone();
        self.runtime.spawn(async move {
            let video_url = video.watch_url();
            summarize_video_streaming(
                http_client,
                video.video_id(),
                &video_url,
                video.title(),
                video.channel_name(),
                &transcripts_work_dir,
                tx,
            )
            .await;
        });

        Ok(SummaryGenerationTask {
            video_id: task_video_id,
            result_rx,
        })
    }

    /// Removes a video's in-progress marker after a failed or cancelled generation.
    pub(super) fn clear_in_progress(&self, video_id: &str) {
        self.summaries_in_progress
            .write()
            .expect("summary in-progress rwlock must not be poisoned")
            .remove(video_id);
    }

    /// Persists the final summary and clears the in-progress marker.
    pub(super) fn persist_summary(
        &self,
        state_rc: &Rc<RefCell<AppState>>,
        video_id: &str,
        summary: String,
    ) -> Result<(), CacheVideoError> {
        self.summaries_in_progress
            .write()
            .expect("summary in-progress rwlock must not be poisoned")
            .remove(video_id);
        state_rc
            .borrow_mut()
            .cache_video_ai_summary(video_id, summary)
    }

    /// Persists a summary and refreshes the UI badge for the given video.
    ///
    /// # Errors
    ///
    /// Returns [`CacheVideoError`] if persisting fails.
    pub(super) fn persist_and_refresh(
        &self,
        state_rc: &Rc<RefCell<AppState>>,
        ui_context: &AppContext,
        video_id: &str,
        summary: String,
    ) -> Result<(), CacheVideoError> {
        self.persist_summary(state_rc, video_id, summary)?;
        let has_summary = state_rc
            .borrow()
            .video_by_id(video_id)
            .is_some_and(Video::has_ai_summary);
        refresh_video_summary_badges(ui_context, video_id, has_summary);
        Ok(())
    }
}

/// In-flight summary stream handle.
pub(super) struct SummaryGenerationTask {
    video_id: String,
    result_rx: async_channel::Receiver<StreamingMessage>,
}

impl SummaryGenerationTask {
    /// Returns the video id associated with this task.
    pub(super) fn video_id(&self) -> &str {
        &self.video_id
    }

    /// Collects the full summary body.
    ///
    /// # Errors
    ///
    /// Returns [`SummaryGenerationError`] if streaming fails or produces empty output.
    pub(super) async fn collect(self) -> Result<String, SummaryGenerationError> {
        self.collect_with_chunks(|_| {}).await
    }

    /// Collects a full summary while forwarding each chunk to `on_chunk`.
    ///
    /// # Errors
    ///
    /// Returns [`SummaryGenerationError`] if streaming fails or produces empty output.
    pub(super) async fn collect_with_chunks<F>(
        self,
        mut on_chunk: F,
    ) -> Result<String, SummaryGenerationError>
    where
        F: FnMut(&str),
    {
        let mut summary = String::new();

        while let Ok(message) = self.result_rx.recv().await {
            match message {
                StreamingMessage::Chunk(text) => {
                    on_chunk(&text);
                    summary.push_str(&text);
                }
                StreamingMessage::Done => break,
                StreamingMessage::Error(error_text) => {
                    return Err(SummaryGenerationError::Stream(error_text));
                }
            }
        }

        let summary = summary.trim().to_string();
        if summary.is_empty() {
            Err(SummaryGenerationError::EmptySummary)
        } else {
            Ok(summary)
        }
    }
}

fn prepare_summary_generation_video(
    state: &AppState,
    summaries_in_progress: &mut HashSet<String>,
    video_id: &str,
    mode: SummaryGenerationMode,
) -> Result<Video, StartSummaryGenerationError> {
    let Some(video) = state.video_by_id(video_id).cloned() else {
        return Err(StartSummaryGenerationError::MissingVideo);
    };

    let has_cached_summary = video.has_ai_summary();

    if matches!(mode, SummaryGenerationMode::Prefetch) && has_cached_summary {
        return Err(StartSummaryGenerationError::AlreadyCached);
    }
    if matches!(mode, SummaryGenerationMode::Prefetch) && summaries_in_progress.contains(video_id) {
        return Err(StartSummaryGenerationError::AlreadyInProgress);
    }

    summaries_in_progress.insert(video_id.to_string());
    Ok(video)
}

pub(super) fn maybe_prefetch_summary_for_watch_later(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let summary_generator = ui_context.summary_generator.clone();
    let generation_task =
        match summary_generator.start(state_rc, video_id, SummaryGenerationMode::Prefetch) {
            Ok(task) => task,
            Err(StartSummaryGenerationError::MissingVideo) => {
                error!("Cannot prefetch summary for missing video {video_id}");
                return;
            }
            Err(
                StartSummaryGenerationError::AlreadyCached
                | StartSummaryGenerationError::AlreadyInProgress,
            ) => {
                return;
            }
        };

    glib::MainContext::default().spawn_local(clone!(@strong state_rc, @strong ui_context, @strong summary_generator => async move {
        let video_id = generation_task.video_id().to_string();
        match generation_task.collect().await {
            Err(generation_error) => {
                summary_generator.clear_in_progress(&video_id);
                error!("Failed to prefetch summary for {video_id}: {generation_error}");
            }
            Ok(summary) => {
                if let Err(cache_error) =
                    summary_generator.persist_and_refresh(&state_rc, &ui_context, &video_id, summary)
                {
                    error!("Failed to cache prefetched summary for {video_id}: {cache_error}");
                }
            }
        }
    }));
}

fn insert_stream_chunk(buffer: &gtk::TextBuffer, text: &str) {
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, text);
}

fn start_summary_generation_for_dialog(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
    buffer: &gtk::TextBuffer,
    regenerate_button: &Button,
    loading_text: &str,
) {
    buffer.set_text(loading_text);
    regenerate_button.set_sensitive(false);

    let summary_generator = ui_context.summary_generator.clone();
    let generation_task =
        match summary_generator.start(state_rc, video_id, SummaryGenerationMode::Interactive) {
            Ok(task) => task,
            Err(StartSummaryGenerationError::MissingVideo) => {
                buffer.set_text("Error: Video is no longer available.");
                regenerate_button.set_sensitive(true);
                error!("Cannot generate summary for missing video {video_id}");
                return;
            }
            Err(start_error) => {
                buffer.set_text(&format!("Error: {start_error}"));
                regenerate_button.set_sensitive(true);
                return;
            }
        };

    glib::MainContext::default().spawn_local(clone!(@strong state_rc, @strong ui_context, @strong buffer, @strong regenerate_button, @strong summary_generator => async move {
        let video_id = generation_task.video_id().to_string();
        let mut received_chunk = false;
        let generation_result = generation_task
            .collect_with_chunks(|text| {
                if !received_chunk {
                    buffer.set_text("");
                    received_chunk = true;
                }
                insert_stream_chunk(&buffer, text);
            })
            .await;

        regenerate_button.set_sensitive(true);

        match generation_result {
            Err(generation_error) => {
                summary_generator.clear_in_progress(&video_id);
                buffer.set_text(&format!("Error: {generation_error}"));
            }
            Ok(summary) => {
                if let Err(cache_error) =
                    summary_generator.persist_and_refresh(&state_rc, &ui_context, &video_id, summary)
                {
                    buffer.set_text(&format!("Error: {cache_error}"));
                    error!("Failed to cache interactive summary for {video_id}: {cache_error}");
                }
            }
        }
    }));
}

pub(super) fn show_summary_dialog(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let (video_title, cached_summary) = {
        let state = state_rc.borrow();
        let Some(video) = state.video_by_id(video_id) else {
            error!("Cannot open summary dialog for missing video {video_id}");
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

    let regenerate_button = Button::with_label("Regenerate Summary");
    let regenerate_button_for_layout = regenerate_button.clone();
    let (_dialog, buffer) = create_text_dialog(
        &ui_context.window,
        &format!("Summary: {video_title}"),
        "",
        move |content_area| {
            let controls_row = GtkBox::new(Orientation::Horizontal, 8);
            controls_row.set_margin_start(8);
            controls_row.set_margin_end(8);
            controls_row.set_margin_top(8);

            let spacer = GtkBox::new(Orientation::Horizontal, 0);
            spacer.set_hexpand(true);
            controls_row.pack_start(&spacer, true, true, 0);
            controls_row.pack_end(&regenerate_button_for_layout, false, false, 0);
            content_area.pack_start(&controls_row, false, false, 0);
        },
    );

    let video_id = video_id.to_string();

    if let Some(summary) = cached_summary {
        buffer.set_text(&summary);
    } else {
        start_summary_generation_for_dialog(
            state_rc,
            ui_context,
            &video_id,
            &buffer,
            &regenerate_button,
            "Loading summary...",
        );
    }

    regenerate_button.connect_clicked(clone!(@strong state_rc, @strong ui_context, @strong video_id, @strong buffer, @strong regenerate_button => move |_| {
        start_summary_generation_for_dialog(
            &state_rc,
            &ui_context,
            &video_id,
            &buffer,
            &regenerate_button,
            "Regenerating summary..."
        );
    }));
}

pub(super) fn show_transcript_dialog(
    state_rc: &Rc<RefCell<AppState>>,
    ui_context: &AppContext,
    video_id: &str,
) {
    let (video_title, cached_transcript) = {
        let state = state_rc.borrow();
        let Some(video) = state.video_by_id(video_id) else {
            error!("Cannot open transcript dialog for missing video {video_id}");
            return;
        };
        (
            video.title().to_string(),
            video.transcript().map(ToString::to_string),
        )
    };

    if let Some(transcript) = cached_transcript {
        show_text_dialog(
            &ui_context.window,
            &format!("Transcript: {video_title}"),
            &transcript,
        );
        return;
    }

    let (_dialog, buffer) = create_text_dialog(
        &ui_context.window,
        &format!("Transcript: {video_title}"),
        "Loading transcript...",
        |_| {},
    );

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
                        error!("Failed to cache transcript for {video_id}: {cache_error}");
                    }
                }
                Err(transcript_error) => {
                    buffer.set_text(&format!("Error: {transcript_error}"));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        prepare_summary_generation_video, StartSummaryGenerationError, SummaryGenerationError,
        SummaryGenerationMode, SummaryGenerationTask,
    };
    use crate::cache::Storage;
    use crate::data::Video;
    use crate::summary::StreamingMessage;
    use crate::ui::app::AppState;

    use chrono::Utc;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirs {
        root: PathBuf,
    }

    impl TestDirs {
        fn new() -> Self {
            let unique_id = NEXT_TEST_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "yt-gtk-summary-generator-tests-{}-{}",
                std::process::id(),
                unique_id
            ));
            std::fs::create_dir_all(&root).expect("test directory must be creatable");
            Self { root }
        }

        fn data_dir(&self) -> PathBuf {
            self.root.join("data")
        }

        fn cache_dir(&self) -> PathBuf {
            self.root.join("cache")
        }
    }

    impl Drop for TestDirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn test_video(video_id: &str) -> Video {
        Video::new(
            video_id.to_string(),
            "channel-id".to_string(),
            "Channel".to_string(),
            "Video Title",
            Utc::now(),
            "https://example.com/thumb.jpg".to_string(),
            None,
        )
    }

    fn test_state(videos: Vec<Video>) -> (AppState, TestDirs) {
        let dirs = TestDirs::new();
        let storage = Storage::new_at(dirs.data_dir(), dirs.cache_dir())
            .expect("test storage must initialize");
        let feed_video_ids = videos
            .iter()
            .map(|video| video.video_id().to_string())
            .collect();
        let state = AppState::new(videos, feed_video_ids, HashSet::new(), storage);
        (state, dirs)
    }

    #[test]
    fn prepare_request_returns_missing_video_error() {
        let (state, _dirs) = test_state(Vec::new());
        let mut summaries_in_progress = HashSet::new();

        let result = prepare_summary_generation_video(
            &state,
            &mut summaries_in_progress,
            "missing-video-id",
            SummaryGenerationMode::Prefetch,
        );

        assert!(matches!(
            result,
            Err(StartSummaryGenerationError::MissingVideo)
        ));
    }

    #[test]
    fn prepare_request_skips_prefetch_when_summary_cached() {
        let video_id = "video-id";
        let mut video = test_video(video_id);
        video.set_ai_summary(Some("cached summary".to_string()));
        let (state, _dirs) = test_state(vec![video]);
        let mut summaries_in_progress = HashSet::new();

        let result = prepare_summary_generation_video(
            &state,
            &mut summaries_in_progress,
            video_id,
            SummaryGenerationMode::Prefetch,
        );

        assert!(matches!(
            result,
            Err(StartSummaryGenerationError::AlreadyCached)
        ));
        assert!(!summaries_in_progress.contains(video_id));
    }

    #[test]
    fn prepare_request_skips_prefetch_when_already_running() {
        let video_id = "video-id";
        let (state, _dirs) = test_state(vec![test_video(video_id)]);
        let mut summaries_in_progress = HashSet::from([video_id.to_string()]);

        let result = prepare_summary_generation_video(
            &state,
            &mut summaries_in_progress,
            video_id,
            SummaryGenerationMode::Prefetch,
        );

        assert!(matches!(
            result,
            Err(StartSummaryGenerationError::AlreadyInProgress)
        ));
    }

    #[test]
    fn prepare_request_marks_in_progress_for_new_prefetch() {
        let video_id = "video-id";
        let (state, _dirs) = test_state(vec![test_video(video_id)]);
        let mut summaries_in_progress = HashSet::new();

        let result = prepare_summary_generation_video(
            &state,
            &mut summaries_in_progress,
            video_id,
            SummaryGenerationMode::Prefetch,
        );

        assert!(result.is_ok());
        assert!(summaries_in_progress.contains(video_id));
    }

    #[test]
    fn prepare_request_allows_interactive_regeneration_when_cached() {
        let video_id = "video-id";
        let mut video = test_video(video_id);
        video.set_ai_summary(Some("cached summary".to_string()));
        let (state, _dirs) = test_state(vec![video]);
        let mut summaries_in_progress = HashSet::from([video_id.to_string()]);

        let result = prepare_summary_generation_video(
            &state,
            &mut summaries_in_progress,
            video_id,
            SummaryGenerationMode::Interactive,
        )
        .expect("interactive generation should start");

        assert_eq!(result.video_id(), video_id);
        assert!(summaries_in_progress.contains(video_id));
    }

    #[tokio::test]
    async fn collect_with_chunks_returns_trimmed_summary() {
        let (tx, rx) = async_channel::unbounded();
        tx.send(StreamingMessage::Chunk(" hello".to_string()))
            .await
            .expect("chunk send should succeed");
        tx.send(StreamingMessage::Chunk(" world ".to_string()))
            .await
            .expect("chunk send should succeed");
        tx.send(StreamingMessage::Done)
            .await
            .expect("done send should succeed");

        let mut rendered = String::new();
        let summary = SummaryGenerationTask {
            video_id: "video-id".to_string(),
            result_rx: rx,
        }
        .collect_with_chunks(|chunk| rendered.push_str(chunk))
        .await
        .expect("summary should be collected");

        assert_eq!(summary, "hello world");
        assert_eq!(rendered, " hello world ");
    }

    #[tokio::test]
    async fn collect_returns_stream_error() {
        let (tx, rx) = async_channel::unbounded();
        tx.send(StreamingMessage::Error("provider failed".to_string()))
            .await
            .expect("error send should succeed");

        let result = SummaryGenerationTask {
            video_id: "video-id".to_string(),
            result_rx: rx,
        }
        .collect()
        .await;

        assert_eq!(
            result,
            Err(SummaryGenerationError::Stream(
                "provider failed".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn collect_returns_empty_summary_error() {
        let (tx, rx) = async_channel::unbounded();
        tx.send(StreamingMessage::Chunk("   ".to_string()))
            .await
            .expect("chunk send should succeed");
        tx.send(StreamingMessage::Done)
            .await
            .expect("done send should succeed");

        let result = SummaryGenerationTask {
            video_id: "video-id".to_string(),
            result_rx: rx,
        }
        .collect()
        .await;

        assert_eq!(result, Err(SummaryGenerationError::EmptySummary));
    }
}
