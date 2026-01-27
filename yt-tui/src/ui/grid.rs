use std::ops::Add;
use std::path::Path;

use crate::cache::ThumbnailCache;
use crate::data::{AppState, Tab, Video};
use crate::gemini::SummaryState;
use crate::ui::GridLayout;
use ansi_to_tui::IntoText;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use termimad::{MadSkin, StyledChar};

use super::scrollbar::{SmoothScrollbar, SmoothScrollbarState};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

/// Render the entire UI
pub fn render(
    frame: &mut Frame,
    state: &AppState,
    layout: &GridLayout,
    thumb_cache: &ThumbnailCache,
    scroll_state: &mut ScrollViewState,
    scroll_position: f64,
    videos_dir: &Path,
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
}

/// A 3-line box button (top border, content, bottom border)
struct BoxButton {
    top: String,
    middle: String,
    bottom: String,
}

impl BoxButton {
    fn new(label: &str) -> Self {
        let width = label.chars().count();
        Self {
            top: format!("╭{}╮", "─".repeat(width)),
            middle: format!("│{}│", label),
            bottom: format!("╰{}╯", "─".repeat(width)),
        }
    }

    fn new_tab(label: &str, is_selected: bool) -> Self {
        let width = label.chars().count();
        let bottom = if is_selected {
            format!("╯{}╰", " ".repeat(width))
        } else {
            format!("─{}─", "─".repeat(width))
        };
        Self {
            top: format!("╭{}╮", "─".repeat(width)),
            middle: format!("│{}│", label),
            bottom,
        }
    }

    fn width(&self) -> usize {
        self.top.chars().count()
    }
}

fn render_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let badges: [&str;_] = [
        "⓿", "➊", "➋", "➌", "➍", "➎", "➏", "➐", "➑", "➒", "➓", "⓫", "⓬", "⓭", "⓮", "⓯", "⓰", "⓱",
        "⓲", "⓳", "⓴"
    ];

    let watch_later_count = state.videos.iter()
        .filter(|v| state.watch_later.contains(&v.video_id))
        .count();
    let watch_later_badge = badges[watch_later_count.min(badges.len() - 1)];
    let mut watch_later_label = " Watch Later ".to_string();
    watch_later_label.push_str(watch_later_badge);
    if watch_later_label.len() > 20 {
        watch_later_label.push_str("✚");
    } else {
        watch_later_label.push_str("  ");
    }
    if watch_later_count == 50 {
        watch_later_label = " Watch Later 🅛 ".to_string();
    }
    if watch_later_count > 50 {
        watch_later_label = " Watch Later 🅛✚".to_string();
    }
    if watch_later_count == 100 {
        watch_later_label = " Watch Later 🅒 ".to_string();
    }
    if watch_later_count > 100 {
        watch_later_label = " Watch Later 🅒✚".to_string();
    }
    if watch_later_count >= 200 {
        watch_later_label = " Watch Later ∞ ".to_string();
    }

    let feed_label = " Feed ↻ ";
    let tabs: &[(&str, Tab)] = &[(feed_label, Tab::Feed), (&watch_later_label, Tab::WatchLater)];

    let selected_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let unselected_style = Style::default().fg(Color::DarkGray);
    let badge_style = Style::default().fg(Color::Yellow);
    let help_style = Style::default().fg(Color::Cyan);
    let refresh_style = if state.is_refreshing {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::RAPID_BLINK)
    } else {
        help_style
    };

    // Build buttons
    let help_btn = BoxButton::new(" ? ");
    let tab_btns: Vec<(BoxButton, bool)> = tabs
        .iter()
        .map(|(label, tab)| {
            let is_selected = state.current_tab == *tab;
            (BoxButton::new_tab(label, is_selected), is_selected)
        })
        .collect();

    // Calculate spacing
    let tabs_width: usize =
        tab_btns.iter().map(|(b, _)| b.width()).sum::<usize>() + tab_btns.len().saturating_sub(1);
    let right_width = help_btn.width();
    let spacing = (area.width as usize).saturating_sub(tabs_width + right_width);

    // Build all three lines using a helper closure. The third parameter
    // controls whether unselected tabs should use white for this line
    // (used to make unselected tab bottoms white).
    let build_line = |get_part: fn(&BoxButton) -> &str,
                      separator: &str,
                      unselected_use_white: bool|
     -> Vec<Span> {
        let mut spans = Vec::new();
        for (i, (btn, is_selected)) in tab_btns.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(separator.to_string()));
            }
            let style = if *is_selected {
                selected_style
            } else if unselected_use_white {
                selected_style
            } else {
                unselected_style
            };
            spans.push(Span::styled(get_part(btn).to_string(), style));
        }
        spans.push(Span::raw(separator.repeat(spacing)));
        spans.push(Span::styled(get_part(&help_btn).to_string(), help_style));
        spans
    };

    let line1 = build_line(|b| &b.top, " ", false);
    // Build line2 separately to style the Watch Later badge differently
    let line2 = {
        let mut spans = Vec::new();
        for (i, (btn, is_selected)) in tab_btns.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" ".to_string()));
            }
            let style = if *is_selected {
                selected_style
            } else {
                unselected_style
            };
            // Feed tab (index 0) - split to style refresh icon separately
            if i == 0 {
                // btn.middle is "│ Feed ↻ │" - split before the refresh icon
                let middle = &btn.middle;
                let refresh_start = "│ Feed ".chars().count();
                let refresh_end = refresh_start + 1; // ↻ is one char
                let before_refresh: String = middle.chars().take(refresh_start).collect();
                let refresh_char: String = middle
                    .chars()
                    .skip(refresh_start)
                    .take(1)
                    .collect();
                let after_refresh: String = middle.chars().skip(refresh_end).collect();
                spans.push(Span::styled(before_refresh, style));
                spans.push(Span::styled(refresh_char, refresh_style));
                spans.push(Span::styled(after_refresh, style));
            // Watch Later tab (index 1) - split to style badge separately
            } else if i == 1 {
                // btn.middle is "│ Watch Later ❶ │" - split before the badge
                let middle = &btn.middle;
                // Find position just before the badge (after " Watch Later ")
                let badge_start = "│ Watch Later ".chars().count();
                let badge_end = badge_start + watch_later_badge.chars().count();
                let before_badge: String = middle.chars().take(badge_start).collect();
                let badge_char: String = middle
                    .chars()
                    .skip(badge_start)
                    .take(watch_later_badge.chars().count())
                    .collect();
                let after_badge: String = middle.chars().skip(badge_end).collect();
                spans.push(Span::styled(before_badge, style));
                spans.push(Span::styled(badge_char, badge_style));
                spans.push(Span::styled(after_badge, style));
            } else {
                spans.push(Span::styled(btn.middle.clone(), style));
            }
        }
        spans.push(Span::raw(" ".repeat(spacing)));
        spans.push(Span::styled(help_btn.middle.clone(), help_style));
        spans
    };
    let line3 = build_line(|b| &b.bottom, "─", true);

    for (y, spans) in [line1, line2, line3].into_iter().enumerate() {
        let rect = Rect {
            x: 0,
            y: y as u16,
            width: area.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    }
}

/// Returns the click regions for header tabs as (start_col, end_col, Tab)
/// and optionally a refresh icon region
/// This is used by the click handler in app.rs
pub fn header_tab_regions() -> (Vec<(u16, u16, Tab)>, Option<(u16, u16)>) {
    let feed_label = " Feed ↻ ";
    let tabs: &[(&str, Tab)] = &[(feed_label, Tab::Feed), (" Watch Later ", Tab::WatchLater)];

    let mut regions = Vec::new();
    let mut refresh_region = None;
    let mut x: u16 = 0;
    for (i, (label, tab)) in tabs.iter().enumerate() {
        let width = label.chars().count() as u16 + 2; // +2 for borders
        regions.push((x, x + width, *tab));

        // Feed tab (index 0) - calculate refresh icon position
        // The icon is at position: border(1) + " Feed "(6) = 7 chars from start
        if i == 0 {
            let refresh_col = x + 1 + " Feed ".chars().count() as u16; // +1 for left border
            refresh_region = Some((refresh_col, refresh_col + 2)); // icon + space after
        }

        x += width + 1; // +1 for space between tabs
    }
    (regions, refresh_region)
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
) {
    // Grid area (between header and footer)
    let grid_area = Rect {
        x: 0,
        y: GridLayout::HEADER_HEIGHT,
        width: area.width,
        height: layout.grid_height,
    };

    // Calculate total content height
    let total_rows = videos.len().div_ceil(layout.cols);
    let content_height = (total_rows as u16 * layout.card_height).max(layout.grid_height);

    // Create scroll view with full content size (disable built-in scrollbars)
    let mut scroll_view = ScrollView::new(Size::new(area.width, content_height))
        .horizontal_scrollbar_visibility(ScrollbarVisibility::Never)
        .vertical_scrollbar_visibility(ScrollbarVisibility::Never);

    //render colored bakckground for grid area
    let background_block = Block::default().style(Style::default().bg(Color::Rgb(20, 20, 20)));
    let background_area = Rect {
        x: 0,
        y: 0,
        width: area.width,
        height: content_height,
    };
    background_block.render(background_area, scroll_view.buf_mut());

    //top shadow
    Paragraph::new("▀".repeat(area.width as usize))
        .style(Style::default().fg(Color::Rgb(4, 4, 4)))
        .bg(Color::Rgb(8, 8, 8))
        .render(
            Rect {
                x: 0,
                y: scroll_state.offset().y,
                width: area.width,
                height: 1,
            },
            scroll_view.buf_mut(),
        );
    Paragraph::new("▀".repeat(area.width as usize))
        .style(Style::default().fg(Color::Rgb(12, 12, 12)))
        .bg(Color::Rgb(16, 16, 16))
        .render(
            Rect {
                x: 0,
                y: scroll_state.offset().y + 1,
                width: area.width,
                height: 1,
            },
            scroll_view.buf_mut(),
        );

    // Calculate which rows are visible for performance (don't render off-screen cards)
    let scroll_offset = scroll_state.offset().y as usize;
    let first_visible_row = scroll_offset / layout.card_height as usize;
    let last_visible_row =
        (scroll_offset + layout.grid_height as usize) / layout.card_height as usize + 1;
    let first_video = first_visible_row * layout.cols;
    let last_video = ((last_visible_row + 1) * layout.cols).min(videos.len());

    let mut vec_items: Vec<usize> = (first_video..last_video).collect();
    if state.selected_index.is_some() {
        // Ensure selected index is rendered last (on top)
        let index = vec_items.iter().position(|&idx| Some(idx) == state.selected_index);
        if let Some(idx) = index {
            let selected = vec_items.remove(idx);
            vec_items.insert(0, selected);
        }
    }

    // Render cards into scroll view at their natural positions
    for idx in vec_items.into_iter().rev() {
        if let Some(video) = videos.get(idx) {
            let row = idx / layout.cols;
            let col = idx % layout.cols;

            let card_area = Rect {
                x: layout.x_offset + col as u16 * layout.card_width,
                y: row as u16 * (layout.card_height - 1),
                width: layout.card_width,
                height: layout.card_height,
            };

            let is_selected = state.selected_index == Some(idx);

            let is_watch_later = state.watch_later.contains(&video.video_id);
            let is_downloaded = videos_dir
                .join(format!("*_{}.mp4", video.video_id))
                .to_str()
                .and_then(|p| glob::glob(p).ok())
                .map(|mut g| g.next().is_some())
                .unwrap_or(false);

            render_video_card(
                scroll_view.buf_mut(),
                video,
                card_area,
                is_selected,
                is_watch_later,
                is_downloaded,
                layout,
                thumb_cache,
            );
        }
    }

    // Render the scroll view to the frame
    frame.render_stateful_widget(scroll_view, grid_area, scroll_state);

    // Render smooth scrollbar with sub-cell precision
    let scrollbar = SmoothScrollbar::new()
        .thumb_color(Color::Cyan)
        .track_color(Color::Rgb(10, 10, 10));

    let mut scrollbar_state = SmoothScrollbarState::new(
        content_height as f64,
        layout.grid_height as f64,
    )
    .position(scroll_position);

    frame.render_stateful_widget(scrollbar, grid_area, &mut scrollbar_state);
}

fn render_video_card(
    buf: &mut Buffer,
    video: &Video,
    area: Rect,
    is_selected: bool,
    is_watch_later: bool,
    is_downloaded: bool,
    layout: &GridLayout,
    thumb_cache: &ThumbnailCache,
) {
    // Card border
    let border_style = if is_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Black)
    };

    let border_set = if is_selected {
        BorderType::Rounded.to_border_set()
    } else {
        ratatui_core::symbols::border::Set {
            vertical_left: "⢸",
            vertical_right: "🮐",
            horizontal_top: "⣀",
            horizontal_bottom: "🮎", //"▀",//"⠻",
            top_left: " ",
            top_right: "⣀",
            bottom_left: "⠘",
            bottom_right: "⠛",
        }
    };

    let block = Block::default()
        .borders(Borders::ALL)
        //.border_type(border_type)
        .border_style(border_style)
        .border_set(border_set);

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Thumbnail area
    let thumb_width = layout.thumbnail_width();
    let thumb_height = layout.thumbnail_height();

    let mut fancy_bg: Vec<(u8, u8, u8)> = Vec::new();

    if let Some(rendered) = thumb_cache.get_rendered(&video.video_id, thumb_width, thumb_height) {
        let thumb_area = Rect {
            x: inner.x,
            y: inner.y,
            width: thumb_width.min(inner.width),
            height: thumb_height.min(inner.height),
        };

        let image = rendered.0;

        fancy_bg = rendered.1;

        if let Ok(text) = image.into_text() {
            Paragraph::new(text).render(thumb_area, buf);
        }
    } else {
        // Queue background render and show placeholder
        thumb_cache.queue_render(&video.video_id, thumb_width, thumb_height);

        let thumb_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        Paragraph::new("[Loading...]")
            .style(Style::default().fg(Color::DarkGray))
            .render(thumb_area, buf);
    }

    // Text area below thumbnail
    let text_area = Rect {
        x: inner.x,
        y: inner.y + thumb_height.saturating_sub(2),
        width: inner.width,
        height: inner.height.saturating_sub(thumb_height).add(2),
    };

    if text_area.height > 0 {
        // Title (always exactly 2 lines)
        let (title_line1, title_line2) =
            wrap_title_two_lines(&video.title, inner.width.saturating_sub(2) as usize);

        let title_line1 = format!(" {} ", title_line1);
        let title_line2 = format!(" {} ", title_line2);

        let channel = truncate_str(&video.channel_name, inner.width.saturating_sub(2) as usize);
        let time_ago = format_time_ago(&video.published);

        // Calculate padding to right-align timestamp
        let channel_len = channel.chars().count();
        let time_len = time_ago.chars().count();
        let padding =
            (inner.width.saturating_sub(2) as usize).saturating_sub(channel_len + time_len);

        let mut channel_and_time_style = String::new();
        channel_and_time_style.push_str(&" ");
        channel_and_time_style.push_str(&channel);
        channel_and_time_style.push_str(&" ".repeat(padding));
        channel_and_time_style.push_str(&time_ago);
        channel_and_time_style.push_str(&" ");

        let checkbox_line = if is_watch_later {
            format!("{:>width$}", "✨ 🗁  ⊂⬤ ", width = thumb_width as usize)
        } else {
            format!("{:>width$}", "✨ 🗁  ⬤⊃ ", width = thumb_width as usize)
        };

        // Colors for folder (based on download status) and toggle (based on watch later status)
        let folder_color = if is_downloaded {
            Color::Rgb(255, 165, 0) // Orange when downloaded
        } else {
            Color::Rgb(128, 128, 128) // Grey when not downloaded
        };
        let toggle_color = if is_watch_later {
            Color::Rgb(255, 165, 0) // Orange when watch later
        } else {
            Color::Rgb(128, 128, 128) // Grey when not watch later
        };

        let text_lines = vec![
            //Line::from(" "),
            fancy_bg
                .iter()
                .take(thumb_width as usize)
                .zip(checkbox_line.chars())
                .map(|((r, g, b), char)| {
                    // Folder icon gets folder_color, toggle chars get toggle_color, ✨ gets cyan
                    let fg_color = if char == '🗁' {
                        folder_color
                    } else if char == '⊂' || char == '⬤' || char == '⊃' {
                        toggle_color
                    } else if char == '✨' {
                        Color::Cyan
                    } else {
                        Color::Rgb(128, 128, 128) // Spaces stay grey
                    };
                    Span::styled(
                        char.to_string(),
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Rgb(*r, *g, *b))
                            .fg(fg_color),
                    )
                })
                .collect(),
            fancy_bg
                .iter()
                .skip(thumb_width as usize)
                .take(thumb_width as usize)
                .zip(title_line1.chars())
                .map(|((r, g, b), char)| {
                    Span::styled(
                        char.to_string(),
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Rgb(*r, *g, *b)),
                    )
                })
                .collect(),
            fancy_bg
                .iter()
                .skip(thumb_width as usize * 2)
                .take(thumb_width as usize)
                .zip(title_line2.chars())
                .map(|((r, g, b), char)| {
                    Span::styled(
                        char.to_string(),
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::Rgb(*r, *g, *b)),
                    )
                })
                .collect(),
            fancy_bg
                .iter()
                .skip(thumb_width as usize * 3)
                .take(thumb_width as usize)
                .zip(channel_and_time_style.chars())
                .map(|((r, g, b), char)| {
                    Span::styled(
                        char.to_string(),
                        Style::default().bg(Color::Rgb(*r, *g, *b)),
                    )
                })
                .collect(),
            //Line::from(Span::styled(title_line1, title_style)),
            //Line::from(Span::styled(title_line2, title_style)),
            //Line::from(vec![
            //    Span::styled(channel, Style::default().fg(Color::Gray)),
            //    Span::raw(" ".repeat(padding)),
            //    Span::styled(time_ago, Style::default().fg(Color::DarkGray)),
            //]),
        ];

        Paragraph::new(text_lines).render(text_area, buf);
    }
}

fn render_footer(frame: &mut Frame, state: &AppState, videos: &[&Video], area: Rect) {
    let footer_area = Rect {
        x: 0,
        y: area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };

    let status = if let Some(msg) = &state.status_message {
        msg.clone()
    } else {
        let refresh_info = state
            .last_refresh
            .map(|t| format!(" | Last refresh: {}", format_time_ago(&t)))
            .unwrap_or_default();

        format!(" {} videos{} ", videos.len(), refresh_info)
    };

    let footer =
        Paragraph::new(status).style(Style::default().fg(Color::LightCyan).bg(Color::Black));

    frame.render_widget(footer, footer_area);
}

fn render_help(frame: &mut Frame, area: Rect) {
    let help_width = 50.min(area.width.saturating_sub(4));
    let help_height = 15.min(area.height.saturating_sub(4));

    let help_area = Rect {
        x: (area.width - help_width) / 2,
        y: (area.height - help_height) / 2,
        width: help_width,
        height: help_height,
    };

    let help_text = vec![
        Line::from(Span::styled(
            "Keyboard Shortcuts",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::raw("↑/k, ↓/j     Move selection up/down"),
        Line::raw("←/h, →/l     Move selection left/right"),
        Line::raw("Enter        Play selected video"),
        Line::raw("w            Toggle Watch Later"),
        Line::raw("s            Summarize video (Gemini)"),
        Line::raw("Tab          Switch tabs"),
        Line::raw("r            Refresh feeds"),
        Line::raw("?            Toggle this help"),
        Line::raw("q/Esc        Quit"),
        Line::raw(""),
        Line::raw("Mouse: Click to select/play, scroll to navigate"),
    ];

    // Clear the area first so content behind doesn't show through
    frame.render_widget(Clear, help_area);

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .border_type(BorderType::Rounded)
        .style(Style::default().bg(Color::Black));

    let help = Paragraph::new(help_text).block(block);
    frame.render_widget(help, help_area);
}

fn render_summary(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal_width = 70.min(area.width.saturating_sub(4));
    let modal_height = (area.height - 6).min(area.height.saturating_sub(4));

    let modal_area = Rect {
        x: (area.width - modal_width) / 2,
        y: (area.height - modal_height) / 2,
        width: modal_width,
        height: modal_height,
    };

    // Clear the area first
    frame.render_widget(Clear, modal_area);

    let title = state
        .summary_video_title
        .as_ref()
        .map(|t| format!(" ✨ {} ", truncate_str(t, modal_width.saturating_sub(6) as usize)))
        .unwrap_or_else(|| " ✨ Summary ".to_string());

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .border_type(BorderType::Rounded)
        .style(Style::default().bg(Color::Black).add_modifier(Modifier::BOLD));

    let inner = block.inner(modal_area);

    match &state.summary_state {
        Some(SummaryState::Loading) => {
            let loading_text = vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "Loading summary...",
                    Style::default().fg(Color::Yellow),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "Asking Gemini to analyze the video",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let paragraph = Paragraph::new(loading_text)
                .block(block)
                .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(paragraph, modal_area);
        }
        Some(SummaryState::Ready(summary)) => {
            // Render the block first
            frame.render_widget(block, modal_area);

            // Use termimad to format markdown with ANSI codes
            let mut skin = MadSkin::default_dark();

            // Polished unicode typography
            skin.bullet = StyledChar::from_fg_char(
                termimad::crossterm::style::Color::White,
                '•',
            );
            skin.quote_mark = StyledChar::from_fg_char(
                termimad::crossterm::style::Color::Grey,
                '▌'
            );

            let mut header_style = termimad::LineStyle::default();
            header_style.add_attr(crossterm::style::Attribute::Bold);
            skin.headers = [
               header_style.clone(),
               header_style.clone(),
               header_style.clone(),
               header_style.clone(),
               header_style.clone(),
               header_style.clone(),
               header_style.clone(),
               header_style.clone(),
            ];

            skin.list_items_indentation_mode = termimad::ListItemsIndentationMode::Block;

            let text_width = inner.width.saturating_sub(3) as usize;
            let formatted = skin.text(summary, Some(text_width));
            let ansi_string = format!("{}", formatted);

            // Convert ANSI to ratatui Text
            let text: Text = ansi_string.into_text().unwrap_or_else(|_| Text::raw(summary));

            // Add left padding to each line, plus empty lines at top and bottom
            let mut text_lines: Vec<Line> = vec![Line::raw("")]; // top padding
            text_lines.extend(text.lines.into_iter().map(|line| {
                let mut spans = vec![Span::raw(" ")];
                spans.extend(line.spans);
                Line::from(spans)
            }));
            text_lines.push(Line::raw("")); // bottom padding

            let total_lines = text_lines.len() as u16;
            let viewport_height = inner.height;

            // Calculate content height for scroll view
            let content_height = total_lines.max(viewport_height);

            // Calculate max scroll and clamp current scroll
            let max_scroll = total_lines.saturating_sub(viewport_height);
            let scroll_offset = (state.summary_scroll).min(max_scroll);

            // Create scroll view
            let mut scroll_view = ScrollView::new(Size::new(inner.width.saturating_sub(1), content_height))
                .horizontal_scrollbar_visibility(ScrollbarVisibility::Never)
                .vertical_scrollbar_visibility(ScrollbarVisibility::Never);

            let text = text_lines;

            Paragraph::new(text)
                .style(Style::default().bg(Color::Black))
                .render(
                    Rect {
                        x: 0,
                        y: 0,
                        width: inner.width.saturating_sub(1),
                        height: content_height,
                    },
                    scroll_view.buf_mut(),
                );

            // Create scroll state with current offset
            let mut scroll_state = ScrollViewState::default();
            scroll_state.set_offset(ratatui::layout::Position::new(0, scroll_offset));

            // Render the scroll view
            frame.render_stateful_widget(scroll_view, inner, &mut scroll_state);

            // Render smooth scrollbar if content is scrollable
            if total_lines > viewport_height {
                let scrollbar_area = Rect {
                    x: inner.x + inner.width.saturating_sub(1),
                    y: inner.y,
                    width: 1,
                    height: inner.height,
                };

                let scrollbar = SmoothScrollbar::new()
                    .thumb_color(Color::Cyan)
                    .track_color(Color::Rgb(30, 30, 30));

                let mut scrollbar_state = SmoothScrollbarState::new(
                    content_height as f64,
                    viewport_height as f64,
                )
                .position(scroll_offset as f64);

                frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);

                // Show hint at bottom
                let hint = " ↑/↓ scroll, Esc close ";
                let hint_area = Rect {
                    x: modal_area.x + 2,
                    y: modal_area.y + modal_area.height - 1,
                    width: hint.len() as u16,
                    height: 1,
                };
                frame.render_widget(
                    Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
                    hint_area,
                );
            }
        }
        Some(SummaryState::Error(err)) => {
            let error_text = vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "Error loading summary",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::raw(""),
                Line::from(Span::styled(err.clone(), Style::default().fg(Color::Red))),
                Line::raw(""),
                Line::from(Span::styled(
                    "Press Esc to close",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let paragraph = Paragraph::new(error_text)
                .block(block)
                .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(paragraph, modal_area);
        }
        None => {
            frame.render_widget(block, modal_area);
        }
    }
}

/// Wrap text to fit within a given width
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars()
                .take(max_len.saturating_sub(1))
                .collect::<String>()
        )
    }
}

/// Wrap text to exactly 2 lines, truncating with ellipsis if needed
fn wrap_title_two_lines(s: &str, line_width: usize) -> (String, String) {
    let chars: Vec<char> = s.chars().collect();

    if chars.len() <= line_width {
        // Fits in one line - use em dash on second line
        return (
            //s.to_string(),
            format!("{:<width$}", s, width = line_width),
            //"—".to_string()
            format!("{:<width$}", "—", width = line_width),
        );
    }

    // Find a good break point for first line (prefer breaking at space)
    let mut break_at = line_width;
    for i in (0..line_width).rev() {
        if chars[i] == ' ' {
            break_at = i;
            break;
        }
    }

    let line1: String = chars[..break_at].iter().collect();
    let remaining: String = chars[break_at..].iter().collect();
    let remaining = remaining.trim_start();

    // Second line - truncate if needed
    let line2 = if remaining.chars().count() <= line_width {
        remaining.to_string()
    } else {
        format!(
            "{}…",
            remaining
                .chars()
                .take(line_width.saturating_sub(1))
                .collect::<String>()
        )
    };
    // pad lines to line_width
    let line1 = format!("{:<width$}", line1, width = line_width);
    let line2 = format!("{:<width$}", line2, width = line_width);

    (line1, line2)
}

fn format_time_ago(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(*dt);

    if duration.num_days() > 365 {
        format!("{}y ago", duration.num_days() / 365)
    } else if duration.num_days() > 30 {
        format!("{}mo ago", duration.num_days() / 30)
    } else if duration.num_days() > 0 {
        format!("{}d ago", duration.num_days())
    } else if duration.num_hours() > 0 {
        format!("{}h ago", duration.num_hours())
    } else if duration.num_minutes() > 0 {
        format!("{}m ago", duration.num_minutes())
    } else {
        "just now".to_string()
    }
}
