# Code Review TODO

## Performance


- [ ] **#1 Two separate `reqwest::Client` instances per summary** — `call_gemini_streaming` and `call_openrouter_with_transcript` each build their own `Client`, allocating a connection pool and TLS context. Share a single client. (`gemini.rs:225`, `gemini.rs:358`)

- [ ] **#2 `load_channel_ids` allocates `String` for filtered-out lines** — `.map(|s| s.trim().to_string())` runs before `.filter(...)`, allocating for empty and comment lines that are immediately discarded. Swap the order. (`fetcher.rs:603-606`)

- [ ] **#3 `let prompt = SUMMARY_PROMPT.to_string()` is a needless allocation** — `prompt` is passed as `&str` immediately. Pass `SUMMARY_PROMPT` directly. (`gemini.rs:185`)

- [ ] **#4 No tests for `chapters.rs`** — `parse_time_token` and `parse_description_chapters` have non-trivial edge cases (punctuation stripping, 3-component rejection, 1ms deduplication) but zero unit tests. (`player/chapters.rs`)

- [ ] **#5 `clean_transcript` does redundant double-normalization** — All lines are already trimmed and non-empty before `join(" ")`. The subsequent `split_whitespace().collect::<Vec<_>>().join(" ")` pass is redundant and allocates an intermediate `Vec`. (`transcript.rs:166`)

- [ ] **#6 `BROWSER_USER_AGENT` is unnecessary for Google API calls** — The YouTube Data API v3 ignores User-Agent. Using a fake browser UA is noise. (`fetcher.rs:20-21`)

- [ ] **#7 `load_videos`/`load_watch_later` silently swallow all errors** — Both return defaults on IO errors AND JSON parse errors without logging. A corrupted cache file is indistinguishable from a missing one, and data is silently lost on the next save. Add at least a `warn!` log. (`storage.rs:189-195`, `storage.rs:153-160`)

- [ ] **#8 `SUMMARY_PROMPT` has inconsistent casing** — The constant switches between sentence case and lowercase mid-text. (`gemini.rs:16`)

for claude: do a brutal code review of subtitle_requests.rs
