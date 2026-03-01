# Structural Improvements

- [ ] Replace `Vec<Video>` + `HashMap<String, usize>` with `IndexMap<String, Video>`
`AppState` maintains `videos: Vec<Video>` and `video_index_by_id: HashMap<String, usize>` in
sync via `sync_video_index_by_id` (mod.rs:38–58). This dual-structure is the root cause of
`set_videos`, `video_by_id`, `video_by_id_mut`, and index mutation all needing coordination.
Replace with `indexmap::IndexMap<String, Video>` keyed by video ID — preserves insertion order,
gives O(1) lookup by ID, eliminates the sync function and the index HashMap entirely.

- [ ] Extract `http_client` and `subs_file` out of `AppState`
These are startup configuration, not mutable application state. `AppState` (mod.rs:28–36) mixes
infrastructure (`http_client`, `subs_file`) with live state (`videos`, `watch_later`,
`summaries_in_progress`). Pass `http_client` and `subs_file` as parameters at the call sites that
need them rather than routing through `AppState`.

- [ ] Unify context menu closure boilerplate in `cards.rs`
Five blocks (lines 82–141) each clone `selected_video`, `state_rc`, `ui_context`, read
`selected_video.borrow().clone()`, and call `context_menu.popdown()`. Extract a helper:
```rust
fn on_menu_action(button: &Button, selected: Rc<RefCell<Option<String>>>, menu: Popover, f: impl Fn(String) + 'static)
```
so each handler is a one-liner.

- [ ] Deduplicate `cache_video_transcript` / `cache_video_ai_summary`
`AppState::cache_video_transcript` and `cache_video_ai_summary` (mod.rs:97–129) are structurally
identical: persist → log error → update in-memory field. Extract a single generic method that
takes a storage closure and an `FnOnce(&mut Video)` mutator.

- [ ] Unify Gemini / OpenRouter HTTP response handling
`call_gemini_streaming` and `call_openrouter_with_transcript` (gemini.rs:286–310, 362–388) share
the same post-request pattern: check status, read body, parse JSON, extract text, send to channel.
The divergence is only in the request shape and JSON extraction. A shared `check_http_response`
helper for the status+body step would remove the duplication.

- [ ] Clean up `populate_flow_box` variable cloning (cards.rs:188–230)
The loop body creates `wl_state_rc`, `wl_ui_context`, `wl_ref`, `summary_state_rc`,
`summary_ui_context`, `summary_video_id` — all clones of variables already in scope. Using
`glib::clone!` consistently (as done for the card button-press handler) would eliminate the manual
clone pyramid and make ownership intent explicit.

- [ ] Move `refresh_video_lists` / `refresh_watch_later_tab` tab argument
`refresh_video_lists` calls `populate_flow_box` for both tabs unconditionally (refresh.rs).
`refresh_watch_later_tab` is a specialised version for just one tab, duplicating the call
structure. Unify into a single `refresh_tab(tab: Tab, ...)` with `refresh_video_lists` calling it
twice, removing the near-duplicate function.

- [ ] Replace `bool` return from `cache_video_*` with `Result`
`AppState::cache_video_transcript` and `cache_video_ai_summary` return `bool` (mod.rs:97, 114).
Callers can't distinguish a storage failure from a missing video ID. Return `Result<()>` and let
the caller decide how to surface the error.
