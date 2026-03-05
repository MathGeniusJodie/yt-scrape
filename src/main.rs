mod cache;
mod data;
mod feed;
mod gemini;
mod player;
mod ui;
mod urls;

use anyhow::Context;
use gtk::prelude::*;
use gtk::Application;
use log::{error, Level, LevelFilter, Metadata, Record};
use std::io::Write;
use std::path::PathBuf;

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(std::io::stderr(), "[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

static STDERR_LOGGER: StderrLogger = StderrLogger;

fn init_logger() {
    if log::set_logger(&STDERR_LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

fn main() {
    init_logger();

    if let Err(error) = run() {
        error!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    glib::set_prgname(Some("yt-gtk"));
    glib::set_application_name("yt-gtk");

    let subs_file = find_subs_file().context("Failed locating youtube-subs.txt")?;

    let app = Application::builder()
        .application_id("com.github.yt-gtk")
        .build();

    app.connect_activate(move |app| {
        ui::build_ui(app, subs_file.clone());
    });

    app.run();
    Ok(())
}

fn find_subs_file() -> anyhow::Result<PathBuf> {
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Could not determine executable directory: {}",
            exe_path.display()
        )
    })?;

    let candidates = [
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
