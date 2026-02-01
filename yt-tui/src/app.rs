use crate::cache::{download_video, fetch_transcript, Storage, ThumbnailCache};
use crate::data::{AppState, Tab, TranscriptState};
use crate::feed::{fetch_all_feeds, load_channel_ids, FetchProgress};
use crate::gemini::{summarize_video_streaming, StreamingMessage, SummaryState};
use crate::player;
use crate::ui::{self, GridLayout, SelectionIndicator};
use crate::urls;
use anyhow::Result;
use arboard::Clipboard;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
    MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use std::io::{stdout, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tui_scrollview::ScrollViewState;

pub struct App {
    pub state: AppState,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    storage: Storage,
    thumb_cache: ThumbnailCache,
    thumb_render_rx: mpsc::UnboundedReceiver<()>,
    summary_stream_rx: Option<mpsc::UnboundedReceiver<StreamingMessage>>,
    transcript_rx: mpsc::UnboundedReceiver<(String, String)>, // (video_id, transcript)
    transcript_tx: mpsc::UnboundedSender<(String, String)>,
    subs_file: PathBuf,
    layout: GridLayout,
    needs_redraw: bool,
    scroll_state: ScrollViewState,
    // Inertial scrolling state
    scroll_position: f64,
    scroll_velocity: f64,
    last_rendered_scroll: usize,
    last_frame: Instant,
    // Animated selection indicator
    selection_indicator: SelectionIndicator,
}

impl App {
    pub fn new(subs_file: PathBuf) -> Result<Self> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = stdout();
        crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        let storage = Storage::new()?;
        let (thumb_cache, thumb_render_rx) = ThumbnailCache::new(storage.cache_dir().clone())?;
        let (transcript_tx, transcript_rx) = mpsc::unbounded_channel();

        let size = terminal.size()?;
        let layout = GridLayout::calculate(size.width, size.height);

        let videos = storage.load_videos();
        let selected_index = if videos.is_empty() { None } else { Some(0) };
        let state = AppState {
            terminal_cols: size.width,
            terminal_rows: size.height,
            videos,
            watch_later: storage.load_watch_later(),
            selected_index,
            ..Default::default()
        };

        Ok(Self {
            state,
            terminal,
            storage,
            thumb_cache,
            thumb_render_rx,
            summary_stream_rx: None,
            transcript_rx,
            transcript_tx,
            subs_file,
            layout,
            needs_redraw: true,
            scroll_state: ScrollViewState::default(),
            scroll_position: 0.0,
            scroll_velocity: 0.0,
            last_rendered_scroll: 0,
            last_frame: Instant::now(),
            selection_indicator: SelectionIndicator::new(),
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        // Start thumbnail downloads for any videos we have
        self.queue_thumbnail_downloads().await;
        // Start video downloads for watch later items
        self.queue_watch_later_downloads();
        // Transcripts are downloaded on-demand when the user clicks the transcript button
        // Initialize selection indicator for the first video
        self.sync_selection_indicator();

        loop {
            // Update scroll physics and selection animation
            self.update_physics();

            // Check for completed thumbnail renders
            while self.thumb_render_rx.try_recv().is_ok() {
                self.needs_redraw = true;
            }

            // Check for completed transcript downloads
            while let Ok((video_id, transcript)) = self.transcript_rx.try_recv() {
                // Check if this is an error message
                let is_error = transcript.starts_with("__ERROR__:");

                if is_error {
                    // If the modal is open for this video, show the error
                    if self.state.show_transcript
                        && self.state.transcript_video_id.as_ref() == Some(&video_id)
                    {
                        let error_msg = transcript.strip_prefix("__ERROR__:").unwrap_or(&transcript);
                        self.state.transcript_state =
                            Some(TranscriptState::Error(error_msg.to_string()));
                        self.needs_redraw = true;
                    }
                } else {
                    // Store transcript in video
                    if let Some(video) = self
                        .state
                        .videos
                        .iter_mut()
                        .find(|v| v.video_id == video_id)
                    {
                        video.transcript = Some(transcript.clone());
                        let _ = self.storage.save_videos(&self.state.videos);
                    }

                    // If the modal is open for this video, update the content
                    if self.state.show_transcript
                        && self.state.transcript_video_id.as_ref() == Some(&video_id)
                    {
                        self.state.transcript_state = Some(TranscriptState::Ready(transcript));
                    }

                    self.needs_redraw = true;
                }
            }

            // Check for streaming summary updates
            if let Some(ref mut rx) = self.summary_stream_rx {
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        StreamingMessage::Chunk(text) => {
                            // Append to existing streaming content
                            match &mut self.state.summary_state {
                                Some(SummaryState::Streaming(content)) => {
                                    content.push_str(&text);
                                }
                                Some(SummaryState::Loading) => {
                                    // First chunk - switch to streaming state
                                    self.state.summary_state = Some(SummaryState::Streaming(text));
                                }
                                _ => {}
                            }
                            self.needs_redraw = true;
                        }
                        StreamingMessage::Done => {
                            // Finalize the summary
                            if let Some(SummaryState::Streaming(content)) =
                                self.state.summary_state.take()
                            {
                                self.state.summary_state = Some(SummaryState::Ready(content));
                            }
                            self.summary_stream_rx = None;
                            self.needs_redraw = true;
                            break;
                        }
                        StreamingMessage::Error(e) => {
                            self.state.summary_state = Some(SummaryState::Error(e));
                            self.summary_stream_rx = None;
                            self.needs_redraw = true;
                            break;
                        }
                    }
                }
            }

            // Only redraw when state has changed
            if self.needs_redraw {
                let videos_dir = self.storage.videos_dir();
                let scroll_pos = self.scroll_position;
                let selection_indicator = &self.selection_indicator;
                self.terminal.draw(|f| {
                    ui::render(
                        f,
                        &self.state,
                        &self.layout,
                        &self.thumb_cache,
                        &mut self.scroll_state,
                        scroll_pos,
                        videos_dir,
                        selection_indicator,
                    );
                })?;
                self.needs_redraw = false;
            }

            // Use shorter poll timeout when animating, streaming, or loading transcript
            let is_streaming = self.summary_stream_rx.is_some();
            let is_loading_transcript = matches!(
                self.state.transcript_state,
                Some(TranscriptState::Loading)
            );
            let poll_timeout = if self.scroll_velocity.abs() > 0.5
                || self.selection_indicator.is_animating()
                || is_streaming
                || is_loading_transcript
            {
                Duration::from_millis(16) // ~60fps during animation/streaming/loading
            } else {
                Duration::from_millis(100) // idle
            };

            if event::poll(poll_timeout)? {
                match event::read()? {
                    Event::Key(key) => {
                        if self.handle_key(key).await? {
                            break;
                        }
                    }
                    Event::Mouse(mouse) => {
                        self.handle_mouse(mouse).await?;
                    }
                    Event::Resize(cols, rows) => {
                        self.handle_resize(cols, rows);
                    }
                    _ => {}
                }
            }

            if self.state.should_quit {
                break;
            }
        }

        Ok(())
    }

    /// Get current scroll offset in lines (quantized)
    fn scroll_offset(&self) -> usize {
        self.scroll_position.round().max(0.0) as usize
    }

    /// Set scroll offset in lines (stops any animation)
    fn set_scroll_offset(&mut self, offset: usize) {
        self.scroll_position = offset as f64;
        self.scroll_velocity = 0.0;
        self.last_rendered_scroll = offset;
        self.sync_scroll_state();
    }

    /// Clamp scroll offset to valid range (hard clamp, no bounce)
    fn clamp_scroll(&mut self) {
        let total = self.state.current_videos().len();
        let max = self.layout.max_scroll(total) as f64;
        if self.scroll_position > max {
            self.scroll_position = max;
        }
        if self.scroll_position < 0.0 {
            self.scroll_position = 0.0;
        }
        self.sync_scroll_state();
    }

    /// Get max scroll position
    fn max_scroll(&self) -> f64 {
        let total = self.state.current_videos().len();
        self.layout.max_scroll(total) as f64
    }

    /// Sync the integer scroll state from floating point position
    fn sync_scroll_state(&mut self) {
        let quantized = self.scroll_position.round() as usize;
        self.scroll_state
            .set_offset(ratatui::layout::Position::new(0, quantized as u16));
    }

    /// Update scroll physics and selection animation
    fn update_physics(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64();
        self.last_frame = now;

        // Cap dt to avoid huge jumps if the app was suspended
        let dt = dt.min(0.1);

        let max_scroll = self.max_scroll();

        // Apply friction
        const FRICTION: f64 = 0.93;
        self.scroll_velocity *= FRICTION.powf(dt * 60.0);

        // Apply velocity
        if self.scroll_velocity.abs() > 0.01 {
            self.scroll_position += self.scroll_velocity * dt * 60.0;
        }

        // Clamp to bounds and kill velocity at edges
        if self.scroll_position < 0.0 {
            self.scroll_position = 0.0;
            self.scroll_velocity = 0.0;
        } else if self.scroll_position > max_scroll {
            self.scroll_position = max_scroll.max(0.0);
            self.scroll_velocity = 0.0;
        }

        // Update display if quantized position changed
        let quantized = self.scroll_position.round() as usize;
        if quantized != self.last_rendered_scroll {
            self.last_rendered_scroll = quantized;
            self.sync_scroll_state();
            self.needs_redraw = true;
        }

        // Stop if velocity is negligible
        if self.scroll_velocity.abs() < 0.2 {
            self.scroll_velocity = 0.0;
        }

        // Animate selection indicator
        if self.selection_indicator.animate(dt) {
            self.needs_redraw = true;
        }
    }

    async fn handle_key(&mut self, key: event::KeyEvent) -> Result<bool> {
        // Handle transcript modal first
        if self.state.show_transcript {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.close_transcript_modal();
                }
                KeyCode::Char('y') => {
                    // Copy transcript to clipboard
                    if let Some(TranscriptState::Ready(transcript)) = &self.state.transcript_state {
                        if let Ok(mut clipboard) = Clipboard::new() {
                            let _ = clipboard.set_text(transcript.clone());
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.transcript_scroll = self.state.transcript_scroll.saturating_sub(1);
                    self.needs_redraw = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.transcript_scroll = self.state.transcript_scroll.saturating_add(1);
                    self.needs_redraw = true;
                }
                KeyCode::PageUp => {
                    self.state.transcript_scroll = self.state.transcript_scroll.saturating_sub(10);
                    self.needs_redraw = true;
                }
                KeyCode::PageDown => {
                    self.state.transcript_scroll = self.state.transcript_scroll.saturating_add(10);
                    self.needs_redraw = true;
                }
                KeyCode::Home => {
                    self.state.transcript_scroll = 0;
                    self.needs_redraw = true;
                }
                _ => {}
            }
            return Ok(false);
        }

        // Handle summary modal
        if self.state.show_summary {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.close_summary_modal();
                }
                KeyCode::Char('y') => {
                    // Copy summary to clipboard
                    if let Some(SummaryState::Ready(summary)) = &self.state.summary_state {
                        if let Ok(mut clipboard) = Clipboard::new() {
                            let _ = clipboard.set_text(summary.clone());
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.summary_scroll = self.state.summary_scroll.saturating_sub(1);
                    self.needs_redraw = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.summary_scroll = self.state.summary_scroll.saturating_add(1);
                    self.needs_redraw = true;
                }
                KeyCode::PageUp => {
                    self.state.summary_scroll = self.state.summary_scroll.saturating_sub(10);
                    self.needs_redraw = true;
                }
                KeyCode::PageDown => {
                    self.state.summary_scroll = self.state.summary_scroll.saturating_add(10);
                    self.needs_redraw = true;
                }
                KeyCode::Home => {
                    self.state.summary_scroll = 0;
                    self.needs_redraw = true;
                }
                _ => {}
            }
            return Ok(false);
        }

        // Handle help overlay
        if self.state.show_help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
            ) {
                self.state.show_help = false;
                self.needs_redraw = true;
            }
            return Ok(false);
        }

        match key.code {
            // Navigation
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-(self.layout.cols as i32)),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(self.layout.cols as i32),
            KeyCode::Left | KeyCode::Char('h') => self.move_selection(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_selection(1),
            KeyCode::PageUp => {
                self.scroll_velocity = -(self.layout.grid_height as f64 * 1.5);
            }
            KeyCode::PageDown => {
                self.scroll_velocity = self.layout.grid_height as f64 * 1.5;
            }
            KeyCode::Home => {
                self.set_scroll_offset(0);
                self.set_selection(0);
                self.sync_selection_indicator(); // Jump, don't animate
                self.needs_redraw = true;
            }
            KeyCode::End => {
                let total = self.state.current_videos().len();
                self.set_scroll_offset(self.layout.max_scroll(total));
                let last_idx = total.saturating_sub(1);
                self.set_selection(last_idx);
                self.sync_selection_indicator(); // Jump, don't animate
                self.needs_redraw = true;
            }

            // Actions
            KeyCode::Enter => self.play_selected()?,
            KeyCode::Char('w') => self.toggle_watch_later()?,
            KeyCode::Char('s') => {
                if let Some(idx) = self.state.selected_index {
                    self.show_video_summary(idx)?;
                }
            }
            KeyCode::Char('r') => self.refresh().await?,
            KeyCode::Tab => self.switch_tab(),
            KeyCode::Char('?') => {
                self.state.show_help = true;
                self.needs_redraw = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),

            _ => {}
        }

        Ok(false)
    }

    async fn handle_mouse(&mut self, mouse: event::MouseEvent) -> Result<()> {
        // Helper to check if a point is inside a rect
        let point_in_rect = |col: u16, row: u16, rect: Rect| {
            col >= rect.x
                && col < rect.x + rect.width
                && row >= rect.y
                && row < rect.y + rect.height
        };

        // When transcript modal is visible, handle scroll and click-outside-to-close
        if self.state.show_transcript {
            match mouse.kind {
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                    const SCROLL_LINES: u16 = 3;
                    if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                        self.state.transcript_scroll =
                            self.state.transcript_scroll.saturating_add(SCROLL_LINES);
                    } else {
                        self.state.transcript_scroll =
                            self.state.transcript_scroll.saturating_sub(SCROLL_LINES);
                    }
                    self.needs_redraw = true;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let bounds = ui::transcript_modal_bounds(
                        self.state.terminal_cols,
                        self.state.terminal_rows,
                    );
                    if !point_in_rect(mouse.column, mouse.row, bounds) {
                        self.close_transcript_modal();
                    }
                }
                _ => {}
            }
            return Ok(());
        }

        // When summary modal is visible, handle scroll and click-outside-to-close
        if self.state.show_summary {
            match mouse.kind {
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                    const SCROLL_LINES: u16 = 3;
                    if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                        self.state.summary_scroll =
                            self.state.summary_scroll.saturating_add(SCROLL_LINES);
                    } else {
                        self.state.summary_scroll =
                            self.state.summary_scroll.saturating_sub(SCROLL_LINES);
                    }
                    self.needs_redraw = true;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let bounds = ui::summary_modal_bounds(
                        self.state.terminal_cols,
                        self.state.terminal_rows,
                    );
                    if !point_in_rect(mouse.column, mouse.row, bounds) {
                        self.close_summary_modal();
                    }
                }
                _ => {}
            }
            return Ok(());
        }

        // When help modal is visible, click-outside-to-close
        if self.state.show_help {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                let bounds =
                    ui::help_modal_bounds(self.state.terminal_cols, self.state.terminal_rows);
                if !point_in_rect(mouse.column, mouse.row, bounds) {
                    self.state.show_help = false;
                    self.needs_redraw = true;
                }
            }
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check header clicks (all 3 rows)
                if mouse.row < GridLayout::HEADER_HEIGHT {
                    // Check tab regions and refresh icon
                    let (tab_regions, refresh_region) = ui::header_tab_regions();
                    let mut tab_clicked = false;

                    // Check refresh icon first (it's inside the Feed tab)
                    if let Some((refresh_start, refresh_end)) = refresh_region {
                        if mouse.column >= refresh_start && mouse.column < refresh_end {
                            if !self.state.is_refreshing {
                                self.refresh().await?;
                            }
                            tab_clicked = true; // Prevent tab switch
                        }
                    }

                    // Check tab regions
                    if !tab_clicked {
                        for (start, end, tab) in tab_regions {
                            if mouse.column >= start && mouse.column < end {
                                self.state.current_tab = tab;
                                self.set_scroll_offset(0);
                                // Select first video in new tab, or hide selection if empty
                                if self.state.current_videos().is_empty() {
                                    self.state.selected_index = None;
                                    self.selection_indicator.hide();
                                } else {
                                    self.state.selected_index = Some(0);
                                    self.sync_selection_indicator();
                                }
                                self.needs_redraw = true;
                                tab_clicked = true;
                                break;
                            }
                        }
                    }

                    // Check right-side help button (only on row 0 where they're rendered)
                    if !tab_clicked
                        && mouse.row <= 3
                        && mouse.column >= self.state.terminal_cols.saturating_sub(6)
                    {
                        // Help button
                        self.state.show_help = !self.state.show_help;
                        self.needs_redraw = true;
                    }
                } else {
                    // Grid click
                    let total = self.state.current_videos().len();

                    // Check if click is on the summary button (✨)
                    if let Some(idx) = self.layout.is_summary_button_click(
                        mouse.column,
                        mouse.row,
                        self.scroll_offset(),
                        total,
                    ) {
                        self.set_selection(idx);
                        self.show_video_summary(idx)?;
                    }
                    // Check if click is on the transcript button (🗏)
                    else if let Some(idx) = self.layout.is_transcript_button_click(
                        mouse.column,
                        mouse.row,
                        self.scroll_offset(),
                        total,
                    ) {
                        self.set_selection(idx);
                        self.show_video_transcript(idx)?;
                    }
                    // Check if click is on a watch later checkbox
                    else if let Some(idx) = self.layout.is_checkbox_click(
                        mouse.column,
                        mouse.row,
                        self.scroll_offset(),
                        total,
                    ) {
                        self.set_selection(idx);
                        self.toggle_watch_later()?;
                    } else if let Some(idx) = self.layout.coords_to_index(
                        mouse.column,
                        mouse.row,
                        self.scroll_offset(),
                        total,
                    ) {
                        self.set_selection(idx);
                        self.needs_redraw = true;
                        self.play_selected()?;
                    }
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                const SCROLL_IMPULSE: f64 = 2.5;

                // Drain all pending scroll events and accumulate impulse
                let mut impulse: f64 = if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                    SCROLL_IMPULSE
                } else {
                    -SCROLL_IMPULSE
                };

                // Consume any additional queued scroll events
                while event::poll(Duration::ZERO).unwrap_or(false) {
                    if let Ok(Event::Mouse(m)) = event::read() {
                        match m.kind {
                            MouseEventKind::ScrollDown => impulse += SCROLL_IMPULSE,
                            MouseEventKind::ScrollUp => impulse -= SCROLL_IMPULSE,
                            _ => break,
                        }
                    } else {
                        break;
                    }
                }

                // Add to velocity (with some resistance if already moving opposite direction)
                if self.scroll_velocity.signum() != impulse.signum() {
                    self.scroll_velocity = impulse; // Override if changing direction
                } else {
                    self.scroll_velocity += impulse * 0.5; // Diminishing returns when scrolling same direction
                }

                // Cap maximum velocity
                const MAX_VELOCITY: f64 = 25.0;
                self.scroll_velocity = self.scroll_velocity.clamp(-MAX_VELOCITY, MAX_VELOCITY);
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.state.terminal_cols = cols;
        self.state.terminal_rows = rows;
        self.layout = GridLayout::calculate(cols, rows);
        self.thumb_cache.clear_rendered_cache(); // Re-render thumbnails at new size
        self.scroll_velocity = 0.0; // Stop any animation
        self.clamp_scroll();
        self.needs_redraw = true;
    }

    fn move_selection(&mut self, delta: i32) {
        let total = self.state.current_videos().len();
        if total == 0 {
            return;
        }

        let current = self.state.selected_index.unwrap_or(0) as i32;
        let new_idx = (current + delta).clamp(0, total as i32 - 1) as usize;
        self.state.selected_index = Some(new_idx);

        // Update selection indicator target
        let (card_x, card_y) = self.layout.card_rect(new_idx);
        self.selection_indicator.set_target(card_x, card_y);

        // Adjust scroll to keep selection visible (line-based)
        let selected_row = new_idx / self.layout.cols;
        let stride = self.layout.card_stride() as usize;
        let card_top = selected_row * stride;
        let card_bottom = card_top + self.layout.card_height as usize;
        let current_scroll = self.scroll_offset();

        // Scroll up if selection is above viewport (instant jump, no inertia)
        if card_top < current_scroll {
            self.scroll_position = card_top as f64;
            self.scroll_velocity = 0.0;
        }
        // Scroll down if selection is below viewport
        else if card_bottom > current_scroll + self.layout.grid_height as usize {
            self.scroll_position =
                (card_bottom.saturating_sub(self.layout.grid_height as usize)) as f64;
            self.scroll_velocity = 0.0;
        }
        self.needs_redraw = true;
    }

    /// Update selection indicator to match current selection (jump, no animation)
    fn sync_selection_indicator(&mut self) {
        if let Some(idx) = self.state.selected_index {
            let (card_x, card_y) = self.layout.card_rect(idx);
            self.selection_indicator.jump_to(card_x, card_y);
        } else {
            self.selection_indicator.hide();
        }
    }

    /// Close the summary modal
    fn close_summary_modal(&mut self) {
        self.state.show_summary = false;
        self.state.summary_state = None;
        self.state.summary_scroll = 0;
        self.state.summary_video_title = None;
        self.summary_stream_rx = None; // Stop processing streaming updates
        self.needs_redraw = true;
    }

    /// Close the transcript modal
    fn close_transcript_modal(&mut self) {
        self.state.show_transcript = false;
        self.state.transcript_state = None;
        self.state.transcript_scroll = 0;
        self.state.transcript_video_title = None;
        self.state.transcript_video_id = None;
        self.needs_redraw = true;
    }

    /// Set selection to a specific index (used by mouse clicks)
    fn set_selection(&mut self, idx: usize) {
        let was_selected = self.state.selected_index;
        self.state.selected_index = Some(idx);

        let (card_x, card_y) = self.layout.card_rect(idx);

        // If this is the first selection or a distant jump, just jump to the position
        if was_selected.is_none() {
            self.selection_indicator.jump_to(card_x, card_y);
        } else {
            self.selection_indicator.set_target(card_x, card_y);
        }
    }

    fn play_selected(&self) -> Result<()> {
        if let Some(idx) = self.state.selected_index {
            let videos = self.state.current_videos();
            if let Some(video) = videos.get(idx) {
                // Check if this video is in watch later and might have a local copy
                let local_path = if self.state.watch_later.contains(&video.video_id) {
                    self.storage.find_video_path(&video.video_id)
                } else {
                    None
                };
                player::play_video(&video.video_id, &video.title, local_path.as_deref())?;
            }
        }
        Ok(())
    }

    fn toggle_watch_later(&mut self) -> Result<()> {
        if let Some(idx) = self.state.selected_index {
            let videos = self.state.current_videos();
            if let Some(video) = videos.get(idx) {
                let video_id = video.video_id.clone();
                let video_title = video.title.clone();
                if self.state.watch_later.contains(&video_id) {
                    self.state.watch_later.remove(&video_id);
                } else {
                    self.state.watch_later.insert(video_id.clone());
                    // Start background download if not already downloaded
                    if !self.storage.has_video(&video_id) {
                        let video_path = self.storage.video_path(&video_id, &video_title);
                        tokio::spawn(async move {
                            let _ = download_video(&video_id, &video_path).await;
                        });
                    }
                }
                self.storage.save_watch_later(&self.state.watch_later)?;
                self.needs_redraw = true;
            }
        }
        Ok(())
    }

    fn show_video_summary(&mut self, idx: usize) -> Result<()> {
        let videos = self.state.current_videos();
        if let Some(video) = videos.get(idx) {
            let video_title = video.title.clone();
            let channel_name = video.channel_name.clone();
            let video_url = urls::watch_url(&video.video_id);

            // Show modal with loading state
            self.state.show_summary = true;
            self.state.summary_state = Some(SummaryState::Loading);
            self.state.summary_scroll = 0;
            self.state.summary_video_title = Some(video_title.clone());
            self.needs_redraw = true;

            // Create channel for streaming updates
            let (tx, rx) = mpsc::unbounded_channel();
            self.summary_stream_rx = Some(rx);

            // Spawn streaming task in background
            tokio::spawn(async move {
                summarize_video_streaming(&video_url, &video_title, &channel_name, tx).await;
            });
        }
        Ok(())
    }

    fn show_video_transcript(&mut self, idx: usize) -> Result<()> {
        // Extract data we need before modifying state
        let video_data = {
            let videos = self.state.current_videos();
            videos.get(idx).map(|v| {
                (
                    v.video_id.clone(),
                    v.title.clone(),
                    v.transcript.clone(),
                )
            })
        };

        if let Some((video_id, video_title, transcript)) = video_data {
            // Show modal
            self.state.show_transcript = true;
            self.state.transcript_scroll = 0;
            self.state.transcript_video_title = Some(video_title);
            self.state.transcript_video_id = Some(video_id.clone());

            // Check if transcript already exists
            if let Some(transcript) = transcript {
                self.state.transcript_state = Some(TranscriptState::Ready(transcript));
            } else {
                // Show loading state and start download
                self.state.transcript_state = Some(TranscriptState::Loading);

                // Start transcript download in background
                let work_dir = self.storage.transcripts_work_dir().clone();
                let tx = self.transcript_tx.clone();
                tokio::spawn(async move {
                    match fetch_transcript(&video_id, &work_dir).await {
                        Ok(transcript) => {
                            let _ = tx.send((video_id, transcript));
                        }
                        Err(e) => {
                            // Send error through the channel
                            let _ = tx.send((video_id, format!("__ERROR__:{}", e)));
                        }
                    }
                });
            }
            self.needs_redraw = true;
        }
        Ok(())
    }

    fn switch_tab(&mut self) {
        self.state.current_tab = match self.state.current_tab {
            Tab::Feed => Tab::WatchLater,
            Tab::WatchLater => Tab::Feed,
        };
        self.set_scroll_offset(0);
        // Select first video in new tab, or hide selection if empty
        if self.state.current_videos().is_empty() {
            self.state.selected_index = None;
            self.selection_indicator.hide();
        } else {
            self.state.selected_index = Some(0);
            self.sync_selection_indicator();
        }
        self.needs_redraw = true;
    }

    async fn refresh(&mut self) -> Result<()> {
        if self.state.is_refreshing {
            return Ok(());
        }

        self.state.is_refreshing = true;
        self.state.status_message = Some("Loading channel list...".to_string());

        // Load channel IDs
        let channel_ids = load_channel_ids(&self.subs_file)?;
        let total_channels = channel_ids.len();
        self.state.status_message = Some(format!("Fetching {} channels...", total_channels));

        // Fetch feeds
        let (tx, mut rx) = mpsc::channel::<FetchProgress>(100);

        let fetch_handle = tokio::spawn(async move { fetch_all_feeds(channel_ids, tx).await });

        // Process progress updates while fetching
        let mut completed = 0;
        while let Some(progress) = rx.recv().await {
            match progress {
                FetchProgress::Started { total } => {
                    self.state.status_message = Some(format!("Fetching 0/{} channels...", total));
                }
                FetchProgress::ChannelComplete { .. } => {
                    completed += 1;
                    self.state.status_message =
                        Some(format!("Fetching {}/{}...", completed, total_channels));

                    // Redraw to show progress
                    let videos_dir = self.storage.videos_dir();
                    let scroll_pos = self.scroll_position;
                    let selection_indicator = &self.selection_indicator;
                    self.terminal.draw(|f| {
                        ui::render(
                            f,
                            &self.state,
                            &self.layout,
                            &self.thumb_cache,
                            &mut self.scroll_state,
                            scroll_pos,
                            videos_dir,
                            selection_indicator,
                        );
                    })?;
                }
                FetchProgress::Error { channel_id, error } => {
                    // Log errors but continue
                    eprintln!("Error fetching {}: {}", channel_id, error);
                }
                FetchProgress::AllComplete { total_videos } => {
                    self.state.status_message = Some(format!("Loaded {} videos", total_videos));
                }
            }
        }

        // Get final results
        match fetch_handle.await? {
            Ok(videos) => {
                self.state.videos = videos;
                self.state.last_refresh = Some(chrono::Utc::now());
                self.storage.save_videos(&self.state.videos)?;

                // Queue thumbnail downloads
                self.queue_thumbnail_downloads().await;
            }
            Err(e) => {
                self.state.status_message = Some(format!("Error: {}", e));
            }
        }

        self.state.is_refreshing = false;
        self.state.status_message = None;
        self.needs_redraw = true;

        Ok(())
    }

    async fn queue_thumbnail_downloads(&mut self) {
        // Download thumbnails for visible videos
        for video in &self.state.videos {
            if !self.thumb_cache.has_thumbnail(&video.video_id) {
                let _ = self.thumb_cache.download(video).await;
            }
        }
    }

    fn queue_watch_later_downloads(&self) {
        // Download videos for watch later items that aren't already downloaded
        for video_id in &self.state.watch_later {
            if !self.storage.has_video(video_id) {
                // Find the video to get its title
                if let Some(video) = self.state.videos.iter().find(|v| &v.video_id == video_id) {
                    let video_path = self.storage.video_path(video_id, &video.title);
                    let video_id = video_id.clone();
                    tokio::spawn(async move {
                        let _ = download_video(&video_id, &video_path).await;
                    });
                }
            }
        }
    }

}

impl Drop for App {
    fn drop(&mut self) {
        // Restore terminal
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = self.terminal.show_cursor();
    }
}
