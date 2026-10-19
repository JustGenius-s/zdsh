// The code in this file is adapted from the alacritty_terminal crate under the
// Apache license; see: crates/terminal_core/LICENSE-ALACRITTY.

/// Grid dimensions.
pub trait Dimensions {
    /// Total number of lines in the buffer, this includes scrollback and visible lines.
    fn total_rows(&self) -> usize;

    /// Number of rows in the viewport
    #[allow(dead_code)]
    fn visible_rows(&self) -> usize;

    /// Width of the terminal in columns.
    fn columns(&self) -> usize;

    /// Number of invisible lines part of the scrollback history.
    #[inline]
    fn history_size(&self) -> usize {
        self.total_rows() - self.visible_rows()
    }
}
