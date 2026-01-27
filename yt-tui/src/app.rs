use crate::cache::{download_video, Storage, ThumbnailCache};
use crate::data::{AppState, Tab};
use crate::feed::{fetch_all_feeds, load_channel_ids, FetchProgress};
use crate::player;
use crate::ui::{self, GridLayout};
use anyhow::Result;
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
use std::time::Duration;
use tokio::sync::mpsc;
use tui_scrollview::ScrollViewState;

pub struct App {
    pub state: AppState,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    storage: Storage,
    thumb_cache: ThumbnailCache,
    subs_file: PathBuf,
    layout: GridLayout,
    needs_redraw: bool,
    scroll_state: ScrollViewState,
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
        let thumb_cache = ThumbnailCache::new(storage.cache_dir().clone())?;

        let size = terminal.size()?;
        let layout = GridLayout::calculate(size.width, size.height);

        let state = AppState {
            terminal_cols: size.width,
            terminal_rows: size.height,
            videos: storage.load_videos(),
            watch_later: storage.load_watch_later(),
            ..Default::default()
        };

        Ok(Self {
            state,
            terminal,
            storage,
            thumb_cache,
            subs_file,
            layout,
            needs_redraw: true,
            scroll_state: ScrollViewState::default(),
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        // Start thumbnail downloads for any videos we have
        self.queue_thumbnail_downloads().await;
        // Start video downloads for watch later items
        self.queue_watch_later_downloads();

        loop {
            // Only redraw when state has changed
            if self.needs_redraw {
                let videos_dir = self.storage.videos_dir();
                self.terminal.draw(|f| {
                    ui::render(
                        f,
                        &self.state,
                        &self.layout,
                        &mut self.thumb_cache,
                        &mut self.scroll_state,
                        videos_dir,
                    );
                })?;
                self.needs_redraw = false;
            }

            // Handle events with a short timeout
            if event::poll(Duration::from_millis(100))? {
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

    /// Get current scroll offset in lines
    fn scroll_offset(&self) -> usize {
        self.scroll_state.offset().y as usize
    }

    /// Set scroll offset in lines
    fn set_scroll_offset(&mut self, offset: usize) {
        self.scroll_state
            .set_offset(ratatui::layout::Position::new(0, offset as u16));
    }

    /// Clamp scroll offset to valid range
    fn clamp_scroll(&mut self) {
        let total = self.state.current_videos().len();
        let max = self.layout.max_scroll(total);
        if self.scroll_offset() > max {
            self.set_scroll_offset(max);
        }
    }

    async fn handle_key(&mut self, key: event::KeyEvent) -> Result<bool> {
        // Handle help overlay first
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
                for _ in 0..self.layout.grid_height {
                    self.scroll_state.scroll_up();
                }
                self.needs_redraw = true;
            }
            KeyCode::PageDown => {
                for _ in 0..self.layout.grid_height {
                    self.scroll_state.scroll_down();
                }
                self.clamp_scroll();
                self.needs_redraw = true;
            }
            KeyCode::Home => {
                self.scroll_state.scroll_to_top();
                self.state.selected_index = Some(0);
                self.needs_redraw = true;
            }
            KeyCode::End => {
                let total = self.state.current_videos().len();
                self.set_scroll_offset(self.layout.max_scroll(total));
                self.state.selected_index = Some(total.saturating_sub(1));
                self.needs_redraw = true;
            }

            // Actions
            KeyCode::Enter => self.play_selected()?,
            KeyCode::Char('w') => self.toggle_watch_later()?,
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
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check header clicks (all 3 rows)
                if mouse.row < GridLayout::HEADER_HEIGHT {
                    // Check tab regions
                    let mut tab_clicked = false;
                    for (start, end, tab) in ui::header_tab_regions() {
                        if mouse.column >= start && mouse.column < end {
                            self.state.current_tab = tab;
                            self.scroll_state.scroll_to_top();
                            self.state.selected_index = None;
                            self.needs_redraw = true;
                            tab_clicked = true;
                            break;
                        }
                    }

                    // Check right-side buttons (only on row 0 where they're rendered)
                    if !tab_clicked
                        && mouse.row <= 3
                        && mouse.column >= self.state.terminal_cols.saturating_sub(11)
                    {
                        let right_offset = self.state.terminal_cols - mouse.column;
                        if right_offset > 6 {
                            // Refresh button area
                            if !self.state.is_refreshing {
                                self.refresh().await?;
                            }
                        } else {
                            // Help button
                            self.state.show_help = !self.state.show_help;
                            self.needs_redraw = true;
                        }
                    }
                } else {
                    // Grid click
                    let total = self.state.current_videos().len();

                    // Check if click is on a watch later checkbox
                    if let Some(idx) = self.layout.is_checkbox_click(
                        mouse.column,
                        mouse.row,
                        self.scroll_offset(),
                        total,
                    ) {
                        self.state.selected_index = Some(idx);
                        self.toggle_watch_later()?;
                    } else if let Some(idx) = self.layout.coords_to_index(
                        mouse.column,
                        mouse.row,
                        self.scroll_offset(),
                        total,
                    ) {
                        self.state.selected_index = Some(idx);
                        self.needs_redraw = true;
                        self.play_selected()?;
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    self.scroll_state.scroll_down();
                }
                self.clamp_scroll();
                self.needs_redraw = true;
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    self.scroll_state.scroll_up();
                }
                self.needs_redraw = true;
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

        // Adjust scroll to keep selection visible (line-based)
        let selected_row = new_idx / self.layout.cols;
        let card_top = selected_row * self.layout.card_height as usize;
        let card_bottom = card_top + self.layout.card_height as usize;
        let current_scroll = self.scroll_offset();

        // Scroll up if selection is above viewport
        if card_top < current_scroll {
            self.set_scroll_offset(card_top);
        }
        // Scroll down if selection is below viewport
        else if card_bottom > current_scroll + self.layout.grid_height as usize {
            self.set_scroll_offset(card_bottom.saturating_sub(self.layout.grid_height as usize));
        }
        self.needs_redraw = true;
    }

    fn play_selected(&self) -> Result<()> {
        if let Some(idx) = self.state.selected_index {
            let videos = self.state.current_videos();
            if let Some(video) = videos.get(idx) {
                // Check if this video is in watch later and might have a local copy
                let local_path = if self.state.watch_later.contains(&video.video_id) {
                    Some(self.storage.video_path(&video.video_id))
                } else {
                    None
                };
                player::play_video(&video.video_id, local_path.as_deref())?;
            }
        }
        Ok(())
    }

    fn toggle_watch_later(&mut self) -> Result<()> {
        if let Some(idx) = self.state.selected_index {
            let videos = self.state.current_videos();
            if let Some(video) = videos.get(idx) {
                let video_id = video.video_id.clone();
                if self.state.watch_later.contains(&video_id) {
                    self.state.watch_later.remove(&video_id);
                } else {
                    self.state.watch_later.insert(video_id.clone());
                    // Start background download if not already downloaded
                    let video_path = self.storage.video_path(&video_id);
                    if !video_path.exists() {
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

    fn switch_tab(&mut self) {
        self.state.current_tab = match self.state.current_tab {
            Tab::Feed => Tab::WatchLater,
            Tab::WatchLater => Tab::Feed,
        };
        self.scroll_state.scroll_to_top();
        self.state.selected_index = None;
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
                    self.terminal.draw(|f| {
                        ui::render(
                            f,
                            &self.state,
                            &self.layout,
                            &mut self.thumb_cache,
                            &mut self.scroll_state,
                            videos_dir,
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
            let video_path = self.storage.video_path(video_id);
            if !video_path.exists() {
                let video_id = video_id.clone();
                tokio::spawn(async move {
                    let _ = download_video(&video_id, &video_path).await;
                });
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
