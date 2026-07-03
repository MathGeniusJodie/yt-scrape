use super::cards::refresh_video_summary_badges;
use super::{AppContext, AppState, CacheVideoError};
use crate::cache::{fetch_transcript, transcript_from_vtt_file};
use crate::data::Video;
use crate::summary::{SummarizeRequest, SummaryOutcome, summarize_video};
use crate::ui::dialogs::{create_text_dialog, show_text_dialog};
use gtk::glib;
use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Orientation};
use log::{error, info};

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;
use tokio::runtime::Runtime;

const TRANSCRIPT_FALLBACK_NOTICE: &str =
    "No AI provider was available; showing the raw transcript instead.\n\n";

/// Configures request guards for starting summary generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SummaryGenerationMode {
    /// Skip generation if a summary exists or one is already running.
    Prefetch,
    /// Start a new generation for explicit user actions (still refuses to run
    /// concurrently with another generation for the same video).
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

/// Service responsible for summary generation orchestration.
///
/// Main-thread only: generation results are delivered back to the GLib main
/// context, so the in-progress guard needs no cross-thread synchronization.
#[derive(Clone)]
pub(super) struct SummaryGenerator {
    runtime: Arc<Runtime>,
    http_client: reqwest::Client,
    summaries_in_progress: Rc<RefCell<HashSet<String>>>,
}

impl SummaryGenerator {
    /// Creates a generator backed by the application's async runtime.
    pub(super) fn new(runtime: Arc<Runtime>, http_client: reqwest::Client) -> Self {
        Self {
            runtime,
            http_client,
            summaries_in_progress: Rc::new(RefCell::new(HashSet::new())),
        }
    }

    /// Validates inputs and starts a background summarization task.
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
        let request = {
            let state = state_rc.borrow();
            let video = prepare_summary_generation_video(
                &state,
                &mut self.summaries_in_progress.borrow_mut(),
                video_id,
                mode,
            )?;
            SummarizeRequest {
                video_url: video.watch_url(),
                video_title: video.title().to_string(),
                channel_name: video.channel_name().to_string(),
                video_id: video.video_id().to_string(),
                transcripts_work_dir: state.storage.transcripts_work_dir().to_path_buf(),
                local_subtitle_path: state.storage.find_subtitle_path(video_id),
            }
        };

        let task_video_id = request.video_id.clone();
        let (tx, result_rx) = async_channel::bounded::<Result<SummaryOutcome, String>>(1);
        let http_client = self.http_client.clone();
        self.runtime.spawn(async move {
            let result = summarize_video(&http_client, &request)
                .await
                .map_err(|summary_error| summary_error.to_string());
            let _ = tx.send(result).await;
        });

        Ok(SummaryGenerationTask {
            video_id: task_video_id,
            result_rx,
        })
    }

    /// Removes a video's in-progress marker after a completed or failed generation.
    pub(super) fn clear_in_progress(&self, video_id: &str) {
        self.summaries_in_progress.borrow_mut().remove(video_id);
    }

    /// Persists a summary, clears the in-progress marker, and refreshes the UI badge.
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
        self.clear_in_progress(video_id);
        state_rc
            .borrow_mut()
            .cache_video_ai_summary(video_id, summary)?;
        let has_summary = state_rc
            .borrow()
            .video_by_id(video_id)
            .is_some_and(Video::has_ai_summary);
        refresh_video_summary_badges(ui_context, video_id, has_summary);
        Ok(())
    }

    /// Clears the in-progress marker and caches a fallback transcript (never as a summary).
    fn cache_transcript_fallback(
        &self,
        state_rc: &Rc<RefCell<AppState>>,
        video_id: &str,
        transcript: String,
    ) {
        self.clear_in_progress(video_id);
        if let Err(cache_error) = state_rc
            .borrow_mut()
            .cache_video_transcript(video_id, transcript)
        {
            error!("Failed to cache fallback transcript for {video_id}: {cache_error}");
        }
    }
}

/// In-flight summarization handle.
pub(super) struct SummaryGenerationTask {
    video_id: String,
    result_rx: async_channel::Receiver<Result<SummaryOutcome, String>>,
}

impl SummaryGenerationTask {
    /// Returns the video id associated with this task.
    pub(super) fn video_id(&self) -> &str {
        &self.video_id
    }

    /// Waits for the summarization result.
    ///
    /// # Errors
    ///
    /// Returns the provider failure description when summarization fails or the
    /// background task is dropped before producing a result.
    pub(super) async fn wait(self) -> Result<SummaryOutcome, String> {
        self.result_rx
            .recv()
            .await
            .unwrap_or_else(|_| Err("summary task ended without a result".to_string()))
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

    if matches!(mode, SummaryGenerationMode::Prefetch) && video.has_ai_summary() {
        return Err(StartSummaryGenerationError::AlreadyCached);
    }
    if summaries_in_progress.contains(video_id) {
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

    let state_rc = state_rc.clone();
    let ui_context = ui_context.clone();
    glib::MainContext::default().spawn_local(async move {
        let video_id = generation_task.video_id().to_string();
        match generation_task.wait().await {
            Ok(SummaryOutcome::Summary(summary)) => {
                if let Err(cache_error) = summary_generator.persist_and_refresh(
                    &state_rc,
                    &ui_context,
                    &video_id,
                    summary,
                ) {
                    error!("Failed to cache prefetched summary for {video_id}: {cache_error}");
                }
            }
            Ok(SummaryOutcome::TranscriptOnly(transcript)) => {
                info!("Prefetch for {video_id} produced only a transcript; not cached as summary");
                summary_generator.cache_transcript_fallback(&state_rc, &video_id, transcript);
            }
            Err(generation_error) => {
                summary_generator.clear_in_progress(&video_id);
                error!("Failed to prefetch summary for {video_id}: {generation_error}");
            }
        }
    });
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
            Err(start_error) => {
                buffer.set_text(&format!("Error: {start_error}"));
                regenerate_button.set_sensitive(true);
                error!("Cannot generate summary for {video_id}: {start_error}");
                return;
            }
        };

    let state_rc = state_rc.clone();
    let ui_context = ui_context.clone();
    let buffer = buffer.clone();
    let regenerate_button = regenerate_button.clone();
    glib::MainContext::default().spawn_local(async move {
        let video_id = generation_task.video_id().to_string();
        let generation_result = generation_task.wait().await;
        regenerate_button.set_sensitive(true);

        match generation_result {
            Ok(SummaryOutcome::Summary(summary)) => {
                buffer.set_text(&summary);
                if let Err(cache_error) = summary_generator.persist_and_refresh(
                    &state_rc,
                    &ui_context,
                    &video_id,
                    summary.clone(),
                ) {
                    buffer.set_text(&format!("Error: {cache_error}"));
                    error!("Failed to cache interactive summary for {video_id}: {cache_error}");
                }
            }
            Ok(SummaryOutcome::TranscriptOnly(transcript)) => {
                buffer.set_text(&format!("{TRANSCRIPT_FALLBACK_NOTICE}{transcript}"));
                summary_generator.cache_transcript_fallback(&state_rc, &video_id, transcript);
            }
            Err(generation_error) => {
                summary_generator.clear_in_progress(&video_id);
                buffer.set_text(&format!("Error: {generation_error}"));
            }
        }
    });
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

            regenerate_button_for_layout.set_halign(gtk::Align::End);
            regenerate_button_for_layout.set_hexpand(true);
            controls_row.append(&regenerate_button_for_layout);
            content_area.append(&controls_row);
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

    {
        let state_rc = state_rc.clone();
        let ui_context = ui_context.clone();
        regenerate_button.connect_clicked(move |button| {
            start_summary_generation_for_dialog(
                &state_rc,
                &ui_context,
                &video_id,
                &buffer,
                button,
                "Regenerating summary...",
            );
        });
    }
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
            // Transcripts are not hydrated into memory at startup; fall back
            // to the sidecar file before fetching a fresh one.
            video
                .transcript()
                .map(ToString::to_string)
                .or_else(|| state.storage.load_transcript(video_id)),
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

    let (work_dir, local_subtitle_path) = {
        let state = state_rc.borrow();
        (
            state.storage.transcripts_work_dir().to_path_buf(),
            state.storage.find_subtitle_path(video_id),
        )
    };

    let (tx, rx) = async_channel::bounded::<Result<String, String>>(1);

    let video_id_for_thread = video_id.to_string();
    let runtime = ui_context.runtime.clone();
    runtime.spawn(async move {
        // Prefer a subtitle file downloaded alongside the video over another
        // rate-limited yt-dlp subtitle request.
        if let Some(subtitle_path) = local_subtitle_path.as_deref()
            && let Some(transcript) = transcript_from_vtt_file(subtitle_path).await
        {
            let _ = tx.send(Ok(transcript)).await;
            return;
        }
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
        StartSummaryGenerationError, SummaryGenerationMode, SummaryGenerationTask,
        prepare_summary_generation_video,
    };
    use crate::cache::Storage;
    use crate::data::{NewVideo, Video};
    use crate::summary::SummaryOutcome;
    use crate::ui::app::AppState;

    use chrono::Utc;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct TestDirs {
        root: TempDir,
    }

    impl TestDirs {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("test directory must be creatable");
            Self { root }
        }

        fn data_dir(&self) -> PathBuf {
            self.root.path().join("data")
        }

        fn cache_dir(&self) -> PathBuf {
            self.root.path().join("cache")
        }
    }

    fn test_video(video_id: &str) -> Video {
        Video::new(NewVideo {
            video_id: video_id.to_string(),
            channel_id: "channel-id".to_string(),
            channel_name: "Channel".to_string(),
            title: "Video Title".to_string(),
            published: Utc::now(),
            thumbnail_url: "https://example.com/thumb.jpg".to_string(),
            duration_seconds: None,
        })
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
        let mut summaries_in_progress = HashSet::new();

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

    #[test]
    fn prepare_request_refuses_concurrent_interactive_generation() {
        let video_id = "video-id";
        let (state, _dirs) = test_state(vec![test_video(video_id)]);
        let mut summaries_in_progress = HashSet::from([video_id.to_string()]);

        let result = prepare_summary_generation_video(
            &state,
            &mut summaries_in_progress,
            video_id,
            SummaryGenerationMode::Interactive,
        );

        assert!(matches!(
            result,
            Err(StartSummaryGenerationError::AlreadyInProgress)
        ));
    }

    #[tokio::test]
    async fn wait_returns_summary_outcome() {
        let (tx, rx) = async_channel::bounded(1);
        tx.send(Ok(SummaryOutcome::Summary("hello world".to_string())))
            .await
            .expect("result send should succeed");

        let outcome = SummaryGenerationTask {
            video_id: "video-id".to_string(),
            result_rx: rx,
        }
        .wait()
        .await
        .expect("summary should be produced");

        assert_eq!(outcome, SummaryOutcome::Summary("hello world".to_string()));
    }

    #[tokio::test]
    async fn wait_returns_stream_error() {
        let (tx, rx) = async_channel::bounded(1);
        tx.send(Err("provider failed".to_string()))
            .await
            .expect("error send should succeed");

        let result = SummaryGenerationTask {
            video_id: "video-id".to_string(),
            result_rx: rx,
        }
        .wait()
        .await;

        assert_eq!(result, Err("provider failed".to_string()));
    }

    #[tokio::test]
    async fn wait_reports_dropped_task() {
        let (tx, rx) = async_channel::bounded::<Result<SummaryOutcome, String>>(1);
        drop(tx);

        let result = SummaryGenerationTask {
            video_id: "video-id".to_string(),
            result_rx: rx,
        }
        .wait()
        .await;

        assert!(result.is_err());
    }
}
