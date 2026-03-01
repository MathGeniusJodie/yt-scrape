# Structural Improvements

- [ ] **Extract `SummaryGenerator` service** (`src/ui/app/summary.rs`) — `start_summary_generation_for_dialog()` does request validation, streaming, cache writes, state mutations, and UI buffer updates. Business logic belongs in a service, not the UI layer.

## Medium Priority

- [ ] **Deduplicate streaming message loops** (`src/ui/app/summary.rs:137-184`, `237-253`) — `start_summary_generation_for_dialog()` and `maybe_prefetch_summary_for_watch_later()` contain near-identical `while let Ok(message) = result_rx.recv().await` match loops. Extract into a shared function.

- [ ] **Split `summary.rs`** (469 lines) — Handles summary generation, transcript fetching, dialog creation for both, and persistence. Split into a service layer (`summary_service.rs`) and dialog layer (`summary_dialog.rs`, `transcript_dialog.rs`).

- [ ] **Remove `SummaryGenerationRequest`** (`src/ui/app/summary.rs:17-33`) — Just re-bundles fields from `Video`. Pass the video directly.

- [ ] **Extract `FeedRefreshProgressTracker`** (`src/ui/app/mod.rs:263-341`) — `spawn_refresh_progress_updates()` is a 70+ line inline state machine tracking total/completed/failed channels. Move into a struct to clarify state transitions and enable testing.

## Lower Priority

- [ ] **Deduplicate tab button connection code** (`src/ui/app/mod.rs:556-589`) — Same `clone()` block and `connect_toggled` pattern repeated for Feed/Watch-Later/Downloading tabs. Extract a helper function.

- [ ] **Incremental video index updates** (`src/ui/app/mod.rs:61-72`) — `rebuild_video_index()` rebuilds the full `HashMap` on every `set_videos()` call. Use the `Entry` API to update only changed videos.

- [ ] **Document threading model** — Code alternates between `runtime.spawn()` and `glib::MainContext::default().spawn_local()` with no explicit contract. Add a short architecture note explaining what runs where.
