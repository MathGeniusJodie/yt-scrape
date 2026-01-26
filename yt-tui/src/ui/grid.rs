use crate::cache::ThumbnailCache;
use crate::data::{AppState, Tab, Video};
use crate::ui::GridLayout;
use ansi_to_tui::IntoText;
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

/// Render the entire UI
pub fn render(
    frame: &mut Frame,
    state: &AppState,
    layout: &GridLayout,
    thumb_cache: &mut ThumbnailCache,
    scroll_state: &mut ScrollViewState,
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
    );

    // Render footer (status bar)
    render_footer(frame, state, &videos, area);

    // Render help overlay if active
    if state.show_help {
        render_help(frame, area);
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
    let tabs: &[(&str, Tab)] = &[(" Feed ", Tab::Feed), (" Watch Later ", Tab::WatchLater)];

    let selected_style = Style::default().fg(Color::White);
    let unselected_style = Style::default().fg(Color::DarkGray);
    let help_style = Style::default().fg(Color::Cyan);
    let refresh_style = if state.is_refreshing {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::RAPID_BLINK)
    } else {
        help_style
    };

    // Build buttons
    let refresh_btn = BoxButton::new(" ↻ ");
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
    let right_width = refresh_btn.width() + 1 + help_btn.width();
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
        spans.push(Span::styled(
            get_part(&refresh_btn).to_string(),
            refresh_style,
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(get_part(&help_btn).to_string(), help_style));
        spans
    };

    let line1 = build_line(|b| &b.top, " ", false);
    let line2 = build_line(|b| &b.middle, " ", false);
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
/// This is used by the click handler in app.rs
pub fn header_tab_regions() -> Vec<(u16, u16, Tab)> {
    let tabs: &[(&str, Tab)] = &[(" Feed ", Tab::Feed), (" Watch Later ", Tab::WatchLater)];

    let mut regions = Vec::new();
    let mut x: u16 = 0;
    for (label, tab) in tabs {
        let width = label.chars().count() as u16 + 2; // +2 for borders
        regions.push((x, x + width, *tab));
        x += width + 1; // +1 for space between tabs
    }
    regions
}

fn render_grid(
    frame: &mut Frame,
    videos: &[&Video],
    state: &AppState,
    layout: &GridLayout,
    thumb_cache: &mut ThumbnailCache,
    area: Rect,
    scroll_state: &mut ScrollViewState,
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

    // Calculate which rows are visible for performance (don't render off-screen cards)
    let scroll_offset = scroll_state.offset().y as usize;
    let first_visible_row = scroll_offset / layout.card_height as usize;
    let last_visible_row =
        (scroll_offset + layout.grid_height as usize) / layout.card_height as usize + 1;
    let first_video = first_visible_row * layout.cols;
    let last_video = ((last_visible_row + 1) * layout.cols).min(videos.len());

    // Render cards into scroll view at their natural positions
    for idx in first_video..last_video {
        if let Some(video) = videos.get(idx) {
            let row = idx / layout.cols;
            let col = idx % layout.cols;

            let card_area = Rect {
                x: col as u16 * layout.card_width,
                y: row as u16 * layout.card_height,
                width: layout.card_width,
                height: layout.card_height,
            };

            let is_selected = state.selected_index == Some(idx);
            let is_watch_later = state.watch_later.contains(&video.video_id);

            render_video_card(
                scroll_view.buf_mut(),
                video,
                card_area,
                is_selected,
                is_watch_later,
                layout,
                thumb_cache,
            );
        }
    }

    // Render the scroll view to the frame
    frame.render_stateful_widget(scroll_view, grid_area, scroll_state);

    // Render custom scrollbar (cyan, no arrows, no track)
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(None)
        .thumb_style(Style::default().fg(Color::Cyan));

    let mut scrollbar_state = ScrollbarState::new(content_height as usize).position(scroll_offset);

    frame.render_stateful_widget(scrollbar, grid_area, &mut scrollbar_state);
}

fn render_video_card(
    buf: &mut Buffer,
    video: &Video,
    area: Rect,
    is_selected: bool,
    is_watch_later: bool,
    layout: &GridLayout,
    thumb_cache: &mut ThumbnailCache,
) {
    // Card border
    let border_style = if is_selected {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Thumbnail area
    let thumb_width = layout.thumbnail_width();
    let thumb_height = layout.thumbnail_height();

    if let Some(rendered) = thumb_cache.get_rendered(&video.video_id, thumb_width, thumb_height) {
        let thumb_area = Rect {
            x: inner.x,
            y: inner.y,
            width: thumb_width.min(inner.width),
            height: thumb_height.min(inner.height),
        };

        if let Ok(text) = rendered.into_text() {
            Paragraph::new(text).render(thumb_area, buf);
        }
    } else {
        // Placeholder while loading
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
        y: inner.y + thumb_height,
        width: inner.width,
        height: inner.height.saturating_sub(thumb_height),
    };

    if text_area.height > 0 {
        // Title (always exactly 2 lines)
        let (title_line1, title_line2) = wrap_title_two_lines(&video.title, inner.width as usize);
        let channel = truncate_str(&video.channel_name, inner.width as usize);
        let time_ago = format_time_ago(&video.published);

        let title_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);

        // Calculate padding to right-align timestamp
        let channel_len = channel.chars().count();
        let time_len = time_ago.chars().count();
        let padding = (inner.width as usize).saturating_sub(channel_len + time_len);

        let text_lines = vec![
            Line::from(" "),
            Line::from(Span::styled(title_line1, title_style)),
            Line::from(Span::styled(title_line2, title_style)),
            Line::from(vec![
                Span::styled(channel, Style::default().fg(Color::Gray)),
                Span::raw(" ".repeat(padding)),
                Span::styled(time_ago, Style::default().fg(Color::DarkGray)),
            ]),
        ];

        Paragraph::new(text_lines).render(text_area, buf);
    }

    // Render watch later checkbox on bottom border
    let (checkbox_text, checkbox_style) = if is_watch_later {
        (" W:☑ ", Style::default().fg(Color::Rgb(255, 165, 0)))
    } else {
        (" W:☐ ", Style::default().fg(Color::Rgb(128, 128, 128)))
    };

    let checkbox_len = checkbox_text.chars().count() as u16;
    let checkbox_x = area.x + area.width.saturating_sub(checkbox_len + 2);
    let checkbox_area = Rect {
        x: checkbox_x,
        y: area.y + area.height - 1,
        width: checkbox_len,
        height: 1,
    };

    Paragraph::new(checkbox_text)
        .style(checkbox_style)
        .render(checkbox_area, buf);
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

        format!(
            " {} videos{} | ↑↓ scroll, ⏎ play, w watch later, r refresh, ? help, q quit",
            videos.len(),
            refresh_info
        )
    };

    let footer = Paragraph::new(status).style(
        Style::default()
            .fg(Color::LightCyan)
            .bg(Color::Rgb(20, 20, 20)),
    );

    frame.render_widget(footer, footer_area);
}

fn render_help(frame: &mut Frame, area: Rect) {
    let help_width = 50.min(area.width.saturating_sub(4));
    let help_height = 14.min(area.height.saturating_sub(4));

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
        return (s.to_string(), "—".to_string());
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
