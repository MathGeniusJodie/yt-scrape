/// Calculates grid layout based on terminal dimensions
#[derive(Debug, Clone)]
pub struct GridLayout {
    /// Number of video cards per row
    pub cols: usize,
    /// Width of each card in terminal columns (fixed)
    pub card_width: u16,
    /// Height of each card in terminal rows (fixed)
    pub card_height: u16,
    /// Available height for the grid area (excluding header/footer)
    pub grid_height: u16,
    /// Horizontal offset to center the grid
    pub x_offset: u16,
}

impl GridLayout {
    /// Fixed card dimensions - CARD_WIDTH is the single source of truth
    const THUMBNAIL_HEIGHT: u16 = 12; // 16:9 aspect
    const THUMBNAIL_WIDTH: u16 = (Self::THUMBNAIL_HEIGHT as f32 * 16.0 / 9.0 * 2.0).round() as u16;
    const BORDER_WIDTH: u16 = 2; // left + right border
    const CARD_WIDTH: u16 = Self::THUMBNAIL_WIDTH + Self::BORDER_WIDTH;
    const TEXT_LINES: u16 = 4; // pad + title (2) + channel and time
    const CARD_HEIGHT: u16 = Self::THUMBNAIL_HEIGHT + Self::TEXT_LINES + 2 - 2; // +2 for top/bottom border, -2 for thumbnail cut by title

    pub const HEADER_HEIGHT: u16 = 3;
    pub const FOOTER_HEIGHT: u16 = 1;

    pub fn calculate(terminal_width: u16, terminal_height: u16) -> Self {
        let grid_height = terminal_height.saturating_sub(Self::HEADER_HEIGHT + Self::FOOTER_HEIGHT);

        // Calculate how many fixed-width columns fit
        let cols = (terminal_width / Self::CARD_WIDTH).max(1) as usize;

        // Calculate horizontal offset to center the grid
        let total_grid_width = cols as u16 * Self::CARD_WIDTH;
        let x_offset = terminal_width.saturating_sub(total_grid_width) / 2;

        Self {
            cols,
            card_width: Self::CARD_WIDTH,
            card_height: Self::CARD_HEIGHT,
            grid_height,
            x_offset,
        }
    }

    /// Convert terminal coordinates to video index (scroll_offset is in lines)
    pub fn coords_to_index(
        &self,
        x: u16,
        y: u16,
        scroll_offset_lines: usize,
        total_items: usize,
    ) -> Option<usize> {
        // Account for header
        if y < Self::HEADER_HEIGHT {
            return None;
        }
        let y = (y - Self::HEADER_HEIGHT) as usize + scroll_offset_lines;

        // Account for horizontal centering offset
        if x < self.x_offset {
            return None;
        }
        let x = x - self.x_offset;

        let col = (x / self.card_width) as usize;
        let row = y / (self.card_height-1) as usize;

        if col >= self.cols {
            return None;
        }

        let index = row * self.cols + col;
        if index < total_items {
            Some(index)
        } else {
            None
        }
    }

    /// Maximum scroll offset in lines
    pub fn max_scroll(&self, total_items: usize) -> usize {
        let total_rows = total_items.div_ceil(self.cols);
        let total_height = total_rows * self.card_height as usize;
        total_height.saturating_sub(self.grid_height as usize)
    }

    /// Width available for thumbnail inside card (fixed)
    pub fn thumbnail_width(&self) -> u16 {
        Self::THUMBNAIL_WIDTH
    }

    /// Height for thumbnail (fixed)
    pub fn thumbnail_height(&self) -> u16 {
        Self::THUMBNAIL_HEIGHT
    }

    /// Check if coordinates are on the watch later checkbox within a card
    /// Returns Some(video_index) if click is on a checkbox, None otherwise
    pub fn is_checkbox_click(
        &self,
        x: u16,
        y: u16,
        scroll_offset_lines: usize,
        total_items: usize,
    ) -> Option<usize> {
        // Account for header
        if y < Self::HEADER_HEIGHT {
            return None;
        }

        // Account for horizontal centering offset
        if x < self.x_offset {
            return None;
        }
        let x = x - self.x_offset;

        let y_in_grid = (y - Self::HEADER_HEIGHT) as usize + scroll_offset_lines;
        let col = (x / self.card_width) as usize;
        let row = y_in_grid / (self.card_height-1) as usize;

        if col >= self.cols {
            return None;
        }

        let index = row * self.cols + col;
        if index >= total_items {
            return None;
        }

        // Check if click is on the bottom border row where checkbox is rendered
        // Card height is 15, so bottom border is at row 14 (0-indexed)
        let y_in_card = y_in_grid % (self.card_height-1) as usize;
        let bottom_border_row = self.card_height as usize - 5;

        if y_in_card != bottom_border_row {
            return None;
        }

        // Check if x is in the checkbox area (right side of bottom border)
        // Checkbox " W:☑ " is 5 chars, positioned 2 chars from right edge
        let x_in_card = (x as usize) % self.card_width as usize;
        let checkbox_start = self.card_width as usize - 6; // 5 chars + 1 offset from edge
        let checkbox_end = self.card_width as usize;

        if x_in_card >= checkbox_start && x_in_card < checkbox_end {
            return Some(index);
        }

        None
    }

    /// Check if coordinates are on the summary button (✦) within a card
    /// Returns Some(video_index) if click is on the button, None otherwise
    pub fn is_summary_button_click(
        &self,
        x: u16,
        y: u16,
        scroll_offset_lines: usize,
        total_items: usize,
    ) -> Option<usize> {
        // Account for header
        if y < Self::HEADER_HEIGHT {
            return None;
        }

        // Account for horizontal centering offset
        if x < self.x_offset {
            return None;
        }
        let x = x - self.x_offset;

        let y_in_grid = (y - Self::HEADER_HEIGHT) as usize + scroll_offset_lines;
        let col = (x / self.card_width) as usize;
        let row = y_in_grid / (self.card_height-1) as usize;

        if col >= self.cols {
            return None;
        }

        let index = row * self.cols + col;
        if index >= total_items {
            return None;
        }

        // Check if click is on the same row as checkbox
        let y_in_card = y_in_grid % (self.card_height-1) as usize;
        let bottom_border_row = self.card_height as usize - 5;

        if y_in_card != bottom_border_row {
            return None;
        }

        // Check if x is in the ✦ button area (to the left of folder icon)
        // The checkbox_line is: "✦ 🗁  ⊂⬤ " right-aligned
        // ✦ is at the start of this string, about 10-12 chars from right edge
        let x_in_card = (x as usize) % self.card_width as usize;
        let button_start = self.card_width as usize - 8;
        let button_end = self.card_width as usize - 6;

        if x_in_card >= button_start && x_in_card < button_end {
            return Some(index);
        }

        None
    }
}
