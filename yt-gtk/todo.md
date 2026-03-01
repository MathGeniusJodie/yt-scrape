- [ ] spawn_buffered_summary_generation defeats streaming
  (ui/app/summary.rs:46-98)
  The function name says "buffered" but it fully buffers all Gemini SSE
  chunks before returning anything to the UI. The streaming API is
  unused — you might as well call a non-streaming endpoint. Either wire
  the chunks to a gtk::TextBuffer progressively or don't bother with SSE
   at all.

- [ ] Duplicate playback logic (ui/app/cards.rs:60-87 and 209-234)
  The context menu Play button and the double-click handler are
  identical: video_by_id → find_video_path → resolve_playback_path →
  play_video. Extract a play_selected_video(state, runtime, video_id)
  function.

- [ ]  Video::published() returns &DateTime<Utc> (data.rs:64)
  DateTime<Utc> is Copy. Returning a reference to a Copy type is
  unidiomatic. Return by value.

- [ ] PLAYLIST_ITEMS_MAX_RESULTS: &str = "25" (feed/fetcher.rs:15)
  Typed as a string constant but semantically a number. Nothing enforces
   this is actually parseable. Use const PLAYLIST_ITEMS_MAX_RESULTS: u32
   = 25 and format it at the call site.

- [ ] should_retry includes NOT_FOUND (feed/fetcher.rs:575)
  404 is not a transient error. The comment-free inclusion here is
  confusing — the actual 404 handling is done in the caller before
  should_retry is consulted, but if that path falls through, a 404 will
  be retried unnecessarily.

- [ ] SelectedVideo struct (ui/app/mod.rs:92-95) — this is just video_id:
  String. A newtype for a string with one field adds no value, just
  wrapping noise.

- [ ] player/mod.rs:28 — stderr: Stdio::null() silently swallows all mpv
  errors including codec failures and missing file errors. At minimum
  pipe it for debug logging.

- [ ] build_ffmetadata fallback end time of chapter.start + 1.0
  (chapters.rs:133) — creates silent 1-second chapters when duration is
  unknown, which is worse than no chapter.

- [ ] the video card grid doesn't fully refresh on change from the main tab
 to the watch later tab, the column count starts at 1 and then after
 a few seconds pops to the correct number, I suspect the width of the
 container is unset when the tab is hidden or something similar try to
 find the most elegant fix