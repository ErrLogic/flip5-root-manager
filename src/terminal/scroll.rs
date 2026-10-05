//! A small, bounded scroll-offset primitive shared by any fixed-item-count
//! scrollable view (the terminal output log and the workflow step list).
//!
//! `offset` is always the index of the first visible item. It is kept
//! clamped to `[0, max_scroll]` where `max_scroll = max(total_items -
//! viewport, 0)`, so a viewport can never be positioned past the point
//! where it would show blank space below the last item.

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScrollState {
    pub offset: usize,
}

impl ScrollState {
    pub fn max_scroll(total_items: usize, viewport: usize) -> usize {
        total_items.saturating_sub(viewport)
    }

    /// Clamps the current offset into the valid range for the given
    /// item count/viewport. Call this whenever either changes (new log
    /// lines arrived, the terminal was resized, ...) before reading
    /// `offset`.
    pub fn clamp(&mut self, total_items: usize, viewport: usize) {
        self.offset = self.offset.min(Self::max_scroll(total_items, viewport));
    }

    pub fn page_up(&mut self, total_items: usize, viewport: usize) {
        self.clamp(total_items, viewport);
        self.offset = self.offset.saturating_sub(viewport.max(1));
    }

    pub fn page_down(&mut self, total_items: usize, viewport: usize) {
        let max_scroll = Self::max_scroll(total_items, viewport);
        self.offset = self.offset.saturating_add(viewport.max(1)).min(max_scroll);
    }

    pub fn jump_to_top(&mut self) {
        self.offset = 0;
    }

    pub fn jump_to_bottom(&mut self, total_items: usize, viewport: usize) {
        self.offset = Self::max_scroll(total_items, viewport);
    }

    pub fn is_at_bottom(&self, total_items: usize, viewport: usize) -> bool {
        self.offset >= Self::max_scroll(total_items, viewport)
    }

    /// Adjusts the offset by the minimal amount needed so that item
    /// `index` is visible within the viewport, without moving it more
    /// than necessary (so it doesn't fight a manual scroll any more than
    /// it has to).
    pub fn ensure_visible(&mut self, index: usize, total_items: usize, viewport: usize) {
        self.clamp(total_items, viewport);
        if viewport == 0 {
            return;
        }
        if index < self.offset {
            self.offset = index;
        } else if index >= self.offset + viewport {
            self.offset = index.saturating_sub(viewport - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_scroll_is_zero_when_content_fits_in_viewport() {
        assert_eq!(ScrollState::max_scroll(5, 20), 0);
        assert_eq!(ScrollState::max_scroll(20, 20), 0);
    }

    #[test]
    fn matches_the_documented_example() {
        // content = 100 lines, viewport = 20 lines -> max_scroll = 80.
        assert_eq!(ScrollState::max_scroll(100, 20), 80);
        let mut s = ScrollState { offset: 1000 };
        s.clamp(100, 20);
        assert_eq!(s.offset, 80);
    }

    #[test]
    fn page_down_from_top_advances_by_one_viewport() {
        let mut s = ScrollState::default();
        s.page_down(100, 20);
        assert_eq!(s.offset, 20);
    }

    #[test]
    fn repeated_page_down_stops_exactly_at_max_scroll() {
        let mut s = ScrollState::default();
        for _ in 0..10 {
            s.page_down(100, 20);
        }
        assert_eq!(s.offset, 80);
    }

    #[test]
    fn page_up_from_bottom_goes_up_by_one_viewport() {
        let mut s = ScrollState { offset: 80 };
        s.page_up(100, 20);
        assert_eq!(s.offset, 60);
    }

    #[test]
    fn repeated_page_up_stops_exactly_at_zero() {
        let mut s = ScrollState { offset: 80 };
        for _ in 0..10 {
            s.page_up(100, 20);
        }
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn offset_never_moves_when_content_is_smaller_than_viewport() {
        let mut s = ScrollState::default();
        s.page_down(5, 20);
        assert_eq!(s.offset, 0);
        s.page_up(5, 20);
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn ensure_visible_scrolls_down_to_reveal_a_later_item() {
        let mut s = ScrollState::default();
        s.ensure_visible(50, 100, 20);
        // Item 50 must now be within [offset, offset+20).
        assert!(s.offset <= 50 && 50 < s.offset + 20);
    }

    #[test]
    fn ensure_visible_scrolls_up_to_reveal_an_earlier_item() {
        let mut s = ScrollState { offset: 50 };
        s.ensure_visible(5, 100, 20);
        assert_eq!(s.offset, 5);
    }

    #[test]
    fn ensure_visible_does_nothing_when_already_visible() {
        let mut s = ScrollState { offset: 10 };
        s.ensure_visible(15, 100, 20);
        assert_eq!(s.offset, 10);
    }
}
