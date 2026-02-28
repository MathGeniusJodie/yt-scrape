use log::warn;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::process::Output;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::sleep;

const MAX_429_RETRIES: usize = 3;
const MIN_SUBTITLE_REQUEST_GAP: Duration = Duration::from_millis(1_500);
const BASE_429_BACKOFF_MS: u64 = 2_000;
const MAX_429_BACKOFF_MS: u64 = 16_000;
const MAX_JITTER_MS: u64 = 300;

static LAST_SUBTITLE_REQUEST_AT: OnceLock<Mutex<Instant>> = OnceLock::new();

/// Run a subtitle-focused `yt-dlp` command with shared pacing and 429 retries.
///
/// # Arguments
///
/// * `video_id` - Video id used for logging and deterministic jitter.
/// * `build_command` - Closure that builds a fresh `yt-dlp` subtitle command per attempt.
///
/// # Returns
///
/// Process output from the final attempt.
///
/// # Errors
///
/// Returns `std::io::Error` when spawning or awaiting the command fails.
pub(crate) async fn run_yt_dlp_subtitle_command(
    video_id: &str,
    mut build_command: impl FnMut() -> Command,
) -> std::io::Result<Output> {
    let mut retry_count = 0usize;

    loop {
        wait_for_subtitle_slot().await;

        let output = build_command().output().await?;
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() || !is_rate_limited(&stderr) || retry_count >= MAX_429_RETRIES {
            return Ok(output);
        }

        retry_count += 1;
        let delay = retry_delay_for(video_id, retry_count);
        warn!(
            "Subtitle request for {} was rate-limited (retry {}/{}). \
             Waiting {:?} before retry.",
            video_id, retry_count, MAX_429_RETRIES, delay
        );
        sleep(delay).await;
    }
}

async fn wait_for_subtitle_slot() {
    let limiter = LAST_SUBTITLE_REQUEST_AT.get_or_init(|| {
        let now = Instant::now();
        let initial = now.checked_sub(MIN_SUBTITLE_REQUEST_GAP).unwrap_or(now);
        Mutex::new(initial)
    });

    let mut last_request_at = limiter.lock().await;
    let elapsed = last_request_at.elapsed();
    if elapsed < MIN_SUBTITLE_REQUEST_GAP {
        // Hold the lock while sleeping so concurrent callers line up instead of stampeding.
        sleep(MIN_SUBTITLE_REQUEST_GAP - elapsed).await;
    }
    *last_request_at = Instant::now();
}

fn retry_delay_for(video_id: &str, retry_count: usize) -> Duration {
    let exponent = retry_count.saturating_sub(1).min(3) as u32;
    let exponential_backoff = BASE_429_BACKOFF_MS
        .saturating_mul(1u64 << exponent)
        .min(MAX_429_BACKOFF_MS);
    let jitter_ms = deterministic_jitter_ms(video_id, retry_count);
    Duration::from_millis(exponential_backoff + jitter_ms)
}

fn deterministic_jitter_ms(video_id: &str, retry_count: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    video_id.hash(&mut hasher);
    retry_count.hash(&mut hasher);
    hasher.finish() % (MAX_JITTER_MS + 1)
}

fn is_rate_limited(stderr: &str) -> bool {
    let stderr_lower = stderr.to_ascii_lowercase();
    stderr_lower.contains("http error 429")
        || stderr_lower.contains("too many requests")
        || stderr_lower.contains("rate limit")
        || stderr_lower.contains("rate-limited")
}

#[cfg(test)]
mod tests {
    use super::{is_rate_limited, retry_delay_for, MAX_429_BACKOFF_MS, MAX_JITTER_MS};
    use std::time::Duration;

    #[test]
    fn is_rate_limited_detects_429_patterns() {
        let stderr = "ERROR: Unable to download video subtitles for 'en': HTTP Error 429";
        assert!(is_rate_limited(stderr));
        assert!(is_rate_limited("Too Many Requests"));
    }

    #[test]
    fn is_rate_limited_ignores_other_failures() {
        assert!(!is_rate_limited("ERROR: Sign in to confirm your age"));
        assert!(!is_rate_limited("ERROR: Video unavailable"));
    }

    #[test]
    fn retry_delay_grows_then_caps() {
        let retry_1 = retry_delay_for("abc123", 1);
        let retry_2 = retry_delay_for("abc123", 2);
        let retry_3 = retry_delay_for("abc123", 3);
        let retry_9 = retry_delay_for("abc123", 9);

        assert!(retry_2 > retry_1);
        assert!(retry_3 > retry_2);
        assert!(retry_9 <= Duration::from_millis(MAX_429_BACKOFF_MS + MAX_JITTER_MS));
    }
}
