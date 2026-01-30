use std::path::Path;

use crate::cache::ThumbnailCache;
use crate::data::{AppState, Video};
use crate::ui::GridLayout;
use ratatui::prelude::*;
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use super::card::render_video_card;
use super::footer::render_footer;
use super::header::render_header;
use super::modals::{render_help, render_summary, render_transcript};
use super::scrollbar::{SmoothScrollbar, SmoothScrollbarState};
use super::selection::SelectionIndicator;

/// Render the entire UI
pub fn render(
    frame: &mut Frame,
    state: &AppState,
    layout: &GridLayout,
    thumb_cache: &ThumbnailCache,
    scroll_state: &mut ScrollViewState,
    scroll_position: f64,
    videos_dir: &Path,
    selection_indicator: &SelectionIndicator,
) {
    let area = frame.area();

    // Render header (tabs)
    render_header(frame, state, area);

    // Render video grid
    let videos = state.current_videos();
    render_grid(
        frame,
        &videos,
        state,
        layout,
        thumb_cache,
        area,
        scroll_state,
        scroll_position,
        videos_dir,
        selection_indicator,
    );

    // Render footer (status bar)
    render_footer(frame, state, &videos, area);

    // Render help overlay if active
    if state.show_help {
        render_help(frame, area);
    }

    // Render summary modal if active
    if state.show_summary {
        render_summary(frame, state, area);
    }

    // Render transcript modal if active
    if state.show_transcript {
        render_transcript(frame, state, area);
    }
}

fn render_grid(
    frame: &mut Frame,
    videos: &[&Video],
    state: &AppState,
    layout: &GridLayout,
    thumb_cache: &ThumbnailCache,
    area: Rect,
    scroll_state: &mut ScrollViewState,
    scroll_position: f64,
    videos_dir: &Path,
    selection_indicator: &SelectionIndicator,
) {
    // Grid area (between header and footer)
    let grid_area = Rect {
        x: 0,
        y: GridLayout::HEADER_HEIGHT,
        width: area.width,
        height: layout.grid_height,
    };

    // Calculate total content height (includes top padding for selection indicator)
    let total_rows = videos.len().div_ceil(layout.cols);
    let content_height = (GridLayout::CONTENT_TOP_PADDING
        + total_rows as u16 * layout.card_stride())
    .max(layout.grid_height);

    // Create scroll view with full content size (disable built-in scrollbars)
    let mut scroll_view = ScrollView::new(Size::new(area.width, content_height))
        .horizontal_scrollbar_visibility(ScrollbarVisibility::Never)
        .vertical_scrollbar_visibility(ScrollbarVisibility::Never);

    // Calculate which rows are visible for performance (don't render off-screen cards)
    let scroll_offset = scroll_state.offset().y as usize;
    let stride = layout.card_stride() as usize;
    let top_padding = GridLayout::CONTENT_TOP_PADDING as usize;
    let content_offset = scroll_offset.saturating_sub(top_padding);
    let first_visible_row = content_offset / stride;
    let last_visible_row = (content_offset + layout.grid_height as usize) / stride + 1;
    let first_video = first_visible_row * layout.cols;
    let last_video = ((last_visible_row + 1) * layout.cols).min(videos.len());

    // Render cards into scroll view at their natural positions
    // No need to sort - selection indicator is rendered separately on top
    for idx in first_video..last_video {
        if let Some(video) = videos.get(idx) {
            let (card_x, card_y) = layout.card_rect(idx);

            let card_area = Rect {
                x: card_x,
                y: card_y,
                width: layout.card_width,
                height: layout.card_height,
            };

            let is_watch_later = state.watch_later.contains(&video.video_id);
            let is_downloaded = videos_dir
                .join(format!("*_{}.mp4", video.video_id))
                .to_str()
                .and_then(|p| glob::glob(p).ok())
                .map(|mut g| g.next().is_some())
                .unwrap_or(false);
            let has_transcript = video.transcript.is_some();

            render_video_card(
                scroll_view.buf_mut(),
                video,
                card_area,
                is_watch_later,
                is_downloaded,
                has_transcript,
                layout,
                thumb_cache,
            );
        }
    }

    // Render selection indicator on top of all cards
    selection_indicator.render(scroll_view.buf_mut(), layout);

    // Render the scroll view to the frame
    frame.render_stateful_widget(scroll_view, grid_area, scroll_state);

    // Render smooth scrollbar with sub-cell precision
    let scrollbar = SmoothScrollbar::new()
        .thumb_color(Color::Cyan)
        .track_color(Color::Rgb(10, 10, 10));

    let mut scrollbar_state =
        SmoothScrollbarState::new(content_height as f64, layout.grid_height as f64)
            .position(scroll_position);

    frame.render_stateful_widget(scrollbar, grid_area, &mut scrollbar_state);
}
