use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::data::{AppState, Tab};

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

pub fn render_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let badges: [&str; _] = [
        "⓿", "➊", "➋", "➌", "➍", "➎", "➏", "➐", "➑", "➒", "➓", "⓫", "⓬", "⓭", "⓮", "⓯", "⓰", "⓱",
        "⓲", "⓳", "⓴",
    ];

    let watch_later_count = state
        .videos
        .iter()
        .filter(|v| state.watch_later.contains(&v.video_id))
        .count();
    let watch_later_badge = badges[watch_later_count.min(badges.len() - 1)];
    let mut watch_later_label = " Watch Later ".to_string();
    watch_later_label.push_str(watch_later_badge);
    if watch_later_count > 20 {
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
    let tabs: &[(&str, Tab)] = &[
        (feed_label, Tab::Feed),
        (&watch_later_label, Tab::WatchLater),
    ];

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
                let refresh_char: String = middle.chars().skip(refresh_start).take(1).collect();
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
