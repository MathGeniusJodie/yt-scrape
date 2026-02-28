# Code Review TODO

## Architecture / Design

- [ ] **#5 Full `videos.json` rewrite for single-field updates** — Saving one transcript or AI summary rewrites the entire 400-video list. Use per-video sidecar files or at minimum debounce saves. (`summary.rs:88`, `summary.rs:377`)

- [ ] **#6 Triple-channel relay in the refresh pipeline** — `tokio::mpsc` → relay task → `async_channel` → `glib::MainContext`. The relay task exists only to bridge channel types. Use `async_channel` directly in `fetch_all_feeds` to eliminate the extra task and channel. (`app/mod.rs:441-449`)

- [ ] **#7 `SelectedVideo` holds a stale snapshot of `Video` fields** — If the underlying `Video` is mutated after `SelectedVideo` is created (e.g. AI summary added), the snapshot is stale. Store only `video_id` and look up the rest from state at action time. (`app/mod.rs:37-53`, `cards.rs`)

## Correctness Bugs

- [ ] **#8 `backoff_ms_for_attempt` MAX cap makes backoff ineffective** — `MAX_BACKOFF_MS = 4_000` clamps every retry to 4 seconds. `base_ms` for rate-limited errors is 30,000ms and for 404s is 20,000ms — both already exceed the cap, so the exponential multiplier never matters. The cap was likely intended to limit growth of a small base, not the entire computed value. (`fetcher.rs:582-584`)

- [ ] **#9 `find_subtitle_path` prefix-check false-positive** — `name.starts_with(video_id)` would match `abc123xyz.en.json3` when fetching video `abc123`. Change to `name.starts_with(&format!("{video_id}."))`. (`transcript.rs:108`)

- [ ] **#10 `should_retry` includes 401/403 — permanent auth errors** — Retrying `UNAUTHORIZED` and `FORBIDDEN` is almost never useful (key is wrong or revoked). Each retry wastes quota and adds a 30-second delay. Remove these from the retry set. (`fetcher.rs:554-555`)

- [ ] **#12 Legacy download triggers repeated re-downloads on every play** — `resolve_playback_path` spawns a re-download every time a legacy video is played. If the first re-download is still running, a second one starts. Add a guard (e.g. check if the upgraded path already exists or track in-progress IDs). (`app/mod.rs:99-107`)

## Performance

- [ ] **#13 SSE buffer manipulation allocates on every event** — `buffer[..event_end].to_string()` and `buffer = buffer[event_end + 2..].to_string()` allocate new `String`s per SSE event. Use `buffer.drain(..event_end + 2)` to mutate in place. (`gemini.rs:289-290`)

- [ ] **#14 `to_string_pretty` for the 400-video cache** — Pretty-printing adds ~40% size and CPU overhead. Use `serde_json::to_string` or `to_writer` with a `BufWriter`. (`storage.rs:208`)

- [ ] **#15 `sanitize_filename` allocates twice** — `.collect::<String>()` then `.trim_end().to_string()`. The second allocation can be avoided. (`storage.rs:24-27`)

- [ ] **#16 Two separate `reqwest::Client` instances per summary** — `call_gemini_streaming` and `call_openrouter_with_transcript` each build their own `Client`, allocating a connection pool and TLS context. Share a single client. (`gemini.rs:225`, `gemini.rs:358`)

- [ ] **#17 `load_channel_ids` allocates `String` for filtered-out lines** — `.map(|s| s.trim().to_string())` runs before `.filter(...)`, allocating for empty and comment lines that are immediately discarded. Swap the order. (`fetcher.rs:603-606`)

## Code Quality / Style

- [ ] **#18 `let prompt = SUMMARY_PROMPT.to_string()` is a needless allocation** — `prompt` is passed as `&str` immediately. Pass `SUMMARY_PROMPT` directly. (`gemini.rs:185`)

- [ ] **#19 `format_two_line_title` uses `"\n--"` as a height stabilizer** — The `"--"` placeholder is visible if label styling changes. Handle via CSS `min-height` on the title label instead. (`video_card.rs:247`)

- [ ] **#20 `CHARS_PER_LINE = 38` breaks for non-ASCII text** — Char-counting with a proportional font is inaccurate; CJK characters are double-width. The entire `format_two_line_title` heuristic is fragile. (`video_card.rs:242`)

- [ ] **#21 `create_context_menu` takes 7 parameters, violating the 5-param limit** — Pack parameters into a context/config struct. (`cards.rs:16-24`)

- [ ] **#22 `build_ui` has excessive redundant cloning** — `state`, `ui_context`, `spinner`, `status_label` are cloned multiple layers deep inside the refresh closure. `state_clone` is then cloned again into `state_for_videos`. Refactor the closure body into a named function. (`app/mod.rs:391-558`)

- [ ] **#23 Missing `dotenvy` for API key loading** — `CLAUDE.md` requires `dotenvy` for secrets. `GEMINI_API_KEY`, `OPENROUTER_API_KEY`, and `GOOGLE_API_KEY` are read via raw `std::env::var` with no `.env` file support. (`gemini.rs`, `fetcher.rs`)

- [ ] **#24 No tests for `chapters.rs`** — `parse_time_token` and `parse_description_chapters` have non-trivial edge cases (punctuation stripping, 3-component rejection, 1ms deduplication) but zero unit tests. (`player/chapters.rs`)

- [ ] **#25 `clean_transcript` does redundant double-normalization** — All lines are already trimmed and non-empty before `join(" ")`. The subsequent `split_whitespace().collect::<Vec<_>>().join(" ")` pass is redundant and allocates an intermediate `Vec`. (`transcript.rs:166`)

- [ ] **#26 `BROWSER_USER_AGENT` is unnecessary for Google API calls** — The YouTube Data API v3 ignores User-Agent. Using a fake browser UA is noise. (`fetcher.rs:20-21`)

- [ ] **#27 `load_videos`/`load_watch_later` silently swallow all errors** — Both return defaults on IO errors AND JSON parse errors without logging. A corrupted cache file is indistinguishable from a missing one, and data is silently lost on the next save. Add at least a `warn!` log. (`storage.rs:189-195`, `storage.rs:153-160`)

- [ ] **#28 `SUMMARY_PROMPT` has inconsistent casing** — The constant switches between sentence case and lowercase mid-text. (`gemini.rs:16`)

for claude: do a brutal code review of subtitle_requests.rs
