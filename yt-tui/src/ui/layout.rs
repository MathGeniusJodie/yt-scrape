/// Calculates grid layout based on terminal dimensions
#[derive(Debug, Clone)]
pub struct GridLayout {
    /// Number of video cards per row
    pub cols: usize,
    /// Width of each card in terminal columns
    pub card_width: u16,
    /// Height of each card in terminal rows
    pub card_height: u16,
    /// How many rows fit in the viewport
    pub visible_rows: usize,
    /// Total visible items
    pub visible_items: usize,
}

impl GridLayout {
    /// Card dimensions (1.5x wider than before)
    const THUMBNAIL_WIDTH: u16 = 30;
    const THUMBNAIL_HEIGHT: u16 = 9;
    const TEXT_LINES: u16 = 4; // title (2) + channel + time
    const CARD_PADDING: u16 = 1;

    const HEADER_HEIGHT: u16 = 1;
    const FOOTER_HEIGHT: u16 = 1;

    pub fn calculate(terminal_width: u16, terminal_height: u16) -> Self {
        let min_card_width = Self::THUMBNAIL_WIDTH + Self::CARD_PADDING * 2;
        let card_height = Self::THUMBNAIL_HEIGHT + Self::TEXT_LINES + Self::CARD_PADDING;

        let available_height = terminal_height.saturating_sub(Self::HEADER_HEIGHT + Self::FOOTER_HEIGHT);

        // Calculate how many columns fit
        let cols = (terminal_width / min_card_width).max(1) as usize;
        // Distribute width evenly
        let card_width = terminal_width / cols as u16;
        let visible_rows = (available_height / card_height).max(1) as usize;
        let visible_items = cols * visible_rows;

        Self {
            cols,
            card_width,
            card_height,
            visible_rows,
            visible_items,
        }
    }

    /// Convert terminal coordinates to video index
    pub fn coords_to_index(
        &self,
        x: u16,
        y: u16,
        scroll_offset: usize,
        total_items: usize,
    ) -> Option<usize> {
        // Account for header
        if y < Self::HEADER_HEIGHT {
            return None;
        }
        let y = y - Self::HEADER_HEIGHT;

        let col = (x / self.card_width) as usize;
        let row = (y / self.card_height) as usize;

        if col >= self.cols {
            return None;
        }

        let index = (scroll_offset + row) * self.cols + col;
        if index < total_items {
            Some(index)
        } else {
            None
        }
    }

    /// Get the area for a specific card
    pub fn card_area(&self, index: usize, scroll_offset: usize) -> Option<(u16, u16, u16, u16)> {
        let visible_index = index.checked_sub(scroll_offset * self.cols)?;
        let row = visible_index / self.cols;
        let col = visible_index % self.cols;

        if row >= self.visible_rows {
            return None;
        }

        let x = col as u16 * self.card_width;
        let y = Self::HEADER_HEIGHT + row as u16 * self.card_height;

        Some((x, y, self.card_width, self.card_height))
    }

    /// Width available for thumbnail inside card
    pub fn thumbnail_width(&self) -> u16 {
        self.card_width.saturating_sub(Self::CARD_PADDING * 2 + 2) // -2 for border
    }

    /// Height for thumbnail
    pub fn thumbnail_height(&self) -> u16 {
        Self::THUMBNAIL_HEIGHT
    }
}
