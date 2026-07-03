mod cache;
mod data;
mod feed;
mod frogpoints;
mod player;
mod summary;
mod ui;
mod urls;

use adw::prelude::*;
use cache::is_on_path;
use gtk::glib;
use log::{error, warn};
use std::path::PathBuf;

/// External binaries the app shells out to, paired with what degrades if missing.
const EXTERNAL_DEPENDENCIES: [(&str, &str); 4] = [
    (
        "yt-dlp",
        "video downloads and transcript extraction will fail",
    ),
    ("mpv", "video playback will fail"),
    ("pgrep", "frogpoints leisure detection will be unavailable"),
    (
        "nice",
        "downloads/encodes will run at normal priority instead of nice -n 19",
    ),
];

fn main() {
    // Info by default; override with RUST_LOG (e.g. RUST_LOG=debug for mpv output).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    if let Err(error) = run() {
        error!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    warn_about_missing_dependencies();

    glib::set_prgname(Some("yt-gtk"));
    glib::set_application_name("yt-gtk");

    let subs_file = find_subs_file()?;

    let app = adw::Application::builder()
        .application_id("com.github.yt-gtk")
        .build();

    app.connect_activate(move |app| {
        // Re-activation of a running single-instance app must present the
        // existing window, not build a second UI with duplicated state.
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        ui::build_ui(app, subs_file.clone());
    });

    app.run();
    Ok(())
}

/// Locates `youtube-subs.txt`, preferring the XDG config directory, then walking up
/// from the executable's directory (covering cargo layouts where the repo root holds
/// the subs file: target/debug is two levels down, target/<triple>/debug three).
///
/// The subs file is optional: subscriptions are only one of several data sources (search
/// and watch-later work without them), so a missing file is not fatal here. When no
/// candidate exists, this logs a warning and falls back to the XDG config candidate path
/// (letting the refresh flow surface a readable error later) rather than bailing. Only
/// when XDG config is itself unavailable *and* nothing was found does this return an error.
fn find_subs_file() -> anyhow::Result<PathBuf> {
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Could not determine executable directory: {}",
            exe_path.display()
        )
    })?;

    let xdg_config_candidate = directories::ProjectDirs::from("", "", "yt-gtk")
        .map(|dirs| dirs.config_dir().join("youtube-subs.txt"));

    let candidates = xdg_config_candidate
        .clone()
        .into_iter()
        .chain(
            exe_dir
                .ancestors()
                .take(4)
                .map(|dir| dir.join("youtube-subs.txt")),
        )
        .collect::<Vec<_>>();

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

    match xdg_config_candidate {
        Some(fallback) => {
            warn!(
                "Could not find youtube-subs.txt relative to executable {}. Searched: {}. \
                 Continuing without subscriptions; refresh will report a readable error.",
                exe_path.display(),
                searched
            );
            Ok(fallback)
        }
        None => anyhow::bail!(
            "Could not find youtube-subs.txt relative to executable {}. Searched: {}",
            exe_path.display(),
            searched
        ),
    }
}

/// Logs a warning for each external dependency in [`EXTERNAL_DEPENDENCIES`] that is missing
/// from `PATH`, explaining which feature degrades without it.
fn warn_about_missing_dependencies() {
    for (program, degraded_feature) in EXTERNAL_DEPENDENCIES {
        if !is_on_path(program) {
            warn!("`{program}` not found on PATH: {degraded_feature}.");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cache::is_on_path;

    #[test]
    fn is_on_path_finds_a_binary_known_to_exist() {
        // `sh` is a POSIX baseline requirement, so it must be present in any test environment.
        assert!(is_on_path("sh"));
    }

    #[test]
    fn is_on_path_rejects_a_nonexistent_binary() {
        assert!(!is_on_path("definitely-not-a-real-binary-xyz123"));
    }
}
