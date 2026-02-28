mod cache;
mod data;
mod feed;
mod gemini;
mod player;
mod ui;
mod urls;

use gtk::prelude::*;
use gtk::Application;
use std::path::{Path, PathBuf};
use std::{fs, io::Write};

fn append_startup_log(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", message);
    }
}

fn log_startup_marker() {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown-exe>".to_string());
    let msg = format!("yt-gtk startup exe={}", exe);

    append_startup_log(Path::new("/tmp/yt-gtk-chapters.log"), &msg);
    if let Ok(home) = std::env::var("HOME") {
        append_startup_log(
            Path::new(&home)
                .join(".cache/yt-gtk/yt-gtk-chapters.log")
                .as_path(),
            &msg,
        );
    }
    if let Ok(cwd) = std::env::current_dir() {
        append_startup_log(cwd.join("yt-gtk-chapters.log").as_path(), &msg);
    }
}

fn main() {
    log_startup_marker();

    // Set program name for desktop environment integration
    glib::set_prgname(Some("yt-gtk"));
    glib::set_application_name("yt-gtk");

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
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Could not determine executable directory: {}",
            exe_path.display()
        )
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
