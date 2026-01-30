use ansi_to_tui::IntoText;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use termimad::{MadSkin, StyledChar};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use crate::data::AppState;
use crate::gemini::SummaryState;

use super::scrollbar::{SmoothScrollbar, SmoothScrollbarState};
use super::utils::{truncate_str, wrap_text};

/// Calculate the summary modal bounds for a given terminal size
pub fn summary_modal_bounds(terminal_cols: u16, terminal_rows: u16) -> Rect {
    let modal_width = 70.min(terminal_cols.saturating_sub(4));
    let modal_height = (terminal_rows - 6).min(terminal_rows.saturating_sub(4));
    Rect {
        x: (terminal_cols - modal_width) / 2,
        y: (terminal_rows - modal_height) / 2,
        width: modal_width,
        height: modal_height,
    }
}

/// Calculate the help modal bounds for a given terminal size
pub fn help_modal_bounds(terminal_cols: u16, terminal_rows: u16) -> Rect {
    let help_width = 50.min(terminal_cols.saturating_sub(4));
    let help_height = 15.min(terminal_rows.saturating_sub(4));
    Rect {
        x: (terminal_cols - help_width) / 2,
        y: (terminal_rows - help_height) / 2,
        width: help_width,
        height: help_height,
    }
}

/// Calculate the transcript modal bounds for a given terminal size
pub fn transcript_modal_bounds(terminal_cols: u16, terminal_rows: u16) -> Rect {
    let modal_width = 80.min(terminal_cols.saturating_sub(4));
    let modal_height = (terminal_rows - 6).min(terminal_rows.saturating_sub(4));
    Rect {
        x: (terminal_cols - modal_width) / 2,
        y: (terminal_rows - modal_height) / 2,
        width: modal_width,
        height: modal_height,
    }
}

pub fn render_help(frame: &mut Frame, area: Rect) {
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

pub fn render_summary(frame: &mut Frame, state: &AppState, area: Rect) {
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
        .map(|t| {
            format!(
                " ✨ {} ",
                truncate_str(t, modal_width.saturating_sub(6) as usize)
            )
        })
        .unwrap_or_else(|| " ✨ Summary ".to_string());

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .border_type(BorderType::Rounded)
        .style(
            Style::default()
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        );

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
        Some(SummaryState::Streaming(summary)) | Some(SummaryState::Ready(summary)) => {
            let is_streaming = matches!(&state.summary_state, Some(SummaryState::Streaming(_)));

            // Render the block first
            frame.render_widget(block, modal_area);

            // Use termimad to format markdown with ANSI codes
            let mut skin = MadSkin::default_dark();

            // Polished unicode typography
            skin.bullet = StyledChar::from_fg_char(termimad::crossterm::style::Color::White, '•');
            skin.quote_mark =
                StyledChar::from_fg_char(termimad::crossterm::style::Color::Grey, '▌');

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
            let text: Text = ansi_string
                .into_text()
                .unwrap_or_else(|_| Text::raw(summary));

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
            let mut scroll_view =
                ScrollView::new(Size::new(inner.width.saturating_sub(1), content_height))
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

                let mut scrollbar_state =
                    SmoothScrollbarState::new(content_height as f64, viewport_height as f64)
                        .position(scroll_offset as f64);

                frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
            }

            // Show hint at bottom (with streaming indicator if applicable)
            let hint = if is_streaming {
                " ● streaming... "
            } else {
                " ↑/↓ scroll, Esc close "
            };
            let hint_color = if is_streaming {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            let hint_area = Rect {
                x: modal_area.x + 2,
                y: modal_area.y + modal_area.height - 1,
                width: hint.len() as u16,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(hint).style(Style::default().fg(hint_color)),
                hint_area,
            );
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

pub fn render_transcript(frame: &mut Frame, state: &AppState, area: Rect) {
    let modal_width = 80.min(area.width.saturating_sub(4));
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
        .transcript_video_title
        .as_ref()
        .map(|t| {
            format!(
                " 🗏 {} ",
                truncate_str(t, modal_width.saturating_sub(6) as usize)
            )
        })
        .unwrap_or_else(|| " 🗏 Transcript ".to_string());

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .border_type(BorderType::Rounded)
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(modal_area);

    match &state.transcript_content {
        Some(transcript) => {
            // Render the block first
            frame.render_widget(block, modal_area);

            // Wrap text to fit modal width
            let text_width = inner.width.saturating_sub(3) as usize;
            let wrapped_lines = wrap_text(transcript, text_width);

            // Add left padding to each line
            let mut text_lines: Vec<Line> = vec![Line::raw("")]; // top padding
            text_lines.extend(
                wrapped_lines
                    .into_iter()
                    .map(|line| Line::from(vec![Span::raw(" "), Span::raw(line)])),
            );
            text_lines.push(Line::raw("")); // bottom padding

            let total_lines = text_lines.len() as u16;
            let viewport_height = inner.height;

            // Calculate content height for scroll view
            let content_height = total_lines.max(viewport_height);

            // Calculate max scroll and clamp current scroll
            let max_scroll = total_lines.saturating_sub(viewport_height);
            let scroll_offset = (state.transcript_scroll).min(max_scroll);

            // Create scroll view
            let mut scroll_view =
                ScrollView::new(Size::new(inner.width.saturating_sub(1), content_height))
                    .horizontal_scrollbar_visibility(ScrollbarVisibility::Never)
                    .vertical_scrollbar_visibility(ScrollbarVisibility::Never);

            Paragraph::new(text_lines)
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
                    .thumb_color(Color::Yellow)
                    .track_color(Color::Rgb(30, 30, 30));

                let mut scrollbar_state =
                    SmoothScrollbarState::new(content_height as f64, viewport_height as f64)
                        .position(scroll_offset as f64);

                frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
            }

            // Show hint at bottom
            let hint = " ↑/↓ scroll, y copy, Esc close ";
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
        None => {
            // No transcript available
            let no_transcript_text = vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "No transcript available",
                    Style::default().fg(Color::Yellow),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "Transcript is still downloading or not available for this video",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "Press Esc to close",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let paragraph = Paragraph::new(no_transcript_text)
                .block(block)
                .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(paragraph, modal_area);
        }
    }
}
