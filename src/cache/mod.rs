mod downloader;
mod storage;
mod subtitle_requests;
mod transcript;

pub use downloader::{convert_to_miyoo, download_video};
pub use storage::{Storage, StorageError};
pub use transcript::fetch_transcript;

/// Spawn `program` under `nice -n 19` so encoding/download processes don't starve the UI.
pub(super) fn nice_command(program: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("nice");
    cmd.arg("-n").arg("19").arg(program);
    cmd
}
