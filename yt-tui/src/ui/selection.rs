use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders};

use super::GridLayout;

/// Floating selection indicator that animates between cards
#[derive(Debug, Clone)]
pub struct SelectionIndicator {
    /// Current animated position (x, y) in pixels
    current_x: f64,
    current_y: f64,
    /// Target position (the selected card's position)
    target_x: f64,
    target_y: f64,
    /// Whether the indicator is visible
    visible: bool,
}

impl Default for SelectionIndicator {
    fn default() -> Self {
        Self {
            current_x: 0.0,
            current_y: 0.0,
            target_x: 0.0,
            target_y: 0.0,
            visible: false,
        }
    }
}

impl SelectionIndicator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the target card position (call when selection changes)
    pub fn set_target(&mut self, x: u16, y: u16) {
        self.target_x = x as f64;
        self.target_y = y as f64;
        self.visible = true;
    }

    /// Jump immediately to position (no animation)
    pub fn jump_to(&mut self, x: u16, y: u16) {
        self.current_x = x as f64;
        self.current_y = y as f64;
        self.target_x = x as f64;
        self.target_y = y as f64;
        self.visible = true;
    }

    /// Hide the indicator
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Update animation, returns true if still animating
    pub fn animate(&mut self, dt: f64) -> bool {
        if !self.visible {
            return false;
        }

        const SPEED: f64 = 4.0; // Movement speed factor
        const SNAP_DIST: f64 = 1.0; // Snap when this close

        for _ in 0..8 {
            let dx = self.target_x - self.current_x;
            let dy = self.target_y - self.current_y;
            let dist = (dx * dx + dy * dy).sqrt();

            // Already at target
            if dist < 0.001 {
                return false;
            }

            // Snap if close enough (return true so final position gets drawn)
            if dist < SNAP_DIST {
                self.current_x = self.target_x;
                self.current_y = self.target_y;
                return true;
            }

            // Proportional movement
            self.current_x += dx * SPEED * dt;
            self.current_y += dy * SPEED * dt;
        }

        true
    }

    /// Check if currently animating
    pub fn is_animating(&self) -> bool {
        if !self.visible {
            return false;
        }
        let dx = (self.target_x - self.current_x).abs();
        let dy = (self.target_y - self.current_y).abs();
        dx > 0.5 || dy > 0.5
    }

    /// Get the current render position (rounded to cells)
    pub fn position(&self) -> (u16, u16) {
        (self.current_x.round() as u16, self.current_y.round() as u16)
    }

    /// Render the selection indicator into the scroll view buffer
    /// The y coordinate is in content space (scroll view handles the offset)
    pub fn render(&self, buf: &mut Buffer, layout: &GridLayout) {
        if !self.visible {
            return;
        }

        let (x, y) = self.position();

        // Selection indicator is 1 row taller than the card to include top border
        // We position it 1 row above the card so the top border appears above the card content
        let indicator_y = y.saturating_sub(1);
        let indicator_height = layout.card_height + 1;

        let area = Rect {
            x,
            y: indicator_y,
            width: layout.card_width,
            height: indicator_height,
        };

        // Check if area is within buffer bounds
        if area.x >= buf.area.width {
            return;
        }

        let border_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);

        // Render just the border (the block with no content)
        block.render(area, buf);
    }
}
