/// Calculates grid layout based on terminal dimensions
#[derive(Debug, Clone)]
pub struct GridLayout {
    /// Number of video cards per row
    pub cols: usize,
    /// Width of each card in terminal columns
    pub card_width: u16,
    /// Height of each card in terminal rows
    pub card_height: u16,
    /// Available height for the grid area (excluding header/footer)
    pub grid_height: u16,
    /// Horizontal offset to center the grid
    pub x_offset: u16,
}

/// Position within the grid, returned by hit-testing
#[derive(Debug, Clone, Copy)]
pub struct CardHit {
    /// Index in the videos list
    pub index: usize,
    /// Y position within the card (0 = top border)
    pub y_in_card: usize,
    /// X position within the card (0 = left border)
    pub x_in_card: usize,
}

impl GridLayout {
    // ═══════════════════════════════════════════════════════════════════════════
    // Card dimensions - all derived from thumbnail size
    // ═══════════════════════════════════════════════════════════════════════════

    /// Thumbnail height in terminal rows (half-block pixels)
    pub const THUMBNAIL_HEIGHT: u16 = 12;

    /// Thumbnail width: 16:9 aspect ratio, doubled for half-block chars
    pub const THUMBNAIL_WIDTH: u16 = (Self::THUMBNAIL_HEIGHT as f32 * 16.0 / 9.0 * 2.0) as u16;

    /// Card width = thumbnail + left/right borders
    const CARD_WIDTH: u16 = Self::THUMBNAIL_WIDTH + 2;

    /// Text overlay: checkbox row + 2 title lines + channel/time line
    const TEXT_OVERLAY_ROWS: u16 = 4;

    /// How many rows the text overlaps the thumbnail (gradient blend area)
    const TEXT_OVERLAP: u16 = 2;

    /// Card height = thumbnail + text rows (minus overlap) + bottom border
    /// No top border on cards - the selection indicator provides the top border when selected
    const CARD_HEIGHT: u16 =
        Self::THUMBNAIL_HEIGHT + Self::TEXT_OVERLAY_ROWS - Self::TEXT_OVERLAP + 1;

    // ═══════════════════════════════════════════════════════════════════════════
    // Fixed layout regions
    // ═══════════════════════════════════════════════════════════════════════════

    pub const HEADER_HEIGHT: u16 = 3;
    pub const FOOTER_HEIGHT: u16 = 1;
    /// Top padding in the grid content area to allow selection indicator top border
    pub const CONTENT_TOP_PADDING: u16 = 1;

    // ═══════════════════════════════════════════════════════════════════════════
    // Card-relative positions (y from top of card, x from left of card)
    // ═══════════════════════════════════════════════════════════════════════════

    /// Y position of the buttons row (checkbox, folder, sparkle) within a card
    /// This is the first row of the text overlay area (no top border, so no +1)
    const BUTTONS_ROW: u16 = Self::THUMBNAIL_HEIGHT - Self::TEXT_OVERLAP;

    /// X range for the watch later toggle (⊂⬤ or ⬤⊃), from right edge
    const TOGGLE_X_END: u16 = Self::CARD_WIDTH - 1; // before right border
    const TOGGLE_X_START: u16 = Self::TOGGLE_X_END - 4; // " ⊂⬤ " is 4 chars

    /// X range for the folder icon (🖬), from right edge
    const FOLDER_X_END: u16 = Self::TOGGLE_X_START;
    const FOLDER_X_START: u16 = Self::FOLDER_X_END - 3; // " 🖬 " is 3 chars

    /// X range for the transcript button (🗏), from right edge
    const TRANSCRIPT_X_END: u16 = Self::FOLDER_X_START;
    const TRANSCRIPT_X_START: u16 = Self::TRANSCRIPT_X_END - 3; // " 🗏 " is 3 chars

    /// X range for the sparkle button (✨), from right edge
    const SPARKLE_X_END: u16 = Self::TRANSCRIPT_X_START;
    const SPARKLE_X_START: u16 = Self::SPARKLE_X_END - 3; // " ✨ " is 3 chars

    // ═══════════════════════════════════════════════════════════════════════════
    // Construction
    // ═══════════════════════════════════════════════════════════════════════════

    pub fn calculate(terminal_width: u16, terminal_height: u16) -> Self {
        let grid_height = terminal_height.saturating_sub(Self::HEADER_HEIGHT + Self::FOOTER_HEIGHT);
        let cols = (terminal_width / Self::CARD_WIDTH).max(1) as usize;
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

    // ═══════════════════════════════════════════════════════════════════════════
    // Grid layout helpers - used by both drawing and hit-testing
    // ═══════════════════════════════════════════════════════════════════════════

    /// Vertical stride between card origins (no overlap, cards are stacked)
    #[inline]
    pub fn card_stride(&self) -> u16 {
        self.card_height
    }

    /// Calculate the screen position for a card at the given index
    #[inline]
    pub fn card_rect(&self, index: usize) -> (u16, u16) {
        let row = index / self.cols;
        let col = index % self.cols;
        let x = self.x_offset + col as u16 * self.card_width;
        let y = Self::CONTENT_TOP_PADDING + row as u16 * self.card_stride();
        (x, y)
    }

    /// Width available for thumbnail inside card (card_width minus borders)
    #[inline]
    pub fn thumbnail_width(&self) -> u16 {
        Self::THUMBNAIL_WIDTH
    }

    /// Height for thumbnail
    #[inline]
    pub fn thumbnail_height(&self) -> u16 {
        Self::THUMBNAIL_HEIGHT
    }

    /// Maximum scroll offset in lines
    pub fn max_scroll(&self, total_items: usize) -> usize {
        let total_rows = total_items.div_ceil(self.cols);
        let total_height =
            Self::CONTENT_TOP_PADDING as usize + total_rows * self.card_stride() as usize;
        total_height.saturating_sub(self.grid_height as usize)
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Hit testing - single source of truth for coordinate conversion
    // ═══════════════════════════════════════════════════════════════════════════

    /// Convert screen coordinates to card position, if the click is on a valid card
    pub fn hit_test(
        &self,
        x: u16,
        y: u16,
        scroll_offset: usize,
        total_items: usize,
    ) -> Option<CardHit> {
        // Must be below header
        if y < Self::HEADER_HEIGHT {
            return None;
        }

        // Must be within horizontally centered grid
        if x < self.x_offset {
            return None;
        }
        let x_in_grid = x - self.x_offset;

        // Column bounds check
        let col = (x_in_grid / self.card_width) as usize;
        if col >= self.cols {
            return None;
        }

        // Row calculation with scroll offset (account for top padding)
        let y_in_grid = (y - Self::HEADER_HEIGHT) as usize + scroll_offset;

        // Must be past the top padding area
        if y_in_grid < Self::CONTENT_TOP_PADDING as usize {
            return None;
        }

        let y_in_content = y_in_grid - Self::CONTENT_TOP_PADDING as usize;
        let stride = self.card_stride() as usize;
        let row = y_in_content / stride;

        // Index bounds check
        let index = row * self.cols + col;
        if index >= total_items {
            return None;
        }

        // Position within card
        let y_in_card = y_in_content % stride;
        let x_in_card = (x_in_grid % self.card_width) as usize;

        Some(CardHit {
            index,
            y_in_card,
            x_in_card,
        })
    }

    /// Convert terminal coordinates to video index
    pub fn coords_to_index(
        &self,
        x: u16,
        y: u16,
        scroll_offset: usize,
        total_items: usize,
    ) -> Option<usize> {
        self.hit_test(x, y, scroll_offset, total_items)
            .map(|hit| hit.index)
    }

    /// Check if coordinates are on the watch later toggle
    pub fn is_checkbox_click(
        &self,
        x: u16,
        y: u16,
        scroll_offset: usize,
        total_items: usize,
    ) -> Option<usize> {
        let hit = self.hit_test(x, y, scroll_offset, total_items)?;

        if hit.y_in_card != Self::BUTTONS_ROW as usize {
            return None;
        }

        let x = hit.x_in_card as u16;
        if x >= Self::TOGGLE_X_START && x < Self::TOGGLE_X_END {
            Some(hit.index)
        } else {
            None
        }
    }

    /// Check if coordinates are on the summary button (✨)
    pub fn is_summary_button_click(
        &self,
        x: u16,
        y: u16,
        scroll_offset: usize,
        total_items: usize,
    ) -> Option<usize> {
        let hit = self.hit_test(x, y, scroll_offset, total_items)?;

        if hit.y_in_card != Self::BUTTONS_ROW as usize {
            return None;
        }

        let x = hit.x_in_card as u16;
        if x >= Self::SPARKLE_X_START && x < Self::SPARKLE_X_END {
            Some(hit.index)
        } else {
            None
        }
    }

    /// Check if coordinates are on the transcript button (🗏)
    pub fn is_transcript_button_click(
        &self,
        x: u16,
        y: u16,
        scroll_offset: usize,
        total_items: usize,
    ) -> Option<usize> {
        let hit = self.hit_test(x, y, scroll_offset, total_items)?;

        if hit.y_in_card != Self::BUTTONS_ROW as usize {
            return None;
        }

        let x = hit.x_in_card as u16;
        if x >= Self::TRANSCRIPT_X_START && x < Self::TRANSCRIPT_X_END {
            Some(hit.index)
        } else {
            None
        }
    }
}
