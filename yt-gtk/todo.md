# Code Review TODO

## `src/main.rs`

- [ ] `find_subs_file`: replace `vec![]` with a `[PathBuf; 4]` array to avoid heap allocation; the array can be iterated twice without a second collection
- [ ] Remove "what" comment on `glib::set_prgname` call (line 51) — code is self-documenting

## `src/feed/fetcher.rs`

- [ ] Extract magic number `400` (line 230) into `const MAX_FEED_VIDEOS: usize = 400`
- [ ] Remove dead `INITIAL_BACKOFF_MS == 0` guard in `backoff_ms_with_base` — the constant is `1_000` and can never be zero
- [ ] Rename `PlaylistThumbnails::best_url` → `preferred_url` (or add a comment) — it intentionally returns medium quality, not the best available
- [ ] Fix misleading `next_attempt` field in `RetryScheduled`: it sends `pending.attempt` (current) before the increment, so the value is off-by-one relative to the variant name. double check before fixing though.
- [ ] Replace `parse_iso8601_duration_seconds` with a library function, we should not be reinventing the wheel

## `src/cache/storage.rs`

- [ ] `sanitize_filename`: eliminate the second `String` allocation — `s` is already owned; use `s.truncate(s.trim_end().len()); s` instead of `.trim_end().to_string()`
- [ ] `find_video_path`: results in multiple `readdir` syscalls per UI action because it's called from both `needs_download_upgrade` and `resolve_playback_path` for the same video; consider combining the call sites
- [ ] `save_video_transcript` / `save_video_ai_summary` each read-then-write the sidecar independently; sequential calls do two reads and two writes where one of each would suffice
- [ ] Replace direct struct literal in test `create_test_storage` with a `Storage::new_at(data_dir, cache_dir)` constructor to decouple tests from private field layout

## `src/cache/transcript.rs`

- [ ] Rewrite `parse_json3` nested loops + intermediate `Vec<String>` as a single iterator chain to eliminate the intermediate allocation:
  ```rust
  let raw_text: String = events
      .into_iter()
      .flat_map(|e| e.segs.unwrap_or_default())
      .filter_map(|s| s.utf8)
      .collect();
  ```
- [ ] Remove `// Join all text and clean it up` comment in `parse_json3` — it describes what, not why

## `src/gemini.rs`

- [ ] API key is interpolated directly into the URL string — use `.query(&[("key", api_key)])` on the request builder to correctly handle any special characters
- [ ] SSE buffer: replace `buffer = buffer[event_end + 2..].to_string()` with `buffer.drain(..event_end + 2)` to avoid a new `String` allocation per event
- [ ] `summarize_video_streaming` creates a new `reqwest::Client` on every call, discarding the connection pool; the client should be shared (e.g. stored in `AppState` or passed in)
- [ ] `spawn_summary_generation` buffers all streaming chunks before returning — the streaming API is entirely wasted; either propagate chunks incrementally to the UI or rename to make the buffering explicit
- [ ] Remove the `smartify_quotes` logic
- [ ] Add unit tests for pure functions: `extract_openrouter_content`, SSE event parsing logic

## `src/player/chapters.rs` and ``src/player/player.rs`

- [ ] investigate finding an existing library to replace `escape_ffmetadata` and `build_ffmetadata` etc.

## `src/ui/app/mod.rs`

- [ ] `video_by_id` is O(n) linear scan called on every UI interaction (click, toggle, summary, etc.) — add a `HashMap<String, usize>` index to `AppState`
- [ ] `build_ui` is ~350 lines and violates single responsibility — decompose it; the refresh button handler alone is 130 lines of inline async logic
- [ ] Clone `subs_file` once before `refresh_button.connect_clicked` and move it into the closure, rather than borrowing `state_clone` inside the closure on every button click

## `src/ui/app/cards.rs`

- [ ] `populate_flow_box` takes both `state: &AppState` and `state_rc: &Rc<RefCell<AppState>>` — `state` is just `&*state_rc.borrow()`; remove the redundant parameter

## `src/ui/app/refresh.rs`

- [ ] `download_missing_thumbnails` creates a new `reqwest::Client` on every call; share a client instance to retain connection pool benefits

## `src/ui/dialogs.rs`

- [ ] `show_text_dialog` closes the dialog twice: once in `connect_response` and once after `dialog.run()`; remove the trailing `dialog.close()`

## Cross-cutting

- [ ] `data::Tab` enum variants are missing doc comments (required by guidelines for all public items)
- [ ] `data::Video` has all fields `pub` — guidelines say fields should be private by default with accessor methods
- [ ] `gemini.rs` has zero tests despite containing the most complex logic (SSE parsing, JSON deserialization, model fallback, quote transformation)
- [ ] `player/mod.rs` has no tests
