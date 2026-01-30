use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::data::{AppState, Video};

use super::utils::format_time_ago;

pub fn render_footer(frame: &mut Frame, state: &AppState, videos: &[&Video], area: Rect) {
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
