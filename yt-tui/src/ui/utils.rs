use chrono::{DateTime, Utc};

/// Wrap text to fit within a given width
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if current_line.is_empty() {
            current_line = word.to_string();
        } else if current_line.len() + 1 + word.len() <= width {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    lines
}

/// Truncate string with ellipsis
pub fn truncate_str(s: &str, max_len: usize) -> String {
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
pub fn wrap_title_two_lines(s: &str, line_width: usize) -> (String, String) {
    let chars: Vec<char> = s.chars().collect();

    if chars.len() <= line_width {
        // Fits in one line - use em dash on second line
        return (
            format!("{:<width$}", s, width = line_width),
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

pub fn format_time_ago(dt: &DateTime<Utc>) -> String {
    let now = Utc::now();
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
