mod downloader;
mod storage;
mod subtitle_requests;
mod transcript;

pub use downloader::download_video;
pub use storage::Storage;
pub use transcript::{fetch_transcript, transcript_from_vtt_file};

/// Checks whether `program` is on `PATH`.
pub fn is_on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path_var| {
        std::env::split_paths(&path_var).any(|dir| dir.join(program).is_file())
    })
}

/// Spawn `program` under `nice -n 19` so encoding/download processes don't starve the UI.
///
/// Falls back to running `program` directly when `nice` is not on `PATH`, so a
/// missing `nice` degrades priority instead of breaking downloads entirely.
pub fn nice_command(program: &str) -> tokio::process::Command {
    static NICE_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let mut cmd = if *NICE_AVAILABLE.get_or_init(|| is_on_path("nice")) {
        let mut cmd = tokio::process::Command::new("nice");
        cmd.arg("-n").arg("19").arg(program);
        cmd
    } else {
        tokio::process::Command::new(program)
    };
    // Timed-out/cancelled callers drop the output future; without this the
    // orphaned yt-dlp would keep downloading (and writing files) forever.
    cmd.kill_on_drop(true);
    cmd
}

/// Browser to pass to `yt-dlp --cookies-from-browser`.
///
/// Reads `YT_DLP_COOKIES_BROWSER` from the environment, defaulting to `"chromium"`.
pub fn cookies_browser() -> String {
    std::env::var("YT_DLP_COOKIES_BROWSER").unwrap_or_else(|_| "chromium".to_string())
}
