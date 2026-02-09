mod app;
mod cache;
mod data;
mod feed;
mod gemini;
mod player;
mod ui;
mod urls;

use app::App;
use std::path::PathBuf;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // Find the subs file - check current dir, then parent dir (for running from yt-tui/)
    let subs_file = find_subs_file()?;

    let mut app = App::new(subs_file)?;
    app.run().await?;

    Ok(())
}

fn find_subs_file() -> anyhow::Result<PathBuf> {
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().ok_or_else(|| {
        anyhow::anyhow!("Could not determine executable directory: {}", exe_path.display())
    })?;

    let candidates = vec![
        exe_dir.join("youtube-subs.txt"),
        exe_dir.join("../youtube-subs.txt"),
        exe_dir.join("../../youtube-subs.txt"),
        exe_dir.join("../../../youtube-subs.txt"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }

    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    anyhow::bail!(
        "Could not find youtube-subs.txt relative to executable {}. Searched: {}",
        exe_path.display(),
        searched
    )
}
