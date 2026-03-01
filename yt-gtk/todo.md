# Structural Improvements

## State & Data Flow

- [ ] **Dowload and cache AI summary on add to watch later**

- [ ] **Single source of truth for video state** — Videos are duplicated across `AppState.videos`, closures captured in card widgets, and both `feed_cards`/`watch_later_cards` maps. Normalize so cards hold only a video ID and read from `AppState` on demand.
- [ ] **Separate data from infrastructure in `AppState`** — `AppState` holds both the video collection (`IndexMap<String, Video>`) and the `Storage` handle. Extract storage into the call sites that need it so `AppState` is a pure data struct.
- [ ] **Flatten `UiContext` → `AsyncContext` / `WidgetContext` nesting** — The triple-layer context wrapping forces callers to write `ui_ctx.async_ctx.runtime` and `ui_ctx.widgets.feed_flow`. Merge into a single flat `AppContext` struct.

## Async & Blocking I/O

- [ ] **Move storage writes off the UI thread** — `save_watch_later()` and `save_videos()` are synchronous and called from GTK callbacks, blocking the event loop. Offload via `tokio::task::spawn_blocking` or make them async.
- [ ] **Add cancellation to long-running tasks** — Summary generation and video downloads have no cancellation mechanism; closing a dialog leaks the task. Use `tokio_util::sync::CancellationToken` or a `oneshot` drop guard.

## Code Duplication

- [ ] **Extract `create_text_dialog()` factory** — `show_summary_dialog()` and `show_transcript_dialog()` share identical boilerplate (dialog creation, scroll area, text buffer, close button, `show_all`). Extract into one function returning `(Dialog, TextBuffer)`.
- [ ] **Extract `for_each_card_matching(video_id, |card| …)` helper** — The loop over `feed_cards` and `watch_later_cards` to find and update a card by ID is repeated 3+ times across `cards.rs` and `refresh.rs`. Pull it out once.
- [ ] **Unify `GeminiResponse` / `OpenRouterResponse` parsing** — Both types share an `error` field and near-identical content extraction logic. Define a `ProviderResponse` trait (or a single enum) so `extract_content()` is written once.

## Long Functions

- [ ] **Split `build_ui()`** (~216 lines) — Responsibilities: window + header creation, tab setup, state initialization, HTTP client setup, event handler wiring, async context spawn. Each should be its own function.
- [ ] **Split `create_video_card()`** (~167 lines) — Inline Cairo drawing and widget tree construction in one function. Extract `create_thumbnail_widget()`, `create_metadata_box()`, `create_action_buttons()`.
- [ ] **Split `fetch_channel_with_retries()`** (~105 lines in `app/mod.rs`) — The retry loop, 404-recovery path, and `PendingChannel` state transitions are three distinct concerns; split into focused helpers.

## Error Handling

- [ ] **Standardise on `thiserror` in library modules, `anyhow` in binaries** — Currently mixed: `main.rs` and `feed/fetcher.rs` use `anyhow`; `cache/` and `player/` use `thiserror`. Define the boundary and apply it consistently.
- [ ] **Surface async task failures to the user** — Download and summary failures are only logged (`error!(...)`). Show a brief in-app notification or status label so the user knows when something went wrong.
