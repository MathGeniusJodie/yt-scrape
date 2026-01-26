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
}

impl GridLayout {
    /// Fixed card dimensions - CARD_WIDTH is the single source of truth
    const CARD_WIDTH: u16 = 34;
    const BORDER_WIDTH: u16 = 2; // left + right border
    const THUMBNAIL_WIDTH: u16 = Self::CARD_WIDTH - Self::BORDER_WIDTH; // fills inner area
    const THUMBNAIL_HEIGHT: u16 = 9; // 16:9 aspect ratio accounting for ~1:2 char cells
    const TEXT_LINES: u16 = 4; // title (2) + channel + time
    const CARD_HEIGHT: u16 = Self::THUMBNAIL_HEIGHT + Self::TEXT_LINES + 2; // +2 for top/bottom border

    pub const HEADER_HEIGHT: u16 = 1;
    pub const FOOTER_HEIGHT: u16 = 1;

    pub fn calculate(terminal_width: u16, terminal_height: u16) -> Self {
        let grid_height = terminal_height.saturating_sub(Self::HEADER_HEIGHT + Self::FOOTER_HEIGHT);

        // Calculate how many fixed-width columns fit
        let cols = (terminal_width / Self::CARD_WIDTH).max(1) as usize;

        Self {
            cols,
            card_width: Self::CARD_WIDTH,
            card_height: Self::CARD_HEIGHT,
            grid_height,
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

        let col = (x / self.card_width) as usize;
        let row = y / self.card_height as usize;

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

    /// Get the area for a specific card (scroll_offset is in lines)
    /// Returns None if card is completely off-screen
    pub fn card_area(&self, index: usize, scroll_offset_lines: usize) -> Option<(i16, u16, u16)> {
        let row = index / self.cols;
        let col = index % self.cols;

        // Calculate y position in lines, accounting for scroll
        let card_top = (row as i32 * self.card_height as i32) - scroll_offset_lines as i32;
        let card_bottom = card_top + self.card_height as i32;

        // Skip if completely above or below viewport
        if card_bottom <= 0 || card_top >= self.grid_height as i32 {
            return None;
        }

        let x = col as u16 * self.card_width;

        Some((card_top as i16, x, self.card_width))
    }

    /// Maximum scroll offset in lines
    pub fn max_scroll(&self, total_items: usize) -> usize {
        let total_rows = (total_items + self.cols - 1) / self.cols;
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
}
