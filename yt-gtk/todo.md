- [ ] **Dowload and cache AI summary on add to watch later**

- [ ] **Single source of truth for video state** — Videos are duplicated across `AppState.videos`, closures captured in card widgets, and both `feed_cards`/`watch_later_cards` maps. Normalize so cards hold only a video ID and read from `AppState` on demand.

- [ ] **Flatten `UiContext` → `AsyncContext` / `WidgetContext` nesting** — The triple-layer context wrapping forces callers to write `ui_ctx.async_ctx.runtime` and `ui_ctx.widgets.feed_flow`. Merge into a single flat `AppContext` struct.

- [ ] **Move storage writes off the UI thread** — `save_watch_later()` and `save_videos()` are synchronous and called from GTK callbacks, blocking the event loop. Offload via `tokio::task::spawn_blocking` or make them async.

- [ ] **Extract `create_text_dialog()` factory** — `show_summary_dialog()` and `show_transcript_dialog()` share identical boilerplate (dialog creation, scroll area, text buffer, close button, `show_all`). Extract into one function returning `(Dialog, TextBuffer)`.

- [ ] **Extract `for_each_card_matching(video_id, |card| …)` helper** — The loop over `feed_cards` and `watch_later_cards` to find and update a card by ID is repeated 3+ times across `cards.rs` and `refresh.rs`. Pull it out once.

- [ ] **Unify `GeminiResponse` / `OpenRouterResponse` parsing** — Both types share an `error` field and near-identical content extraction logic. Define a `ProviderResponse` trait (or a single enum) so `extract_content()` is written once.

- [ ] **Standardise on `thiserror` in library modules, `anyhow` in binaries** — Currently mixed: `main.rs` and `feed/fetcher.rs` use `anyhow`; `cache/` and `player/` use `thiserror`. Define the boundary and apply it consistently.
