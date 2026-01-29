# WIP terminal youtube client

Originally written in JS and not interractive. Slowly being rewritten in Rust. Designed to make you spend less time on youtube by having no recommendations and a builtin ai video summarizer.

use with alacritty or another similarly fast terminal emulator for best results.

## Todos:
* bugfixes
* close button on modals
* code cleanup and compartmentalization
* faster ai summarization using cached transcript and cerebras api.
* interface to add channels to subscriptions list
* search functionality?
* button to open in browser
* actual gui with framebuffer support
* support non-standard image rendering terminals
* settings page and command line args
* error handling and logging
* hide videos? log watched/ignored/hidden videos and collect stats so you know which channels you might want to unsubscribe from
* support for playlists, channel pages and playing urls
* add video descriptions to transcript and/or summary
* ascii mode for tty
* mouse support in tty
* 16 color mode for tty
* https://ratatui.rs/ecosystem/tachyonfx/ ?
* better use of unicode https://emojidb.org/ https://www.alanwood.net/demos/wingdings.html
