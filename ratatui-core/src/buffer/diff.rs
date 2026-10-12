use crate::buffer::{Buffer, Cell, CellDiffOption, CellWidth};
use crate::layout::Rect;
use crate::style::{Color, Modifier};

/// A zero-allocation iterator over the differences between two buffers of the same width.
///
/// Yields `(x, y, &Cell)` tuples for each cell in `next` that differs from the corresponding cell
/// in `prev`. [Continuation](Cell::is_continuation) cells are never yielded: the wide glyph they
/// belong to draws them, and writing to them would erase it. A continuation in `prev` that is a
/// cell of its own in `next` differs from it, so the columns freed by a wide glyph are redrawn
/// without special handling.
///
/// Before a wide glyph, the diff may yield blank cells over the columns the glyph covers, for
/// terminals that would otherwise leave stale content there (see `needs_clearing`).
#[derive(Debug)]
pub struct BufferDiff<'prev, 'next> {
    /// The next (current) buffer's cells.
    next: &'next [Cell],
    /// The previous buffer's cells.
    prev: &'prev [Cell],
    /// Buffer width (for `pos_of` calculation).
    area: Rect,
    /// Current position in the flat cell array.
    pos: usize,
    /// The wide glyph being drawn, whose other columns may need clearing first.
    clearing: Option<Clearing>,
}

/// The columns of a wide glyph still to check for clearing, and the glyph to draw after them.
#[derive(Debug)]
struct Clearing {
    columns: core::ops::Range<usize>,
    anchor: usize,
}

/// A blank cell, used to clear the columns of a wide glyph before drawing it.
static BLANK: Cell = Cell::EMPTY;

impl<'prev, 'next> BufferDiff<'prev, 'next> {
    /// Creates a new iterator over the differences between `prev` and `next` terminal cells.
    ///
    /// Heights may differ; the iterator uses the minimum of the two.
    ///
    /// # Panics
    ///
    /// Panics if the buffers have different `x`, `y`, or `width` values.
    pub(crate) fn new(prev: &'prev Buffer, next: &'next Buffer) -> Self {
        assert!(
            prev.area.x == next.area.x
                && prev.area.y == next.area.y
                && prev.area.width == next.area.width,
            "buffer areas must have the same x, y, and width: prev={:?}, next={:?}",
            prev.area,
            next.area,
        );

        let mut area = prev.area;
        area.height = area.height.min(next.area.height);

        Self {
            next: &next.content,
            prev: &prev.content,
            area,
            pos: 0,
            clearing: None,
        }
    }

    /// Converts a flat index to (x, y) coordinates.
    const fn pos_of(&self, index: usize) -> (u16, u16) {
        let w = self.area.width as usize;

        let x = index % w + self.area.x as usize;
        let y = index / w + self.area.y as usize;

        (x as u16, y as u16)
    }

    /// The item for `cell`, drawn at `index`.
    const fn item(&self, index: usize, cell: &'next Cell) -> (u16, u16, &'next Cell) {
        let (x, y) = self.pos_of(index);
        (x, y, cell)
    }
}

impl<'next> Iterator for BufferDiff<'_, 'next> {
    type Item = (u16, u16, &'next Cell);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(clearing) = &mut self.clearing {
            let anchor = &self.next[clearing.anchor];
            for column in clearing.columns.by_ref() {
                if needs_clearing(anchor, &self.prev[column]) {
                    return Some(self.item(column, &BLANK));
                }
            }
            let anchor = clearing.anchor;
            self.clearing = None;
            return Some(self.item(anchor, &self.next[anchor]));
        }

        let len = self.next.len().min(self.prev.len());
        while self.pos < len {
            let i = self.pos;
            self.pos += 1;

            let current = &self.next[i];
            if current.is_continuation()
                || is_skip(current)
                || (current.diff_option != CellDiffOption::AlwaysUpdate && *current == self.prev[i])
            {
                continue;
            }

            // A wide glyph's continuation cells end the glyph within its row.
            let width = current.cell_width() as usize;
            if width > 1 {
                self.clearing = Some(Clearing {
                    columns: i + 1..(i + width).min(len),
                    anchor: i,
                });
                return self.next();
            }
            return Some(self.item(i, current));
        }

        None
    }
}

/// Returns `true` if a column covered by the wide glyph `anchor` must be blanked before the glyph
/// is drawn, because drawing the glyph may leave `previous` visible there.
///
/// Drawing the glyph repaints its columns on most terminals, but not on all of them:
///
/// - Terminals disagree on the width of emoji presentation sequences (those containing VS16,
///   `U+FE0F`): some draw them in one column, and then show what the other column held before.
/// - Some terminals (the Windows console) keep the attributes of the other columns when a wide
///   glyph is redrawn, so a background or underline the glyph had there stays.
///
/// Blanking first is correct everywhere: a terminal that draws the glyph in full covers the
/// blank.
fn needs_clearing(anchor: &Cell, previous: &Cell) -> bool {
    if previous.is_continuation() {
        shows_on_blank(previous) && previous.style() != anchor.style()
    } else {
        (previous.symbol() != " " || shows_on_blank(previous))
            && has_uncertain_width(anchor.symbol())
    }
}

/// Returns `true` if the cell's style is visible on a blank cell.
fn shows_on_blank(cell: &Cell) -> bool {
    /// Modifiers that are visible on a blank cell.
    const VISIBLE_ON_BLANK: Modifier = Modifier::REVERSED
        .union(Modifier::UNDERLINED)
        .union(Modifier::SLOW_BLINK)
        .union(Modifier::RAPID_BLINK)
        .union(Modifier::CROSSED_OUT);
    cell.bg != Color::Reset || cell.modifier.intersects(VISIBLE_ON_BLANK)
}

/// Returns `true` if terminals disagree on how many columns `symbol` occupies.
///
/// Emoji presentation sequences (those containing VS16, `U+FE0F`) are the known case:
/// `unicode-width` and most modern terminals treat them as two columns, but some terminals draw
/// them in one. The diff cannot assume either, so it must emit updates that are correct under
/// both.
fn has_uncertain_width(symbol: &str) -> bool {
    symbol.chars().any(|c| c == '\u{FE0F}')
}

/// Returns `true` if this cell should be skipped during diffing.
#[allow(deprecated)]
const fn is_skip(cell: &Cell) -> bool {
    matches!(cell.diff_option, CellDiffOption::Skip)
        || (cell.skip && matches!(cell.diff_option, CellDiffOption::None))
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::num::NonZeroU16;

    use compact_str::CompactString;

    use super::*;
    use crate::buffer::Buffer;
    use crate::layout::Rect;
    use crate::style::{Color, Style};

    fn row(width: u16, content: &str, style: Style) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
        buf.set_string(0, 0, content, style);
        buf
    }

    #[test]
    fn empty_buffers_yield_no_diffs() {
        let rect = Rect::new(0, 0, 5, 1);
        let buf = Buffer::empty(rect);
        let diff: Vec<_> = BufferDiff::new(&buf, &buf).collect();
        assert_eq!(diff, Vec::new());
    }

    #[test]
    fn identical_buffers_yield_no_diffs() {
        let buf = Buffer::with_lines(["hello"]);
        let diff: Vec<_> = BufferDiff::new(&buf, &buf).collect();
        assert_eq!(diff, Vec::new());
    }

    /// The diff never yields a continuation cell, whatever the transition: the wide glyph draws
    /// those columns, and writing them would erase it.
    #[test]
    fn continuations_are_never_emitted() {
        let rows = [
            "你好，世界！",
            "a❤️b漢c😀d",
            "int value8;;",
            "❤️❤️❤️❤️❤️❤️",
            " 漢字 漢字 x",
        ];
        let styles = [Style::new(), Style::new().bg(Color::Blue)];
        for (prev, next) in rows.iter().flat_map(|a| rows.iter().map(move |b| (a, b))) {
            for (prev_style, next_style) in styles.iter().flat_map(|a| styles.map(|b| (a, b))) {
                let prev = row(12, prev, *prev_style);
                let next = row(12, next, next_style);
                for (x, y, cell) in BufferDiff::new(&prev, &next) {
                    assert!(
                        !cell.is_continuation(),
                        "{prev:?} -> {next:?} emitted a continuation at ({x}, {y})"
                    );
                }
            }
        }
    }

    /// Regression for <https://github.com/ratatui/ratatui/issues/2685>: an image protocol cell
    /// keeps its whole escape sequence in one symbol, which measures hundreds of columns wide
    /// while covering a single column. Advancing the cursor by that width dropped every later
    /// cell in the buffer, so the frame kept whatever was on screen before.
    #[test]
    fn long_symbol_does_not_hide_later_cells() {
        let area = Rect::new(0, 0, 20, 5);
        let prev = Buffer::filled(area, Cell::new("S"));
        let mut next = Buffer::filled(area, Cell::new("L"));
        next.cell_mut((10, 0))
            .unwrap()
            .set_symbol(&"\u{1b}[s".repeat(400));

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert_eq!(diff.len(), area.area() as usize);
    }

    /// The same cell, unchanged between the two frames. It still must not hide the cells after
    /// it, which is how the regression showed up once the image had been drawn once.
    #[test]
    fn unchanged_long_symbol_does_not_hide_later_cells() {
        let area = Rect::new(0, 0, 20, 5);
        let mut prev = Buffer::filled(area, Cell::new("S"));
        let mut next = Buffer::filled(area, Cell::new("L"));
        let payload = "\u{1b}[s".repeat(400);
        prev.cell_mut((10, 0)).unwrap().set_symbol(&payload);
        next.cell_mut((10, 0)).unwrap().set_symbol(&payload);

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert_eq!(diff.len(), area.area() as usize - 1);
    }

    /// A style change on a continuation cell is not drawn: the glyph draws that column.
    #[test]
    fn continuation_style_change_is_not_emitted() {
        let prev = row(3, "＋", Style::new());
        let mut next = prev.clone();
        next.cell_mut((1, 0)).unwrap().set_bg(Color::Red);

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert!(diff.is_empty(), "got {diff:?}");
    }

    /// A glyph declared wider than its text covers the declared columns.
    #[test]
    fn declared_width_covers_columns() {
        let prev = Buffer::with_lines(["abcd"]);
        let mut next = Buffer::with_lines(["xbcd"]);
        next.cell_mut((0, 0))
            .unwrap()
            .set_symbol_with_width("x", NonZeroU16::new(2).unwrap());

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert_eq!(diff, [(0, 0, &next[(0, 0)])]);
    }

    /// Regression for <https://github.com/ratatui/ratatui/issues/2652>: removing a style that is
    /// visible on blank cells from an unchanged wide glyph must clear its other column before
    /// repainting the glyph, for terminals that keep that column's attributes. Clearing it
    /// afterwards can erase the glyph on some terminals.
    #[test]
    fn wide_glyph_style_change_clears_trailing_cell_before_repaint() {
        for glyph in ["❤️", "😀"] {
            let prev = row(3, glyph, Style::new().bg(Color::Blue));
            let next = row(3, glyph, Style::new());

            let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

            assert_eq!(diff.len(), 2, "trailing cell and glyph must be redrawn");
            assert_eq!((diff[0].0, diff[0].1, diff[0].2.symbol()), (1, 0, " "));
            assert_eq!((diff[1].0, diff[1].1, diff[1].2.symbol()), (0, 0, glyph));
        }
    }

    #[test]
    fn wide_glyph_style_change_resumes_after_repaint() {
        let prev = row(3, "한a", Style::new().bg(Color::Blue));
        let next = row(3, "한b", Style::new().bg(Color::Red));

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        assert_eq!(diff.len(), 3);
        assert_eq!((diff[0].0, diff[0].1, diff[0].2.symbol()), (1, 0, " "));
        assert_eq!((diff[1].0, diff[1].1, diff[1].2.symbol()), (0, 0, "한"));
        assert_eq!((diff[2].0, diff[2].1, diff[2].2.symbol()), (2, 0, "b"));
        assert_eq!(diff[1].2.bg, Color::Red);
    }

    #[test]
    fn wide_glyph_foreground_change_does_not_clear_trailing_cell() {
        let prev = row(3, "한", Style::new().fg(Color::Blue));
        let next = row(3, "한", Style::new().fg(Color::Red));

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        assert_eq!(diff.len(), 1);
        assert_eq!((diff[0].0, diff[0].1, diff[0].2.symbol()), (0, 0, "한"));
    }

    #[test]
    fn single_cell_change() {
        let prev = Buffer::with_lines(["hello"]);
        let next = Buffer::with_lines(["hallo"]);
        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].0, 1); // x
        assert_eq!(diff[0].1, 0); // y
        assert_eq!(diff[0].2.symbol(), "a");
    }

    #[test]
    fn all_cells_changed() {
        let prev = Buffer::with_lines(["aaa"]);
        let next = Buffer::with_lines(["bbb"]);
        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert_eq!(diff.len(), 3);
    }

    #[test]
    fn skip_cells_are_skipped() {
        let prev = Buffer::with_lines(["abc"]);
        let mut next = Buffer::with_lines(["xyz"]);
        next.cell_mut((1, 0))
            .unwrap()
            .set_diff_option(CellDiffOption::Skip);

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert_eq!(diff.len(), 2);
        assert_eq!(diff[0].2.symbol(), "x");
        assert_eq!(diff[1].2.symbol(), "z");
    }

    #[test]
    fn always_update_cells_are_emitted_even_when_identical() {
        let mut prev = Buffer::with_lines(["abc"]);
        prev.cell_mut((1, 0))
            .unwrap()
            .set_diff_option(CellDiffOption::AlwaysUpdate);
        let next = prev.clone();

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].0, 1);
        assert_eq!(diff[0].1, 0);
        assert_eq!(diff[0].2.symbol(), "b");
    }

    #[test]
    fn vs16_trailing_cell_unchanged() {
        let mut prev = row(4, "⌨️", Style::new());
        prev.set_string(2, 0, "ab", Style::new());

        let mut next = row(4, "⌨️", Style::new().fg(Color::Red));
        next.set_string(2, 0, "ab", Style::new());

        // Only the emoji (0,0) differs (different style); its other column needs no clearing.
        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].0, 0);
        assert_eq!(diff[0].1, 0);
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_skip_field_is_respected() {
        let prev = Buffer::with_lines(["abc"]);
        let mut next = Buffer::with_lines(["xyz"]);
        next.content[1].skip = true;

        let diff: CompactString = BufferDiff::new(&prev, &next)
            .map(|(_, _, cell)| cell.symbol())
            .collect();

        assert_eq!(diff, "xz");
    }

    /// Regression test for the "uncovered trailing cell" bug.
    ///
    /// When a wide char with a non-default style (e.g. bg=Blue) is replaced by a narrow char,
    /// the terminal still shows the wide char's style on the column it covered. The column is a
    /// continuation in `prev` and a blank in `next`, so it is redrawn.
    ///
    /// With "你好，世界！" (Blue) replaced by "Hello" (default), "Hello" covers columns 0–4.
    /// Columns 5, 7, 9, 11 were covered by "，", "世", "界", "！" respectively.
    #[test]
    fn uncovered_trailing_cells_emitted_when_wide_char_style_changes() {
        let prev = row(12, "你好，世界！", Style::new().bg(Color::Blue));
        let next = row(12, "Hello", Style::default());

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        for x in [5u16, 7, 9, 11] {
            assert!(
                diff.iter().any(|(dx, dy, _)| *dx == x && *dy == 0),
                "expected update for trailing cell at x={x} (blue bg not cleared otherwise)"
            );
        }
    }

    /// Regression for a multi-step scenario that produces "Hello  ░" artifacts.
    ///
    /// "你好，世界！"(Blue) → "喵呜www"(Blue): "世"(cols 6-7) is replaced by "w"(Blue). Col 7,
    /// covered by "世" before, must be cleared.
    #[test]
    fn uncovered_trailing_cells_emitted_when_wide_chars_partially_replaced() {
        let prev = row(12, "你好，世界！", Style::new().bg(Color::Blue));
        let next = row(12, "喵呜www", Style::new().bg(Color::Blue));

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        assert!(
            diff.iter().any(|(dx, dy, _)| *dx == 7 && *dy == 0),
            "expected update for trailing cell at x=7 (blue bg not cleared otherwise)"
        );
    }

    /// Columns freed by a wide glyph are redrawn even when the glyph had no visible style: the
    /// buffer does not assume what a terminal shows in a column whose glyph was overwritten.
    #[test]
    fn freed_columns_are_redrawn() {
        let prev = row(12, "你好，世界！", Style::default());
        let next = row(12, "Hello", Style::default());

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        let columns: Vec<u16> = diff.iter().map(|(x, _, _)| *x).collect();
        assert_eq!(columns, (0..12).collect::<Vec<_>>());
    }

    /// Regression for <https://github.com/ratatui/ratatui/issues/2585>: when a wide glyph is
    /// replaced by narrower content, the column it covered must be redrawn.
    #[test]
    fn shrinking_wide_glyph_clears_trailing_cell() {
        let rect = Rect::new(0, 0, 2, 1);
        let mut prev = row(2, "＋", Style::new());
        prev.cell_mut((0, 0)).unwrap().set_bg(Color::Red);
        assert!(prev[(1, 0)].is_continuation());

        let next = Buffer::empty(rect);
        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        assert_eq!(diff, [(0, 0, &Cell::EMPTY), (1, 0, &Cell::EMPTY)]);
    }

    #[test]
    fn shrinking_wide_glyph_clears_trailing_background() {
        let prev = row(2, "＋", Style::new().bg(Color::Blue));
        let next = Buffer::empty(Rect::new(0, 0, 2, 1));

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        assert!(
            diff.iter().any(|(x, y, _)| *x == 1 && *y == 0),
            "trailing cell (1,0) must be redrawn to clear the leftover background, got {diff:?}"
        );
    }

    /// A cell written over the right half of a wide glyph replaces the glyph, and a later style
    /// change on that cell is drawn.
    #[test]
    fn cell_written_over_continuation_is_emitted() {
        let mut prev = row(3, "你", Style::new().bg(Color::Blue));
        prev.cell_mut((1, 0))
            .unwrap()
            .set_symbol("X")
            .set_fg(Color::Red)
            .set_bg(Color::Reset);

        let mut next = row(3, "a", Style::default());
        next.cell_mut((1, 0))
            .unwrap()
            .set_symbol("X")
            .set_fg(Color::Green);

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        assert!(
            diff.iter().any(|(x, y, _)| *x == 1 && *y == 0),
            "col 1 style change (Red→Green) must be emitted; got {diff:?}"
        );
    }

    /// prev="你好"(Blue) → next="a好"(Blue): '好' moves to col 1, and col 2 is its continuation,
    /// which must not receive a blank that would erase it.
    #[test]
    fn moved_wide_glyph_continuation_is_not_overwritten() {
        let prev = row(4, "你好", Style::new().bg(Color::Blue));
        let next = row(4, "a好", Style::new().bg(Color::Blue));

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        assert!(
            !diff.iter().any(|(x, y, _)| *x == 2 && *y == 0),
            "continuation of '好' must not receive a write; got {diff:?}"
        );
        assert!(
            diff.iter()
                .any(|(x, y, cell)| *x == 1 && *y == 0 && cell.symbol() == "好"),
            "'好' at col 1 must be emitted; got {diff:?}"
        );
    }

    /// Regression for <https://github.com/ratatui/ratatui/issues/2695>: a wide glyph with a
    /// visible style in the last cell of the buffer, replaced by narrower content, must not read
    /// past the end of the buffer. The buffer clips the glyph to the one column it has.
    #[test]
    fn wide_glyph_in_last_cell_does_not_overrun_buffer() {
        let rect = Rect::new(0, 0, 3, 1);
        let mut prev = Buffer::empty(rect);
        prev.cell_mut((2, 0))
            .unwrap()
            .set_symbol("你")
            .set_bg(Color::Red);
        let mut next = Buffer::empty(rect);
        next.cell_mut((2, 0)).unwrap().set_symbol("a");

        let diff: Vec<_> = BufferDiff::new(&prev, &next).collect();

        assert_eq!(
            diff.len(),
            1,
            "only the replaced cell must be emitted, got {diff:?}"
        );
        assert_eq!((diff[0].0, diff[0].1), (2, 0));
        assert_eq!(diff[0].2.symbol(), "a");
    }

    #[test]
    #[should_panic(expected = "buffer areas must have the same x, y, and width")]
    fn mismatched_widths_panics() {
        let prev = Buffer::empty(Rect::new(0, 0, 5, 1));
        let next = Buffer::empty(Rect::new(0, 0, 10, 1));
        BufferDiff::new(&prev, &next);
    }
}

/// Replays diffs through a model backend with real cursor semantics, and checks the resulting
/// screen against what the buffer says the row should look like.
///
/// Backends skip a cursor move only when the previous symbol has a predictable terminal width.
/// VS16 clusters force a move because their actual cursor advance may differ from the buffer.
///
/// Terminals disagree on the width of a cluster carrying VS16 (`U+FE0F`), so the model is
/// parameterised over both observed behaviours; every scenario must pass under both.
#[cfg(test)]
mod terminal_model {
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use rstest::rstest;

    use super::*;
    use crate::buffer::Buffer;
    use crate::style::Style;

    /// What is visibly occupying one terminal column.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Column {
        Blank,
        Glyph(String),
        /// This column is painted by the multi-column glyph to its left.
        Continuation,
    }

    impl Column {
        fn from_symbol(symbol: &str) -> Self {
            // A continuation cell draws nothing of its own.
            if symbol == " " || symbol.is_empty() {
                Self::Blank
            } else {
                Self::Glyph(symbol.to_string())
            }
        }
    }

    /// How many columns a terminal advances after printing a VS16 grapheme cluster.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Vs16Width {
        /// VS16 clusters advance two columns (tmux, most modern terminals).
        TwoColumns,
        /// VS16 clusters advance one column (the terminals
        /// <https://github.com/ratatui/ratatui/pull/2063> was filed against).
        OneColumn,
    }

    const VS16_WIDTHS: [Vs16Width; 2] = [Vs16Width::TwoColumns, Vs16Width::OneColumn];

    fn contains_vs16(symbol: &str) -> bool {
        symbol.chars().any(|c| c == '\u{FE0F}')
    }

    impl Vs16Width {
        /// Columns the cursor advances when `symbol` is printed, given the width the buffer
        /// reserved for it.
        fn advance(self, symbol: &str, reserved: usize) -> usize {
            // Keep this model independent from the production width detection so it remains a
            // useful oracle if that detection regresses.
            if self == Self::OneColumn && reserved > 1 && contains_vs16(symbol) {
                1
            } else {
                reserved
            }
        }
    }

    /// A single row of terminal columns.
    #[derive(Debug)]
    struct TerminalRow {
        columns: Vec<Column>,
    }

    impl TerminalRow {
        /// Prints `symbol` at `col`, advancing the cursor by `advance` columns.
        ///
        /// Overwriting either half of a multi-column glyph destroys the whole glyph, so its full
        /// footprint is blanked before the new symbol is written.
        fn print(&mut self, col: usize, symbol: &str, advance: usize) {
            let len = self.columns.len();
            if col >= len {
                return;
            }
            let end = (col + advance).min(len);

            let mut lo = col;
            while lo > 0 && self.columns[lo] == Column::Continuation {
                lo -= 1;
            }
            let mut hi = end;
            while hi < len && self.columns[hi] == Column::Continuation {
                hi += 1;
            }
            self.columns[lo..hi].fill(Column::Blank);

            self.columns[col] = Column::from_symbol(symbol);
            self.columns[col + 1..end].fill(Column::Continuation);
        }
    }

    /// The columns a buffer row occupies on screen for the given VS16 width.
    ///
    /// A glyph paints its own column plus every column it advances over; columns the buffer
    /// reserved but the terminal did not cover show the buffer cell's own content.
    fn expected_columns(vs16_width: Vs16Width, buf: &Buffer) -> Vec<Column> {
        let width = buf.area.width as usize;
        let mut columns = vec![Column::Blank; width];
        let mut i = 0;
        while i < width {
            let cell = &buf.content[i];
            let reserved = (cell.cell_width() as usize).max(1);
            let advance = vs16_width.advance(cell.symbol(), reserved);
            columns[i] = Column::from_symbol(cell.symbol());
            for (c, column) in columns
                .iter_mut()
                .enumerate()
                .take((i + reserved).min(width))
                .skip(i + 1)
            {
                *column = if c < i + advance {
                    Column::Continuation
                } else {
                    Column::from_symbol(buf.content[c].symbol())
                };
            }
            i += reserved;
        }
        columns
    }

    /// Replays `BufferDiff` the way a backend does and returns the resulting screen.
    ///
    /// The screen starts as `prev` rendered with `vs16_width`, so any column the diff never
    /// rewrites keeps the previous frame's content — a stale column surviving into the comparison
    /// *is* the artifact.
    fn replay(vs16_width: Vs16Width, prev: &Buffer, next: &Buffer) -> Vec<Column> {
        assert_eq!(prev.area.height, 1, "harness models a single row");
        let mut terminal = TerminalRow {
            columns: expected_columns(vs16_width, prev),
        };
        let mut cursor = 0usize;
        let mut last: Option<(u16, usize)> = None;

        for (x, _y, cell) in BufferDiff::new(prev, next) {
            // Reposition unless the backend can predict the terminal's cursor position.
            let col = if matches!(last, Some((p, width)) if usize::from(p) + width == usize::from(x))
            {
                cursor
            } else {
                x as usize
            };
            let reserved = (cell.cell_width() as usize).max(1);
            let advance = vs16_width.advance(cell.symbol(), reserved);
            terminal.print(col, cell.symbol(), advance);
            cursor = col + advance;
            // Model the backend rule independently from BufferDiff's production helper.
            let uncertain_width = reserved > 1 && contains_vs16(cell.symbol());
            last = (!uncertain_width).then_some((x, reserved));
        }
        terminal.columns
    }

    #[track_caller]
    fn assert_no_artifacts(vs16_width: Vs16Width, prev: &Buffer, next: &Buffer) {
        let expected = expected_columns(vs16_width, next);
        let actual = replay(vs16_width, prev, next);
        assert_eq!(
            actual, expected,
            "{vs16_width:?}: terminal differs from buffer"
        );
    }

    #[track_caller]
    fn assert_no_artifacts_for_both_widths(prev: &str, next: &str, width: u16) {
        for vs16_width in VS16_WIDTHS {
            assert_no_artifacts(vs16_width, &row(width, prev), &row(width, next));
        }
    }

    fn row(width: u16, content: &str) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
        buf.set_string(0, 0, content, Style::default());
        buf
    }

    /// <https://github.com/ratatui/ratatui/issues/2357>: updating an ASCII row to VS16 emoji.
    #[rstest]
    #[case::heart("❤️❤️❤️❤️❤️❤️")]
    #[case::rocket("🚀️🚀️🚀️🚀️🚀️🚀️")]
    fn vs16_emoji_replacing_ascii_does_not_drift(#[case] next: &str) {
        assert_no_artifacts_for_both_widths("int value8;;", next, 12);
    }

    /// Same as above with keycap sequences, which carry VS16 plus a combining enclosing keycap.
    #[test]
    fn vs16_keycaps_replacing_ascii_do_not_drift() {
        assert_no_artifacts_for_both_widths("int value8;;", "1️⃣1️⃣1️⃣1️⃣1️⃣1️⃣", 12);
    }

    /// A VS16 row redrawn one column over: every glyph moves, so no update may inherit an offset.
    #[test]
    fn shifted_vs16_row_does_not_drift() {
        assert_no_artifacts_for_both_widths("❤️❤️❤️❤️❤️", " ❤️❤️❤️❤️❤️", 12);
    }

    /// <https://github.com/ratatui/ratatui/pull/2063>: a terminal that draws a VS16 cluster in a
    /// single column leaves the buffer's reserved column showing the old content unless the diff
    /// writes it. Regression guard: deleting the VS16 special case to fix #2357 breaks this.
    #[test]
    fn vs16_emoji_replacing_narrower_content_leaves_no_stale_column() {
        assert_no_artifacts_for_both_widths("ab", "❤️", 2);
        assert_no_artifacts_for_both_widths("abcd", "❤️cd", 4);
        assert_no_artifacts_for_both_widths("abcd", "ab❤️", 4);
    }

    /// A stale symbol (here a border) under the right half of a VS16 emoji used to be re-sent by
    /// the diff, and broke the emoji. Writing the emoji makes that column a continuation, so the
    /// border character is gone from the buffer and the emoji is drawn whole.
    #[test]
    fn vs16_emoji_over_stale_border_is_drawn_correctly() {
        let prev = row(4, "────");
        let mut next = row(4, "────");
        next.cell_mut((0, 0)).unwrap().set_symbol("❤️");
        assert!(next[(1, 0)].is_continuation());
        for vs16_width in VS16_WIDTHS {
            assert_no_artifacts(vs16_width, &prev, &next);
        }
        assert_eq!(
            replay(Vs16Width::TwoColumns, &prev, &next),
            [
                Column::Glyph("❤️".into()),
                Column::Continuation,
                Column::Glyph("─".into()),
                Column::Glyph("─".into()),
            ]
        );
    }

    /// Mixed rows: narrow, certain-width (`漢`, `😀`) and uncertain-width clusters interleaved.
    ///
    /// Also pins that certain-width clusters keep skipping their own reserved column: they are
    /// not routed through the clear-first path, and must not drift either.
    #[test]
    fn mixed_width_rows_do_not_drift() {
        assert_no_artifacts_for_both_widths("int value8;;", "a❤️b漢c😀d", 12);
        assert_no_artifacts_for_both_widths("a❤️b漢c😀d", "int value8;;", 12);
        assert_no_artifacts_for_both_widths("a❤️b漢c😀d", "漢a❤️b😀c", 12);
    }
}
