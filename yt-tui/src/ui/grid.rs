use ansi_to_tui::IntoText;
use crate::cache::ThumbnailCache;
use crate::data::{AppState, Tab, Video};
use crate::ui::GridLayout;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Render the entire UI
pub fn render(
    frame: &mut Frame,
    state: &AppState,
    layout: &GridLayout,
    thumb_cache: &mut ThumbnailCache,
) {
    let area = frame.area();

    // Render header (tabs)
    render_header(frame, state, area);

    // Render video grid
    let videos = state.current_videos();
    render_grid(frame, &videos, state, layout, thumb_cache);

    // Render footer (status bar)
    render_footer(frame, state, &videos, area);

    // Render help overlay if active
    if state.show_help {
        render_help(frame, area);
    }
}

fn render_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let header_area = Rect {
        x: 0,
        y: 0,
        width: area.width,
        height: 1,
    };

    let feed_style = if state.current_tab == Tab::Feed {
        Style::default().fg(Color::Black).bg(Color::White)
    } else {
        Style::default().fg(Color::White)
    };

    let watch_later_style = if state.current_tab == Tab::WatchLater {
        Style::default().fg(Color::Black).bg(Color::White)
    } else {
        Style::default().fg(Color::White)
    };

    let refresh_style = if state.is_refreshing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };

    let header = Line::from(vec![
        Span::styled(" [Feed] ", feed_style),
        Span::raw(" "),
        Span::styled("[Watch Later] ", watch_later_style),
        Span::raw(" ".repeat((area.width as usize).saturating_sub(40))),
        Span::styled(
            if state.is_refreshing {
                "[Refreshing...]"
            } else {
                "[r:Refresh]"
            },
            refresh_style,
        ),
        Span::raw(" "),
        Span::styled("[?:Help]", Style::default().fg(Color::Cyan)),
        Span::raw(" "),
    ]);

    frame.render_widget(Paragraph::new(header), header_area);
}

fn render_grid(
    frame: &mut Frame,
    videos: &[&Video],
    state: &AppState,
    layout: &GridLayout,
    thumb_cache: &mut ThumbnailCache,
) {
    let start_idx = state.scroll_offset * layout.cols;

    for (i, video) in videos.iter().skip(start_idx).take(layout.visible_items).enumerate() {
        let global_idx = start_idx + i;

        if let Some((x, y, w, h)) = layout.card_area(global_idx, state.scroll_offset) {
            let card_area = Rect {
                x,
                y,
                width: w,
                height: h,
            };

            let is_selected = state.selected_index == Some(global_idx);
            let is_watch_later = state.watch_later.contains(&video.video_id);

            render_video_card(
                frame,
                video,
                card_area,
                is_selected,
                is_watch_later,
                layout,
                thumb_cache,
            );
        }
    }
}

fn render_video_card(
    frame: &mut Frame,
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
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Thumbnail area
    let thumb_width = layout.thumbnail_width();
    let thumb_height = layout.thumbnail_height();

    if let Some(rendered) = thumb_cache.get_rendered(&video.video_id, thumb_width, thumb_height) {
        // Render the chafa output with ANSI color codes
        let thumb_area = Rect {
            x: inner.x,
            y: inner.y,
            width: thumb_width,
            height: thumb_height,
        };

        // Convert ANSI escape codes to ratatui Text
        if let Ok(text) = rendered.into_text() {
            frame.render_widget(Paragraph::new(text), thumb_area);
        }
    } else {
        // Placeholder while loading
        let placeholder = Paragraph::new("[Loading...]")
            .style(Style::default().fg(Color::DarkGray));
        let thumb_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: thumb_height.min(inner.height),
        };
        frame.render_widget(placeholder, thumb_area);
    }

    // Text area below thumbnail
    let text_y = inner.y + thumb_height;
    let text_height = inner.height.saturating_sub(thumb_height);

    if text_height > 0 {
        let text_area = Rect {
            x: inner.x,
            y: text_y,
            width: inner.width,
            height: text_height,
        };

        // Title (truncate to fit)
        let title = truncate_str(&video.title, inner.width as usize * 2);
        let channel = truncate_str(&video.channel_name, inner.width as usize);
        let time_ago = format_time_ago(&video.published);

        let watch_later_marker = if is_watch_later { " [W]" } else { "" };

        let text = vec![
            Line::from(Span::styled(
                title,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("{}{}", channel, watch_later_marker),
                Style::default().fg(Color::Gray),
            )),
            Line::from(Span::styled(time_ago, Style::default().fg(Color::DarkGray))),
        ];

        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), text_area);
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

        format!(
            " {} videos{} | ↑↓ scroll, Enter play, w watch later, q quit",
            videos.len(),
            refresh_info
        )
    };

    let footer = Paragraph::new(status)
        .style(Style::default().fg(Color::Cyan).bg(Color::DarkGray));

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

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let help = Paragraph::new(help_text).block(block);
    frame.render_widget(help, help_area);
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max_len.saturating_sub(3)).collect::<String>())
    }
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
