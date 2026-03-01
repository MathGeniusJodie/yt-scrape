# Structural Improvements

- [ ] **Deduplicate tab button connection code** (`src/ui/app/mod.rs:556-589`) — Same `clone()` block and `connect_toggled` pattern repeated for Feed/Watch-Later/Downloading tabs. Extract a helper function.

- [ ] **Incremental video index updates** (`src/ui/app/mod.rs:61-72`) — `rebuild_video_index()` rebuilds the full `HashMap` on every `set_videos()` call. Use the `Entry` API to update only changed videos.
