- [ ] iso8601_to_humantime_duration should use a library for this purpose, don't reinvent the wheel
- [ ] video_id_from_stem — dense boundary predicate

  if id_start == 0 || stem[..id_start].ends_with('_') {
      Some(video_id)
  } else {
      None
  }
  This is correct: the ID is valid only if it's the entire stem, or the
  character immediately before it is _. But reading this cold, it's easy
   to ask "why ends_with and not a single char check on stem[id_start -
  1]?" The latter is equivalent and clearer:

  // id_start == 0: stem is exactly the video ID
  // stem.as_bytes()[id_start - 1] == b'_': ID follows an underscore
  separator

  Also, stem.get(id_start..) returning None when id_start > stem.len()
  is already handled by checked_sub, so the ? on get is technically
  redundant (the slice can't fail after checked_sub succeeds on valid
  UTF-8 boundaries). Not a bug, just belt-and-suspenders.
- [ ] gemini.rs call_gemini_streaming: use the non streaming api to greatly simplify the code
