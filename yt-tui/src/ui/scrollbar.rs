//! Smooth scrollbar with sub-cell precision using color blending.
//!
//! Uses half-block characters (╻, ╹) for 2x vertical resolution,
//! combined with RGB color interpolation for perceptually smooth scrolling.

use ratatui::prelude::*;
use ratatui::widgets::StatefulWidget;

/// Characters for sub-cell precision
const CHAR_FULL: &str = "┃";
const CHAR_TOP_HALF: &str = "╻"; // Visible in top portion of cell
const CHAR_BOTTOM_HALF: &str = "╹"; // Visible in bottom portion of cell

/// A smooth scrollbar with sub-cell precision using color blending.
///
/// Renders on the right edge of the given area. Uses floating-point
/// positioning for smooth animation during inertial scrolling.
pub struct SmoothScrollbar {
    thumb_color: Color,
    track_color: Color,
}

impl Default for SmoothScrollbar {
    fn default() -> Self {
        Self {
            thumb_color: Color::Cyan,
            track_color: Color::Rgb(10, 10, 10),
        }
    }
}

impl SmoothScrollbar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn thumb_color(mut self, color: Color) -> Self {
        self.thumb_color = color;
        self
    }

    pub fn track_color(mut self, color: Color) -> Self {
        self.track_color = color;
        self
    }

    /// Extract RGB components from a Color, with sensible defaults
    fn color_to_rgb(color: Color) -> (f64, f64, f64) {
        match color {
            Color::Rgb(r, g, b) => (r as f64, g as f64, b as f64),
            Color::Cyan => (0.0, 255.0, 255.0),
            Color::Black => (0.0, 0.0, 0.0),
            Color::White => (255.0, 255.0, 255.0),
            Color::DarkGray => (64.0, 64.0, 64.0),
            Color::Gray => (128.0, 128.0, 128.0),
            _ => (128.0, 128.0, 128.0),
        }
    }

    /// Convert any Color to Color::Rgb for consistent rendering
    fn to_rgb(color: Color) -> Color {
        let (r, g, b) = Self::color_to_rgb(color);
        Color::Rgb(r as u8, g as u8, b as u8)
    }

    /// Linear interpolation between two RGB colors
    /// t=0.0 returns `from`, t=1.0 returns `to`
    fn lerp_rgb(from: Color, to: Color, t: f64) -> Color {
        let t = t.clamp(0.0, 1.0);
        let (fr, fg, fb) = Self::color_to_rgb(from);
        let (tr, tg, tb) = Self::color_to_rgb(to);

        Color::Rgb(
            (fr + (tr - fr) * t) as u8,
            (fg + (tg - fg) * t) as u8,
            (fb + (tb - fb) * t) as u8,
        )
    }
}

/// State for the smooth scrollbar.
pub struct SmoothScrollbarState {
    /// Total content length (in scroll units, e.g., pixels or lines)
    pub content_length: f64,
    /// Visible viewport length (same units as content_length)
    pub viewport_length: f64,
    /// Current scroll position (0.0 to content_length - viewport_length)
    pub position: f64,
}

impl SmoothScrollbarState {
    pub fn new(content_length: f64, viewport_length: f64) -> Self {
        Self {
            content_length,
            viewport_length,
            position: 0.0,
        }
    }

    pub fn position(mut self, position: f64) -> Self {
        self.position = position;
        self
    }
}

impl StatefulWidget for SmoothScrollbar {
    type State = SmoothScrollbarState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.height == 0 || state.content_length <= state.viewport_length {
            // No scrolling needed - render subtle track only
            let x = area.x + area.width.saturating_sub(1);
            let track_rgb = Self::to_rgb(self.track_color);
            for row in 0..area.height {
                if let Some(cell) = buf.cell_mut((x, area.y + row)) {
                    cell.set_symbol(CHAR_FULL)
                        .set_fg(track_rgb)
                        .set_bg(Color::Black);
                }
            }
            return;
        }

        let track_height = area.height as f64;

        // Calculate thumb size (proportional to viewport/content ratio)
        // Minimum size is 1.0 cell
        let thumb_ratio = state.viewport_length / state.content_length;
        let thumb_height = (track_height * thumb_ratio).max(1.0);

        // Calculate thumb position
        let max_scroll = state.content_length - state.viewport_length;
        let scroll_ratio = if max_scroll > 0.0 {
            (state.position / max_scroll).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // thumb_top is in cell units (0.0 to track_height - thumb_height)
        let max_thumb_top = track_height - thumb_height;
        let thumb_top = scroll_ratio * max_thumb_top;
        let thumb_bottom = thumb_top + thumb_height;

        // Render each cell in the track
        let x = area.x + area.width.saturating_sub(1);

        for row in 0..area.height {
            let cell_top = row as f64;
            let cell_bottom = cell_top + 1.0;
            let y = area.y + row;

            // Calculate overlap between thumb and this cell
            let overlap_start = thumb_top.max(cell_top);
            let overlap_end = thumb_bottom.min(cell_bottom);
            let coverage = (overlap_end - overlap_start).max(0.0);

            // Convert colors to RGB for consistent rendering
            // (ANSI named colors like Color::Cyan render differently than Color::Rgb)
            let thumb_rgb = Self::to_rgb(self.thumb_color);
            let track_rgb = Self::to_rgb(self.track_color);

            let (symbol, color) = if coverage <= 0.0 {
                // No coverage - pure track
                (CHAR_FULL, track_rgb)
            } else if coverage >= 0.999 {
                // Full coverage - pure thumb (use 0.999 to handle float imprecision)
                (CHAR_FULL, thumb_rgb)
            } else {
                // Partial coverage - determine character and blend color
                self.render_partial_cell(
                    cell_top,
                    thumb_top,
                    thumb_bottom,
                    coverage,
                    thumb_rgb,
                    track_rgb,
                )
            };

            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(symbol).set_fg(color).set_bg(Color::Black);
            }
        }
    }
}

impl SmoothScrollbar {
    /// Determine the character and color for a partially-covered cell.
    ///
    /// Strategy:
    /// - Half-characters (╻, ╹) are used only when coverage <= 0.5
    /// - Color blending only applies to half-characters for sub-half-cell precision
    /// - When coverage > 0.5, use full character at FULL thumb color
    ///   (this prevents middle cells from appearing faded)
    fn render_partial_cell(
        &self,
        cell_top: f64,
        thumb_top: f64,
        thumb_bottom: f64,
        coverage: f64,
        thumb_rgb: Color,
        track_rgb: Color,
    ) -> (&'static str, Color) {
        let cell_mid = cell_top + 0.5;
        let cell_bottom = cell_top + 1.0;

        // Is this the top edge of the thumb (thumb entering this cell)?
        let is_top_edge = thumb_top > cell_top && thumb_top < cell_bottom;
        // Is this the bottom edge of the thumb (thumb exiting this cell)?
        let is_bottom_edge = thumb_bottom > cell_top && thumb_bottom < cell_bottom;

        if is_top_edge && is_bottom_edge {
            // Entire thumb fits within this single cell (very small thumb)
            if coverage <= 0.5 {
                // Use half char based on where thumb is positioned in cell
                let thumb_center = (thumb_top + thumb_bottom) / 2.0;
                let symbol = if thumb_center < cell_mid {
                    CHAR_TOP_HALF // Thumb in top half of cell
                } else {
                    CHAR_BOTTOM_HALF // Thumb in bottom half of cell
                };
                // Blend: 0 → 0%, 0.5 → 100% (scaled to half-cell)
                let blend = coverage * 2.0;
                (symbol, Self::lerp_rgb(track_rgb, thumb_rgb, blend))
            } else {
                // More than half covered - full char with proportional blend
                // Blend: 0.5 → 50%, 1.0 → 100%
                (CHAR_FULL, Self::lerp_rgb(track_rgb, thumb_rgb, coverage))
            }
        } else if is_top_edge {
            // Thumb starts in this cell (top edge of thumb)
            // Thumb covers from thumb_top down to cell_bottom
            if thumb_top >= cell_mid {
                // Starts in bottom half - use ╻ to cap the top of the thumb
                // Blend: cell_bottom → 0%, cell_mid → 100%
                let blend = (cell_bottom - thumb_top) * 2.0;
                (CHAR_TOP_HALF, Self::lerp_rgb(track_rgb, thumb_rgb, blend))
            } else {
                // Starts in top half - more than half covered, use full char
                // coverage is 0.5-1.0, blend proportionally: 0.5 → 50%, 1.0 → 100%
                (CHAR_FULL, Self::lerp_rgb(track_rgb, thumb_rgb, coverage))
            }
        } else if is_bottom_edge {
            // Thumb ends in this cell (bottom edge of thumb)
            // Thumb covers from cell_top up to thumb_bottom
            if thumb_bottom <= cell_mid {
                // Ends in top half - use ╹ to cap the bottom of the thumb
                // Blend: cell_top → 0%, cell_mid → 100%
                let blend = (thumb_bottom - cell_top) * 2.0;
                (
                    CHAR_BOTTOM_HALF,
                    Self::lerp_rgb(track_rgb, thumb_rgb, blend),
                )
            } else {
                // Ends in bottom half - more than half covered, use full char
                // coverage is 0.5-1.0, blend proportionally: 0.5 → 50%, 1.0 → 100%
                (CHAR_FULL, Self::lerp_rgb(track_rgb, thumb_rgb, coverage))
            }
        } else {
            // Shouldn't reach here for partial coverage, but fallback
            (CHAR_FULL, thumb_rgb)
        }
    }
}
