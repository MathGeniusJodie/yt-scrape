mod cache;
mod data;
mod feed;
mod gemini;
mod player;
mod ui;
mod urls;

use gtk::prelude::*;
use gtk::Application;
use std::path::PathBuf;

fn main() {
    // Find the subs file
    let subs_file = match find_subs_file() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let app = Application::builder()
        .application_id("com.github.yt-gtk")
        .build();

    app.connect_activate(move |app| {
        ui::build_ui(app, subs_file.clone());
    });

    app.run();
}

fn find_subs_file() -> anyhow::Result<PathBuf> {
    // Try current directory
    let current = PathBuf::from("youtube-subs.txt");
    if current.exists() {
        return Ok(current);
    }

    // Try parent directory (when running from yt-gtk subdirectory)
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
