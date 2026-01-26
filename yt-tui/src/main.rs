mod app;
mod cache;
mod data;
mod feed;
mod player;
mod ui;

use app::App;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Find the subs file - check current dir, then parent dir (for running from yt-tui/)
    let subs_file = find_subs_file()?;

    let mut app = App::new(subs_file)?;
    app.run().await?;

    Ok(())
}

fn find_subs_file() -> anyhow::Result<PathBuf> {
    // Try current directory
    let current = PathBuf::from("youtube-subs.txt");
    if current.exists() {
        return Ok(current);
    }

    // Try parent directory (when running from yt-tui subdirectory)
    let parent = PathBuf::from("../youtube-subs.txt");
    if parent.exists() {
        return Ok(parent);
    }

    // Try alongside the executable
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let beside_exe = exe_dir.join("youtube-subs.txt");
            if beside_exe.exists() {
                return Ok(beside_exe);
            }

            // Check parent of exe dir
            if let Some(parent_dir) = exe_dir.parent() {
                let in_parent = parent_dir.join("youtube-subs.txt");
                if in_parent.exists() {
                    return Ok(in_parent);
                }
            }
        }
    }

    anyhow::bail!(
        "Could not find youtube-subs.txt. Please ensure it exists in the current directory or parent directory."
    )
}
