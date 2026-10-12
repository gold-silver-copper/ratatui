use crate::buffer::{Cell, CellDiffOption, CellWidth};

/// A cell that is drawn to the terminal, as yielded by [`visible_cells`].
#[derive(Debug)]
pub(crate) struct VisibleCell<'a> {
    /// Index of the cell in the flat cell slice.
    pub index: usize,
    /// Number of columns the cell covers, counting the hidden cells after it. Never zero, and
    /// never past the end of the row.
    pub width: usize,
    pub cell: &'a Cell,
}

/// Walks rows of `width` cells and yields only the cells that get drawn to the terminal.
///
/// A wide glyph (or a cell with [`CellDiffOption::ForcedWidth`]) is stored as the glyph's cell
/// followed by cells that are hidden behind it. Those hidden cells are left out: drawing one after
/// the glyph would overwrite the glyph. A glyph wider than the rest of its row only hides the
/// cells up to the end of that row. Skipped cells ([`CellDiffOption::Skip`] or the deprecated
/// [`Cell::skip`]) are left out too, but they hide nothing.
pub(crate) fn visible_cells(cells: &[Cell], width: u16) -> VisibleCells<'_> {
    let width = usize::from(width).max(1);
    VisibleCells {
        cells,
        width,
        index: 0,
        row_end: width,
    }
}

/// The iterator returned by [`visible_cells`].
#[derive(Debug)]
pub(crate) struct VisibleCells<'a> {
    cells: &'a [Cell],
    width: usize,
    /// Index of the next cell to look at.
    index: usize,
    /// Index of the first cell after the row of `index`.
    row_end: usize,
}

impl<'a> Iterator for VisibleCells<'a> {
    type Item = VisibleCell<'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let index = self.index;
            let cell = self.cells.get(index)?;
            if index == self.row_end {
                self.row_end += self.width;
            }
            if is_skip(cell) {
                self.index += 1;
                continue;
            }
            let width = usize::from(cell.cell_width()).clamp(1, self.row_end - index);
            self.index += width;
            return Some(VisibleCell { index, width, cell });
        }
    }
}

/// Returns `true` if the cell must not be drawn.
#[allow(deprecated)]
pub(crate) const fn is_skip(cell: &Cell) -> bool {
    matches!(cell.diff_option, CellDiffOption::Skip)
        || (cell.skip && matches!(cell.diff_option, CellDiffOption::None))
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::buffer::Buffer;

    fn visible(buffer: &Buffer) -> Vec<(usize, usize, &str)> {
        visible_cells(&buffer.content, buffer.area.width)
            .map(|c| (c.index, c.width, c.cell.symbol()))
            .collect()
    }

    #[test]
    fn hidden_cells_are_left_out() {
        let buffer = Buffer::with_lines(["界a", "b界"]);
        assert_eq!(
            visible(&buffer),
            [(0, 2, "界"), (2, 1, "a"), (3, 1, "b"), (4, 2, "界")]
        );
    }

    /// A wide glyph in the last column hides nothing on the next row.
    #[test]
    fn wide_glyph_ending_a_row_does_not_hide_the_next_row() {
        let mut buffer = Buffer::with_lines(["ab", "cd"]);
        buffer.content[1].set_symbol("界");
        assert_eq!(
            visible(&buffer),
            [(0, 1, "a"), (1, 1, "界"), (2, 1, "c"), (3, 1, "d")]
        );
    }

    #[test]
    fn skipped_cells_are_left_out_but_hide_nothing() {
        let mut buffer = Buffer::with_lines(["界ab"]);
        buffer.content[0].set_diff_option(CellDiffOption::Skip);
        assert_eq!(visible(&buffer), [(1, 1, " "), (2, 1, "a"), (3, 1, "b")]);
    }
}
