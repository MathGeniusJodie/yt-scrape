use ansi_to_tui::IntoText;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::ops::Add;

use crate::cache::ThumbnailCache;
use crate::data::Video;
use crate::ui::GridLayout;

use super::utils::{format_time_ago, truncate_str, wrap_title_two_lines};

pub fn render_video_card(
    buf: &mut Buffer,
    video: &Video,
    area: Rect,
    is_watch_later: bool,
    is_downloaded: bool,
    has_transcript: bool,
    layout: &GridLayout,
    thumb_cache: &ThumbnailCache,
) {
    // Card border - no top border, selection indicator provides that when selected
    let border_style = Style::default()
        .fg(Color::Rgb(0, 0, 0))
        .add_modifier(Modifier::BOLD);

    let border_set = ratatui::symbols::border::Set {
        vertical_left: "⢐",
        vertical_right: "⣗",
        horizontal_top: " ", // No top border
        horizontal_bottom: "⠙",
        top_left: " ",
        top_right: " ",
        bottom_left: "⠈",
        bottom_right: "⠁",
    };

    let block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
        .border_style(border_style)
        .border_set(border_set);

    // Calculate inner area manually since we have no top border
    let inner = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    };
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
            format!(
                "{:>width$}",
                " ✨  🗏  🖬  ⊂⬤  ",
                width = thumb_width as usize
            )
        } else {
            format!(
                "{:>width$}",
                " ✨  🗏  🖬  ⬤⊃  ",
                width = thumb_width as usize
            )
        };

        // Colors for folder (based on download status), toggle (based on watch later status),
        // and transcript icon (based on transcript availability)
        let folder_color = if is_downloaded {
            Color::Yellow
        } else {
            Color::White // White when not downloaded
        };
        let toggle_color = if is_watch_later {
            Color::Yellow
        } else {
            Color::White // white when not watch later
        };
        let transcript_color = if has_transcript {
            Color::Yellow
        } else {
            Color::White // White when no transcript
        };

        let text_lines = vec![
            //Line::from(" "),
            fancy_bg
                .iter()
                .take(thumb_width as usize)
                .zip(checkbox_line.chars())
                .map(|((r, g, b), char)| {
                    // Folder icon gets folder_color, toggle chars get toggle_color, ✨ gets cyan, 🗏 gets transcript_color
                    let fg_color = if char == '🗁' || char == '🖬' {
                        folder_color
                    } else if char == '⊂' || char == '⬤' || char == '⊃' {
                        toggle_color
                    } else if char == '✨' {
                        Color::Cyan
                    } else if char == '🗏' {
                        transcript_color
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
                .enumerate()
                .map(|(i, ((r, g, b), char))| {
                    if i <= channel_len {
                        Span::styled(
                            char.to_string(),
                            Style::default()
                                .bg(Color::Rgb(*r, *g, *b))
                                .add_modifier(Modifier::ITALIC),
                        )
                    } else {
                        Span::styled(
                            char.to_string(),
                            Style::default().bg(Color::Rgb(*r, *g, *b)),
                        )
                    }
                })
                .collect(),
        ];

        Paragraph::new(text_lines).render(text_area, buf);
    }
}
