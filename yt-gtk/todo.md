- [ ] **Pass shared `reqwest::Client` into `fetch_all_feeds`** — `feed/fetcher.rs:178` creates its own client (30s timeout, gzip) instead of accepting the one from `build_ui` (120s timeout). Two clients with different configs exist simultaneously. Accept a `&reqwest::Client` parameter and remove the internal construction.

- [ ] **Avoid full FlowBox rebuild on every state change** — `refresh_video_lists` (`refresh.rs:28`) clears and repopulates both FlowBoxes (up to 400 cards each) on every watch-later toggle, thumbnail completion, or summary update. Replace full rebuilds with targeted updates: update only the affected card's badge/toggle state rather than recreating all widgets.

- [ ] **Split `UiContext` into async infra and widget state** — `UiContext` bundles `runtime`, `http_client`, two FlowBoxes, `selected_video`, badge, button map, context menu, and window. It is cloned and passed into every function. The async fields (`runtime`, `http_client`) and the GTK widget references are orthogonal concerns and should be separate structs.

- [ ] **Collapse `SummaryGenerator` or make it part of `UiContext`** — `SummaryGenerator::new(runtime, http_client)` is reconstructed on every call in `summary.rs:21` and `summary.rs:88`. It wraps the exact same values already in `UiContext`. Store one instance in `UiContext` or eliminate the wrapper entirely and call the generation functions directly.

- [ ] **Move `summaries_in_progress` ownership to `SummaryGenerator`** — `AppState.summaries_in_progress` (`mod.rs:33`) is managed entirely by `SummaryGenerator` but lives in `AppState`. This splits the concern across two types. Either make `SummaryGenerator` stateful with an `Arc<Mutex<HashSet<String>>>`, or dissolve the generator abstraction and keep everything in `AppState`.

- [ ] **Remove `cache_video_sidecar` generic abstraction** — `AppState::cache_video_sidecar` (`mod.rs:80`) takes two generic function parameters to share ~3 lines between transcript and summary caching. The abstraction is harder to read than the duplication it avoids. Replace with two direct, obvious methods.

- [ ] **Inline `index_videos_by_id`** — The three-line function (`mod.rs:50`) is only called from `AppState::new` and `AppState::set_videos`. Inline it as `.into_iter().map(|v| (v.video_id().to_string(), v)).collect()` at both call sites.

- [ ] **Move `create_readonly_text_scroller` to the UI layer** — This widget factory (`mod.rs:257`) lives in the app orchestration module but is purely a UI construction helper. It belongs in `ui/dialogs.rs` or a new `ui/widgets.rs`.