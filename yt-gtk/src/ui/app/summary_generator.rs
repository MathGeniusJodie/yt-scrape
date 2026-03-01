use super::{AppState, CacheVideoError};
use crate::data::Video;
use crate::gemini::{summarize_video_streaming, StreamingMessage};

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::runtime::Runtime;

fn spawn_summary_generation_stream(
    runtime: Arc<Runtime>,
    client: reqwest::Client,
    video: Video,
    transcripts_work_dir: std::path::PathBuf,
) -> async_channel::Receiver<StreamingMessage> {
    let (tx, rx) = async_channel::unbounded::<StreamingMessage>();

    runtime.spawn(async move {
        let video_url = video.watch_url();
        summarize_video_streaming(
            client,
            video.video_id(),
            &video_url,
            video.title(),
            video.channel_name(),
            &transcripts_work_dir,
            tx,
        )
        .await;
    });

    rx
}

/// Configures request guards for starting summary generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SummaryGenerationMode {
    /// Skip generation if a summary exists or one is already running.
    Prefetch,
    /// Always start a new generation for explicit user actions.
    Interactive,
}

impl SummaryGenerationMode {
    const fn should_skip_cached(self) -> bool {
        matches!(self, Self::Prefetch)
    }

    const fn should_skip_in_progress(self) -> bool {
        matches!(self, Self::Prefetch)
    }
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
    summaries_in_progress: Arc<Mutex<HashSet<String>>>,
}

impl SummaryGenerator {
    /// Creates a generator backed by the application's async runtime.
    pub(super) fn new(runtime: Arc<Runtime>, http_client: reqwest::Client) -> Self {
        Self {
            runtime,
            http_client,
            summaries_in_progress: Arc::new(Mutex::new(HashSet::new())),
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
        let video = {
            let state = state_rc.borrow();
            let mut summaries_in_progress = self
                .summaries_in_progress
                .lock()
                .expect("summary in-progress mutex must not be poisoned");
            prepare_summary_generation_video(&state, &mut summaries_in_progress, video_id, mode)?
        };

        let transcripts_work_dir = {
            let state = state_rc.borrow();
            state.storage.transcripts_work_dir().to_path_buf()
        };

        let task_video_id = video.video_id().to_string();
        let result_rx = spawn_summary_generation_stream(
            self.runtime.clone(),
            self.http_client.clone(),
            video,
            transcripts_work_dir,
        );

        Ok(SummaryGenerationTask {
            video_id: task_video_id,
            result_rx,
        })
    }

    /// Removes a video's in-progress marker after a failed or cancelled generation.
    pub(super) fn clear_in_progress(&self, video_id: &str) {
        self.summaries_in_progress
            .lock()
            .expect("summary in-progress mutex must not be poisoned")
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
            .lock()
            .expect("summary in-progress mutex must not be poisoned")
            .remove(video_id);
        state_rc
            .borrow_mut()
            .cache_video_ai_summary(video_id, summary)
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

    if mode.should_skip_cached() && has_cached_summary {
        return Err(StartSummaryGenerationError::AlreadyCached);
    }
    if mode.should_skip_in_progress() && summaries_in_progress.contains(video_id) {
        return Err(StartSummaryGenerationError::AlreadyInProgress);
    }

    summaries_in_progress.insert(video_id.to_string());
    Ok(video)
}

#[cfg(test)]
mod tests {
    use super::{
        prepare_summary_generation_video, StartSummaryGenerationError, SummaryGenerationError,
        SummaryGenerationMode, SummaryGenerationTask,
    };
    use crate::cache::Storage;
    use crate::data::Video;
    use crate::gemini::StreamingMessage;
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
            "Video Title".to_string(),
            Utc::now(),
            "https://example.com/thumb.jpg".to_string(),
            None,
        )
    }

    fn test_state(videos: Vec<Video>) -> (AppState, TestDirs) {
        let dirs = TestDirs::new();
        let storage = Storage::new_at(dirs.data_dir(), dirs.cache_dir())
            .expect("test storage must initialize");
        let state = AppState::new(videos, HashSet::new(), storage);
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
