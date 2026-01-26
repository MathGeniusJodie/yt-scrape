use ansi_to_tui::IntoText;
use crate::cache::ThumbnailCache;
use crate::data::{AppState, Tab, Video};
use crate::ui::GridLayout;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

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
    render_grid(frame, &videos, state, layout, thumb_cache, area);

    // Render footer (status bar)
    render_footer(frame, state, &videos, area);

    // Render help overlay if active
    if state.show_help {
        render_help(frame, area);
    }
}

fn render_header(frame: &mut Frame, state: &AppState, area: Rect) {
    // Tab definitions: (label, Tab variant)
    let tabs: &[(&str, Tab)] = &[
        (" Feed ", Tab::Feed),
        (" Watch Later ", Tab::WatchLater),
    ];

    let selected_style = Style::default().fg(Color::White);
    let unselected_style = Style::default().fg(Color::DarkGray);

    let refresh_style = if state.is_refreshing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };

    let refresh_label = if state.is_refreshing { " ↻… " } else { " ↻ " };
    let help_label = " ? ";

    // Build button parts (top, middle, bottom) for refresh and help
    let refresh_width = refresh_label.chars().count();
    let refresh_top = format!("╭{}╮", "─".repeat(refresh_width));
    let refresh_middle = format!("│{}│", refresh_label);
    let refresh_bottom = format!("╰{}╯", "─".repeat(refresh_width));

    let help_width = help_label.chars().count();
    let help_top = format!("╭{}╮", "─".repeat(help_width));
    let help_middle = format!("│{}│", help_label);
    let help_bottom = format!("╰{}╯", "─".repeat(help_width));

    // Build tab parts for each tab
    let tab_parts: Vec<(String, String, String, bool)> = tabs
        .iter()
        .map(|(label, tab)| {
            let width = label.chars().count();
            let top = format!("╭{}╮", "─".repeat(width));
            let middle = format!("│{}│", label);
            let is_selected = state.current_tab == *tab;
            let bottom = if is_selected{
                format!("╯{}╰", " ".repeat(width))   
            } else {
                format!("─{}─", "─".repeat(width))
            };
            
            (top, middle, bottom, is_selected)
        })
        .collect();

    // Calculate total tabs width
    let tabs_width: usize = tab_parts.iter().map(|(top, _, _, _)| top.chars().count()).sum::<usize>()
        + tab_parts.len().saturating_sub(1); // spaces between tabs
    let right_side_len = refresh_top.chars().count() + 1 + help_top.chars().count();
    let spacing = (area.width as usize).saturating_sub(tabs_width + right_side_len);

    // Build line 1: tab tops with right-aligned buttons
    let mut line1_spans: Vec<Span> = Vec::new();
    for (i, (top, _, _, is_selected)) in tab_parts.iter().enumerate() {
        if i > 0 {
            line1_spans.push(Span::raw(" "));
        }
        let style = if *is_selected { selected_style } else { unselected_style };
        line1_spans.push(Span::styled(top.clone(), style));
    }
    line1_spans.push(Span::raw(" ".repeat(spacing)));
    line1_spans.push(Span::styled(&refresh_top, refresh_style));
    line1_spans.push(Span::raw(" "));
    line1_spans.push(Span::styled(&help_top, Style::default().fg(Color::Cyan)));

    // Build line 2: tab middles with labels
    let mut line2_spans: Vec<Span> = Vec::new();
    for (i, (_, middle, _, is_selected)) in tab_parts.iter().enumerate() {
        if i > 0 {
            line2_spans.push(Span::raw(" "));
        }
        let style = if *is_selected { selected_style } else { unselected_style };
        line2_spans.push(Span::styled(middle.clone(), style));
    }
    line2_spans.push(Span::raw(" ".repeat(spacing)));
    line2_spans.push(Span::styled(&refresh_middle, refresh_style));
    line2_spans.push(Span::raw(" "));
    line2_spans.push(Span::styled(&help_middle, Style::default().fg(Color::Cyan)));

    // Build line 3: tab bottoms
    let mut line3_spans: Vec<Span> = Vec::new();
    for (i, (_, _, bottom, _)) in tab_parts.iter().enumerate() {
        if i > 0 {
            line3_spans.push(Span::raw("─"));
        }
        line3_spans.push(Span::styled(bottom.clone(), selected_style));
    }
    line3_spans.push(Span::raw("─".repeat(spacing)));
    line3_spans.push(Span::styled(&refresh_bottom, refresh_style));
    line3_spans.push(Span::raw(" "));
    line3_spans.push(Span::styled(&help_bottom, Style::default().fg(Color::Cyan)));

    let header_area1 = Rect { x: 0, y: 0, width: area.width, height: 1 };
    let header_area2 = Rect { x: 0, y: 1, width: area.width, height: 1 };
    let header_area3 = Rect { x: 0, y: 2, width: area.width, height: 1 };

    frame.render_widget(Paragraph::new(Line::from(line1_spans)), header_area1);
    frame.render_widget(Paragraph::new(Line::from(line2_spans)), header_area2);
    frame.render_widget(Paragraph::new(Line::from(line3_spans)), header_area3);
}

/// Returns the click regions for header tabs as (start_col, end_col, Tab)
/// This is used by the click handler in app.rs
pub fn header_tab_regions() -> Vec<(u16, u16, Tab)> {
    let tabs: &[(&str, Tab)] = &[
        (" Feed ", Tab::Feed),
        (" Watch Later ", Tab::WatchLater),
    ];

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
) {
    // Grid area (between header and footer)
    let grid_area = Rect {
        x: 0,
        y: GridLayout::HEADER_HEIGHT,
        width: area.width,
        height: layout.grid_height,
    };

    // Calculate which video rows might be visible
    let first_visible_row = state.scroll_offset / layout.card_height as usize;
    let last_visible_row = (state.scroll_offset + layout.grid_height as usize) / layout.card_height as usize + 1;

    let first_video = first_visible_row * layout.cols;
    let last_video = ((last_visible_row + 1) * layout.cols).min(videos.len());

    for idx in first_video..last_video {
        if let Some(video) = videos.get(idx) {
            if let Some((y_offset, x, w)) = layout.card_area(idx, state.scroll_offset) {
                // Calculate visible portion of card
                let visible_y_start = y_offset.max(0) as u16;
                let clip_top = (-y_offset).max(0) as u16;
                let visible_height = (layout.card_height as i16 - clip_top as i16)
                    .min((layout.grid_height as i16) - y_offset.max(0))
                    .max(0) as u16;

                if visible_height == 0 {
                    continue;
                }

                let card_area = Rect {
                    x,
                    y: grid_area.y + visible_y_start,
                    width: w,
                    height: visible_height,
                };

                let is_selected = state.selected_index == Some(idx);
                let is_watch_later = state.watch_later.contains(&video.video_id);

                render_video_card(
                    frame,
                    video,
                    card_area,
                    clip_top,
                    is_selected,
                    is_watch_later,
                    layout,
                    thumb_cache,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_video_card(
    frame: &mut Frame,
    video: &Video,
    area: Rect,
    clip_top: u16,
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
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Thumbnail area
    let thumb_width = layout.thumbnail_width();
    let thumb_height = layout.thumbnail_height();

    // Calculate how much of the thumbnail is clipped
    let thumb_clip = clip_top.saturating_sub(1); // -1 for border
    let thumb_visible_height = thumb_height.saturating_sub(thumb_clip).min(inner.height);

    if thumb_visible_height > 0 && thumb_clip < thumb_height {
        if let Some(rendered) = thumb_cache.get_rendered(&video.video_id, thumb_width, thumb_height) {
            let thumb_area = Rect {
                x: inner.x,
                y: inner.y,
                width: thumb_width.min(inner.width),
                height: thumb_visible_height,
            };

            // Convert ANSI and skip clipped lines
            if let Ok(text) = rendered.into_text() {
                let lines: Vec<Line> = text
                    .lines
                    .into_iter()
                    .skip(thumb_clip as usize)
                    .take(thumb_visible_height as usize)
                    .collect();
                frame.render_widget(Paragraph::new(lines), thumb_area);
            }
        } else {
            // Placeholder while loading
            if thumb_clip == 0 {
                let placeholder = Paragraph::new("[Loading...]")
                    .style(Style::default().fg(Color::DarkGray));
                let thumb_area = Rect {
                    x: inner.x,
                    y: inner.y,
                    width: inner.width,
                    height: thumb_visible_height.min(1),
                };
                frame.render_widget(placeholder, thumb_area);
            }
        }
    }

    // Text area below thumbnail
    let text_start_in_card = thumb_height + 1; // +1 for border
    if clip_top < text_start_in_card + 3 {
        let text_clip = clip_top.saturating_sub(text_start_in_card);
        let text_y = if clip_top <= text_start_in_card {
            inner.y + thumb_height.saturating_sub(thumb_clip)
        } else {
            inner.y
        };
        let text_height = inner.height.saturating_sub(thumb_height.saturating_sub(thumb_clip));

        if text_height > 0 {
            let text_area = Rect {
                x: inner.x,
                y: text_y,
                width: inner.width,
                height: text_height,
            };

            // Title (always exactly 2 lines)
            let (title_line1, title_line2) = wrap_title_two_lines(&video.title, inner.width as usize);
            let channel = truncate_str(&video.channel_name, inner.width as usize);
            let time_ago = format_time_ago(&video.published);

            let title_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);

            // Calculate padding to right-align timestamp
            let channel_len = channel.chars().count();
            let time_len = time_ago.chars().count();
            let padding = (inner.width as usize).saturating_sub(channel_len + time_len);

            let mut text_lines = vec![
                Line::from(" "),
                Line::from(Span::styled(title_line1, title_style)),
                Line::from(Span::styled(title_line2, title_style)),
                Line::from(vec![
                    Span::styled(channel.clone(), Style::default().fg(Color::Gray)),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(time_ago.clone(), Style::default().fg(Color::DarkGray)),
                ]),
            ];

            // Skip clipped lines
            let text: Vec<Line> = text_lines
                .drain(..)
                .skip(text_clip as usize)
                .collect();

            frame.render_widget(Paragraph::new(text), text_area);
        }
    }

    // Render watch later checkbox on bottom border (overlapping like a title)
    let bottom_border_row = area.y + area.height - 1;
    if area.height > 0 {
        let (checkbox_text, checkbox_style) = if is_watch_later {
            (" W:☑ ", Style::default().fg(Color::Rgb(255, 165, 0))) // Bright orange
        } else {
            (" W:☐ ", Style::default().fg(Color::Rgb(128, 128, 128))) // Medium grey
        };

        // Position checkbox on the right side of the bottom border
        let checkbox_len = checkbox_text.chars().count() as u16;
        let checkbox_x = area.x + area.width.saturating_sub(checkbox_len + 2);

        let checkbox_area = Rect {
            x: checkbox_x,
            y: bottom_border_row,
            width: checkbox_len,
            height: 1,
        };

        frame.render_widget(Paragraph::new(checkbox_text).style(checkbox_style), checkbox_area);
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
            " {} videos{} | ↑↓ scroll, ⏎ play, w watch later, r refresh, ? help, q quit",
            videos.len(),
            refresh_info
        )
    };

    let footer = Paragraph::new(status)
        .style(Style::default().fg(Color::LightCyan).bg(Color::Rgb(20, 20, 20)));

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
        .style(Style::default().bg(Color::Black));

    let help = Paragraph::new(help_text).block(block);
    frame.render_widget(help, help_area);
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_len.saturating_sub(1)).collect::<String>())
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
        format!("{}…", remaining.chars().take(line_width.saturating_sub(1)).collect::<String>())
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
