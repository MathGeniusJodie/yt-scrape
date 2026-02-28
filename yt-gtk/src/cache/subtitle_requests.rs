use log::{debug, warn};
use std::process::Output;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::sleep;

const MAX_429_RETRIES: usize = 3;
const MIN_SUBTITLE_REQUEST_GAP: Duration = Duration::from_millis(1_500);
// Exponential back-off: 2s → 4s → 8s across the three retries.
const BASE_429_BACKOFF: Duration = Duration::from_millis(2_000);

/// A pacing mechanism that serializes subtitle requests and enforces a minimum
/// gap between them to avoid triggering YouTube rate limits.
///
/// Use [`SubtitleRateLimiter::global`] for production code or
/// [`SubtitleRateLimiter::new`] with a custom gap for testing.
#[derive(Debug)]
pub(crate) struct SubtitleRateLimiter {
    last_request_at: Mutex<Instant>,
    min_gap: Duration,
}

impl SubtitleRateLimiter {
    /// Create a new rate limiter with the given minimum gap between requests.
    ///
    /// The internal timestamp is initialized `min_gap` in the past so the first
    /// call to [`Self::wait_for_slot`] is never delayed.
    pub(crate) fn new(min_gap: Duration) -> Self {
        let now = Instant::now();
        Self {
            last_request_at: Mutex::new(now.checked_sub(min_gap).unwrap_or(now)),
            min_gap,
        }
    }

    /// Return the process-global rate limiter used for all production subtitle fetches.
    pub(crate) fn global() -> &'static Self {
        static INSTANCE: OnceLock<SubtitleRateLimiter> = OnceLock::new();
        INSTANCE.get_or_init(|| Self::new(MIN_SUBTITLE_REQUEST_GAP))
    }

    /// Block until `self.min_gap` has elapsed since the last request, then update
    /// the timestamp. The mutex is held across the sleep to serialize callers and
    /// prevent stampedes. Requires `tokio::sync::Mutex` — holding `std::sync::Mutex`
    /// across `.await` would deadlock or panic.
    async fn wait_for_slot(&self) {
        let mut last = self.last_request_at.lock().await;
        let elapsed = last.elapsed();
        if elapsed < self.min_gap {
            let wait = self.min_gap - elapsed;
            debug!("Subtitle slot throttled; waiting {wait:?} before next request.");
            sleep(wait).await;
        }
        *last = Instant::now();
    }
}

/// Run a subtitle-focused `yt-dlp` command with shared pacing and 429 retries.
///
/// Calls [`SubtitleRateLimiter::wait_for_slot`] before each attempt. On HTTP 429,
/// retries up to [`MAX_429_RETRIES`] times with exponential back-off.
///
/// # Arguments
///
/// * `limiter` - Shared pacing/rate-limit state for all subtitle requests.
/// * `video_id` - Video id used for log messages.
/// * `build_command` - Closure that builds a fresh `yt-dlp` subtitle command per attempt.
///
/// # Errors
///
/// Returns `std::io::Error` when spawning or awaiting the command fails.
pub(crate) async fn run_yt_dlp_subtitle_command(
    limiter: &SubtitleRateLimiter,
    video_id: &str,
    mut build_command: impl FnMut() -> Command,
) -> std::io::Result<Output> {
    let mut retry_count = 0usize;
    loop {
        limiter.wait_for_slot().await;
        let output = build_command().output().await?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() || !is_rate_limited(&stderr) || retry_count >= MAX_429_RETRIES {
            return Ok(output);
        }
        retry_count += 1;
        // 2^(retry_count-1) doubling, capped at exponent 3 (i.e. 16 s max).
        let delay = BASE_429_BACKOFF * (1u32 << (retry_count - 1).min(3));
        warn!(
            "Subtitle request for {video_id} was rate-limited \
             (retry {retry_count}/{MAX_429_RETRIES}). Waiting {delay:?} before retry."
        );
        sleep(delay).await;
    }
}

/// Return `true` if `stderr` indicates an HTTP 429 / rate-limit error.
fn is_rate_limited(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("http error 429")
        || lower.contains("too many requests")
        || lower.contains("rate limit")
        || lower.contains("rate-limited")
}

#[cfg(test)]
mod tests {
    use super::{is_rate_limited, run_yt_dlp_subtitle_command, SubtitleRateLimiter, MAX_429_RETRIES};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use tokio::process::Command;

    // ── is_rate_limited ──────────────────────────────────────────────────────

    #[test]
    fn is_rate_limited_detects_http_429() {
        assert!(is_rate_limited(
            "ERROR: Unable to download subtitles: HTTP Error 429"
        ));
    }

    #[test]
    fn is_rate_limited_detects_too_many_requests() {
        assert!(is_rate_limited("Too Many Requests"));
    }

    #[test]
    fn is_rate_limited_detects_rate_limit_two_words() {
        assert!(is_rate_limited("Exceeded rate limit for this endpoint"));
    }

    #[test]
    fn is_rate_limited_detects_rate_limited_hyphenated() {
        assert!(is_rate_limited("You have been rate-limited by the server"));
    }

    #[test]
    fn is_rate_limited_ignores_unrelated_errors() {
        assert!(!is_rate_limited("ERROR: Sign in to confirm your age"));
        assert!(!is_rate_limited("ERROR: Video unavailable"));
    }

    // ── SubtitleRateLimiter::wait_for_slot ───────────────────────────────────

    #[tokio::test]
    async fn wait_for_slot_does_not_delay_first_request() {
        let limiter = SubtitleRateLimiter::new(Duration::from_millis(100));
        let before = Instant::now();
        limiter.wait_for_slot().await;
        // The initial timestamp is `min_gap` in the past, so no sleep occurs.
        assert!(before.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn wait_for_slot_paces_consecutive_requests() {
        let gap = Duration::from_millis(50);
        let limiter = SubtitleRateLimiter::new(gap);
        limiter.wait_for_slot().await; // consume the free first slot
        let before = Instant::now();
        limiter.wait_for_slot().await; // should block for ~gap
        assert!(
            before.elapsed() >= gap,
            "second slot should be delayed by at least {gap:?}"
        );
    }

    // ── run_yt_dlp_subtitle_command ──────────────────────────────────────────
    // These tests use `start_paused = true` so that tokio::time::sleep calls
    // complete instantly via auto-advance.

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_command_returns_immediately_on_success() {
        let limiter = SubtitleRateLimiter::new(Duration::ZERO);
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);

        let result = run_yt_dlp_subtitle_command(&limiter, "vid1", move || {
            cc.fetch_add(1, Ordering::Relaxed);
            Command::new("true")
        })
        .await
        .expect("io should not fail");

        assert!(result.status.success());
        assert_eq!(call_count.load(Ordering::Relaxed), 1, "should not retry on success");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_command_returns_immediately_on_non_429_failure() {
        let limiter = SubtitleRateLimiter::new(Duration::ZERO);
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);

        // `false` exits with code 1 and empty stderr — not a 429.
        let result = run_yt_dlp_subtitle_command(&limiter, "vid2", move || {
            cc.fetch_add(1, Ordering::Relaxed);
            Command::new("false")
        })
        .await
        .expect("io should not fail");

        assert!(!result.status.success());
        assert_eq!(call_count.load(Ordering::Relaxed), 1, "non-429 failure should not retry");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_command_retries_on_429_then_succeeds() {
        let limiter = SubtitleRateLimiter::new(Duration::ZERO);
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);

        let result = run_yt_dlp_subtitle_command(&limiter, "vid3", move || {
            let n = cc.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                // First attempt: simulate a 429 response.
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg("printf 'HTTP Error 429' >&2; exit 1");
                cmd
            } else {
                Command::new("true")
            }
        })
        .await
        .expect("io should not fail");

        assert!(result.status.success());
        assert_eq!(call_count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn run_command_gives_up_after_max_retries() {
        let limiter = SubtitleRateLimiter::new(Duration::ZERO);
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);

        // Always return a 429 — the function should give up after MAX_429_RETRIES.
        let result = run_yt_dlp_subtitle_command(&limiter, "vid4", move || {
            cc.fetch_add(1, Ordering::Relaxed);
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg("printf 'HTTP Error 429' >&2; exit 1");
            cmd
        })
        .await
        .expect("io should not fail");

        assert!(!result.status.success());
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            MAX_429_RETRIES + 1,
            "should attempt exactly MAX_429_RETRIES + 1 times total"
        );
    }
}
