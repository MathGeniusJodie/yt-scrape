use crate::cache::{Storage, ThumbnailCache};
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

pub struct App {
    pub state: AppState,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    storage: Storage,
    thumb_cache: ThumbnailCache,
    subs_file: PathBuf,
    layout: GridLayout,
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

        let mut state = AppState::default();
        state.terminal_cols = size.width;
        state.terminal_rows = size.height;

        // Load cached data
        state.videos = storage.load_videos();
        state.watch_later = storage.load_watch_later();

        Ok(Self {
            state,
            terminal,
            storage,
            thumb_cache,
            subs_file,
            layout,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        // Start thumbnail downloads for any videos we have
        self.queue_thumbnail_downloads().await;

        loop {
            // Draw UI
            self.terminal.draw(|f| {
                ui::render(f, &self.state, &self.layout, &mut self.thumb_cache);
            })?;

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

    async fn handle_key(&mut self, key: event::KeyEvent) -> Result<bool> {
        // Handle help overlay first
        if self.state.show_help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
            ) {
                self.state.show_help = false;
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
                self.scroll(-(self.layout.grid_height as i32));
            }
            KeyCode::PageDown => {
                self.scroll(self.layout.grid_height as i32);
            }
            KeyCode::Home => {
                self.state.scroll_offset = 0;
                self.state.selected_index = Some(0);
            }
            KeyCode::End => {
                let total = self.state.current_videos().len();
                self.state.scroll_offset = self.layout.max_scroll(total);
                self.state.selected_index = Some(total.saturating_sub(1));
            }

            // Actions
            KeyCode::Enter => self.play_selected()?,
            KeyCode::Char('w') => self.toggle_watch_later()?,
            KeyCode::Char('r') => self.refresh().await?,
            KeyCode::Tab => self.switch_tab(),
            KeyCode::Char('?') => self.state.show_help = true,
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),

            _ => {}
        }

        Ok(false)
    }

    async fn handle_mouse(&mut self, mouse: event::MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check header clicks
                if mouse.row == 0 {
                    if mouse.column < 8 {
                        // [Feed]
                        self.state.current_tab = Tab::Feed;
                        self.state.scroll_offset = 0;
                        self.state.selected_index = None;
                    } else if mouse.column < 22 {
                        // [Watch Later]
                        self.state.current_tab = Tab::WatchLater;
                        self.state.scroll_offset = 0;
                        self.state.selected_index = None;
                    } else if mouse.column >= self.state.terminal_cols.saturating_sub(20) {
                        // Right side buttons
                        let right_offset = self.state.terminal_cols - mouse.column;
                        if right_offset > 10 {
                            // Refresh button area
                            if !self.state.is_refreshing {
                                self.refresh().await?;
                            }
                        } else {
                            // Help button
                            self.state.show_help = !self.state.show_help;
                        }
                    }
                } else {
                    // Grid click
                    let total = self.state.current_videos().len();
                    if let Some(idx) =
                        self.layout
                            .coords_to_index(mouse.column, mouse.row, self.state.scroll_offset, total)
                    {
                        if self.state.selected_index == Some(idx) {
                            // Double-click effect: play if already selected
                            self.play_selected()?;
                        } else {
                            self.state.selected_index = Some(idx);
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                self.scroll(1);
            }
            MouseEventKind::ScrollUp => {
                self.scroll(-1);
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.state.terminal_cols = cols;
        self.state.terminal_rows = rows;
        self.layout = GridLayout::calculate(cols, rows);

        // Clamp scroll offset to valid range
        let total = self.state.current_videos().len();
        let max_scroll = self.layout.max_scroll(total);
        if self.state.scroll_offset > max_scroll {
            self.state.scroll_offset = max_scroll;
        }
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

        // Scroll up if selection is above viewport
        if card_top < self.state.scroll_offset {
            self.state.scroll_offset = card_top;
        }
        // Scroll down if selection is below viewport
        else if card_bottom > self.state.scroll_offset + self.layout.grid_height as usize {
            self.state.scroll_offset = card_bottom.saturating_sub(self.layout.grid_height as usize);
        }
    }

    fn scroll(&mut self, delta: i32) {
        let total = self.state.current_videos().len();
        let max_scroll = self.layout.max_scroll(total);

        let new_offset = (self.state.scroll_offset as i32 + delta).clamp(0, max_scroll as i32);
        self.state.scroll_offset = new_offset as usize;
    }

    fn play_selected(&self) -> Result<()> {
        if let Some(idx) = self.state.selected_index {
            let videos = self.state.current_videos();
            if let Some(video) = videos.get(idx) {
                player::play_video(&video.video_id)?;
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
                    self.state.watch_later.insert(video_id);
                }
                self.storage.save_watch_later(&self.state.watch_later)?;
            }
        }
        Ok(())
    }

    fn switch_tab(&mut self) {
        self.state.current_tab = match self.state.current_tab {
            Tab::Feed => Tab::WatchLater,
            Tab::WatchLater => Tab::Feed,
        };
        self.state.scroll_offset = 0;
        self.state.selected_index = None;
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
                    self.terminal.draw(|f| {
                        ui::render(f, &self.state, &self.layout, &mut self.thumb_cache);
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
