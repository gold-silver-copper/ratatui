use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::num::NonZeroU16;
use core::ops::{Deref, Index};

use unicode_segmentation::UnicodeSegmentation;

use crate::buffer::cell::Span as CellSpan;
use crate::buffer::{BufferDiff, Cell, CellDiffOption, CellWidth};
use crate::layout::{Position, Rect};
use crate::style::{Color, Style};
use crate::symbols::merge::MergeStrategy;
use crate::text::{Line, Span};

/// A buffer that maps to the desired content of the terminal after the draw call
///
/// No widget in the library interacts directly with the terminal. Instead each of them is required
/// to draw their state to an intermediate buffer. It is basically a grid where each cell contains
/// a grapheme, a foreground color and a background color. This grid will then be used to output
/// the appropriate escape sequences and characters to draw the UI as the user has defined it.
///
/// # Examples:
///
/// ```
/// use ratatui_core::buffer::{Buffer, Cell};
/// use ratatui_core::layout::{Position, Rect};
/// use ratatui_core::style::{Color, Style};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut buf = Buffer::empty(Rect {
///     x: 0,
///     y: 0,
///     width: 10,
///     height: 5,
/// });
///
/// // writing a cell; `None` if the position is outside the buffer
/// buf.cell_mut(Position { x: 0, y: 0 })
///     .ok_or("cell not found")?
///     .set_symbol("A");
/// buf.cell_mut((0, 1))
///     .ok_or("cell not found")?
///     .set_symbol("B");
///
/// // reading a cell by indexing with a Position or an (x, y) tuple
/// assert_eq!(buf[Position { x: 0, y: 0 }].symbol(), "A");
/// assert_eq!(buf[(0, 1)].symbol(), "B");
///
/// // getting an Option instead of panicking if the position is outside the buffer
/// let cell = buf.cell(Position { x: 0, y: 1 }).ok_or("cell not found")?;
/// assert_eq!(cell.symbol(), "B");
///
/// buf.set_string(
///     3,
///     0,
///     "string",
///     Style::default().fg(Color::Red).bg(Color::White),
/// );
/// let cell = &buf[(5, 0)]; // cannot move out of buf, so we borrow it
/// assert_eq!(cell.symbol(), "r");
/// assert_eq!(cell.fg, Color::Red);
/// assert_eq!(cell.bg, Color::White);
/// # Ok(())
/// # }
/// ```
///
/// # Wide glyphs
///
/// A glyph wider than one column, such as `界` or `❤️`, is stored in the cell where it starts.
/// The other columns it covers hold [continuation](Cell::is_continuation) cells, which have no
/// symbol of their own and are never drawn. Every write goes through the buffer, which keeps
/// that true:
///
/// - Writing a wide glyph makes the cells it covers continuations, up to the end of the row.
/// - Writing over any part of a wide glyph (its first cell or a continuation) removes the whole
///   glyph: its other cells become blanks that keep their style, as in a terminal.
/// - A wide glyph has one style, shared by its continuation cells. Styling its first cell styles
///   the glyph; styling a continuation cell does nothing.
///
/// That is why the buffer hands out a [`CellMut`] rather than a `&mut Cell`.
///
/// ```
/// use ratatui_core::buffer::Buffer;
/// use ratatui_core::layout::Rect;
/// use ratatui_core::style::Style;
///
/// let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
/// buf.set_string(0, 0, "界", Style::new());
/// assert!(buf[(1, 0)].is_continuation());
///
/// // Writing over the right half of `界` removes the whole glyph.
/// buf.cell_mut((1, 0)).unwrap().set_symbol("a");
/// assert_eq!(buf, Buffer::with_lines([" a  "]));
/// ```
#[derive(Default, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Buffer {
    /// The area represented by this buffer
    pub area: Rect,
    /// The content of the buffer, row by row. Its length is always `area.width * area.height`.
    ///
    /// Private so that writes go through the buffer, which keeps continuation cells next to the
    /// wide glyphs they belong to. Code in this crate may move, copy or clear whole rows, which
    /// keeps that true; anything else must use [`Buffer::cell_mut`].
    pub(crate) content: Vec<Cell>,
}

impl Buffer {
    /// Returns a Buffer with all cells set to the default one
    #[must_use]
    pub fn empty(area: Rect) -> Self {
        Self::filled(area, Cell::EMPTY)
    }

    /// Returns a Buffer with all cells initialized with the attributes of the given Cell
    ///
    /// A wide `cell` is repeated every `width` columns, with continuation cells in between. A
    /// continuation `cell` has nothing to draw and gives an empty buffer.
    #[must_use]
    pub fn filled(area: Rect, cell: Cell) -> Self {
        if cell.is_continuation() {
            return Self::empty(area);
        }
        if cell.columns() == 1 {
            let content = vec![cell; area.area() as usize];
            return Self { area, content };
        }
        let mut buffer = Self::empty(area);
        for y in area.top()..area.bottom() {
            for x in (area.left()..area.right()).step_by(usize::from(cell.columns())) {
                buffer.write(Position { x, y }, |c| *c = cell.clone());
            }
        }
        buffer
    }

    /// Returns a Buffer containing the given lines
    #[must_use]
    pub fn with_lines<'a, Iter>(lines: Iter) -> Self
    where
        Iter: IntoIterator,
        Iter::Item: Into<Line<'a>>,
    {
        let lines = lines.into_iter().map(Into::into).collect::<Vec<_>>();
        let height = lines.len() as u16;
        let width = lines.iter().map(Line::width).max().unwrap_or_default() as u16;
        let mut buffer = Self::empty(Rect::new(0, 0, width, height));
        for (y, line) in lines.iter().enumerate() {
            buffer.set_line(0, y as u16, line, width);
        }
        buffer
    }

    /// Returns the content of the buffer as a slice
    pub fn content(&self) -> &[Cell] {
        &self.content
    }

    /// Returns the area covered by this buffer
    pub const fn area(&self) -> &Rect {
        &self.area
    }

    /// Returns a reference to the [`Cell`] at the given coordinates
    ///
    /// Callers should use [`Buffer[]`](Self::index) or [`Buffer::cell`] instead of this method.
    ///
    /// Note: idiomatically methods named `get` usually return `Option<&T>`, but this method panics
    /// instead. This is kept for backwards compatibility. See [`cell`](Self::cell) for a safe
    /// alternative.
    ///
    /// # Panics
    ///
    /// Panics if the index is out of bounds.
    #[track_caller]
    #[deprecated = "use `Buffer[(x, y)]` instead. To avoid panicking, use `Buffer::cell((x, y))`. Both methods take `impl Into<Position>`."]
    #[must_use]
    pub fn get(&self, x: u16, y: u16) -> &Cell {
        let i = self.index_of(x, y);
        &self.content[i]
    }

    /// Returns a reference to the [`Cell`] at the given position or [`None`] if the position is
    /// outside the `Buffer`'s area.
    ///
    /// This method accepts any value that can be converted to [`Position`] (e.g. `(x, y)` or
    /// `Position::new(x, y)`).
    ///
    /// For a method that panics when the position is outside the buffer instead of returning
    /// `None`, use [`Buffer[]`](Self::index).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use ratatui_core::buffer::{Buffer, Cell};
    /// use ratatui_core::layout::{Position, Rect};
    ///
    /// let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 10));
    ///
    /// assert_eq!(buffer.cell(Position::new(0, 0)), Some(&Cell::default()));
    /// assert_eq!(buffer.cell(Position::new(10, 10)), None);
    /// assert_eq!(buffer.cell((0, 0)), Some(&Cell::default()));
    /// assert_eq!(buffer.cell((10, 10)), None);
    /// ```
    #[must_use]
    pub fn cell<P: Into<Position>>(&self, position: P) -> Option<&Cell> {
        let position = position.into();
        let index = self.index_of_opt(position)?;
        self.content.get(index)
    }

    /// Returns a [`CellMut`] for writing the [`Cell`] at the given position, or [`None`] if the
    /// position is outside the `Buffer`'s area.
    ///
    /// This method accepts any value that can be converted to [`Position`] (e.g. `(x, y)` or
    /// `Position::new(x, y)`).
    ///
    /// The buffer does not hand out `&mut Cell`: changing a cell's symbol can change how many
    /// columns it covers, and the buffer has to update the neighbouring cells to match. See
    /// [wide glyphs](Buffer#wide-glyphs).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use ratatui_core::buffer::{Buffer, Cell};
    /// use ratatui_core::layout::{Position, Rect};
    /// use ratatui_core::style::{Color, Style};
    /// let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 10));
    ///
    /// if let Some(mut cell) = buffer.cell_mut(Position::new(0, 0)) {
    ///     cell.set_symbol("A")
    ///         .set_style(Style::default().fg(Color::Red));
    /// }
    /// assert_eq!(buffer[(0, 0)].symbol(), "A");
    /// assert!(buffer.cell_mut((10, 10)).is_none());
    /// ```
    #[must_use]
    pub fn cell_mut<P: Into<Position>>(&mut self, position: P) -> Option<CellMut<'_>> {
        let index = self.index_of_opt(position.into())?;
        Some(CellMut {
            buffer: self,
            index,
        })
    }

    /// Writes the cell at `position` with `update`, which must leave it drawing a glyph, and
    /// keeps the cells around it consistent. Positions outside the buffer are ignored.
    ///
    /// This is the one place that changes how many columns a glyph covers:
    ///
    /// - If the cell was covered by a wide glyph, that glyph's cells to the left become blank.
    /// - The cells the new glyph covers become its continuations, clipped to the row; a glyph cut
    ///   short by the edge of the buffer declares the columns it kept.
    /// - Continuations right after the new glyph belonged to a glyph it overwrote, and become
    ///   blank.
    fn write(&mut self, position: Position, update: impl FnOnce(&mut Cell)) {
        self.write_measured(position, None, update);
    }

    /// [`write`](Self::write), for a caller that already measured the new glyph's width.
    fn write_measured(
        &mut self,
        position: Position,
        measured: Option<u16>,
        update: impl FnOnce(&mut Cell),
    ) {
        let Some(index) = self.index_of_opt(position) else {
            return;
        };
        let width = usize::from(self.area.width);
        let row_end = index - index % width + width;

        if self.content[index].is_continuation() {
            let row_start = index - index % width;
            let anchor = self.content[row_start..index]
                .iter()
                .rposition(|cell| !cell.is_continuation())
                .map_or(row_start, |offset| row_start + offset);
            for cell in &mut self.content[anchor..index] {
                cell.make_blank();
            }
        }

        let anchor = &mut self.content[index];
        update(anchor);
        let columns = usize::from(measured.map_or_else(|| anchor.columns(), |width| width.max(1)));
        let end = (index + columns).min(row_end);
        if end - index < columns {
            // `end - index` is at least one and at most the buffer width.
            if let Some(kept) = NonZeroU16::new((end - index) as u16) {
                anchor.set_span(CellSpan::Fixed(kept));
            }
        }
        let style = anchor.style();
        for cell in &mut self.content[index + 1..end] {
            cell.make_continuation(style);
        }
        for cell in self.content[end..row_end]
            .iter_mut()
            .take_while(|cell| cell.is_continuation())
        {
            cell.make_blank();
        }
    }

    /// Returns the index in the `Vec<Cell>` for the given global (x, y) coordinates.
    ///
    /// Global coordinates are offset by the Buffer's area offset (`x`/`y`).
    ///
    /// Usage discouraged, as it exposes `self.content` as a linearly indexable array, which limits
    /// potential future abstractions. See <https://github.com/ratatui/ratatui/issues/1122>.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui_core::buffer::Buffer;
    /// use ratatui_core::layout::Rect;
    ///
    /// let buffer = Buffer::empty(Rect::new(200, 100, 10, 10));
    /// // Global coordinates to the top corner of this buffer's area
    /// assert_eq!(buffer.index_of(200, 100), 0);
    /// ```
    ///
    /// # Panics
    ///
    /// Panics when given an coordinate that is outside of this Buffer's area.
    ///
    /// ```should_panic
    /// use ratatui_core::buffer::Buffer;
    /// use ratatui_core::layout::Rect;
    ///
    /// let buffer = Buffer::empty(Rect::new(200, 100, 10, 10));
    /// // Top coordinate is outside of the buffer in global coordinate space, as the Buffer's area
    /// // starts at (200, 100).
    /// buffer.index_of(0, 0); // Panics
    /// ```
    #[track_caller]
    #[must_use]
    pub fn index_of(&self, x: u16, y: u16) -> usize {
        self.index_of_opt(Position { x, y }).unwrap_or_else(|| {
            panic!(
                "index outside of buffer: the area is {area:?} but index is ({x}, {y})",
                area = self.area,
            )
        })
    }

    /// Returns the index in the `Vec<Cell>` for the given global (x, y) coordinates.
    ///
    /// Returns `None` if the given coordinates are outside of the Buffer's area.
    ///
    /// Note that this is private because of <https://github.com/ratatui/ratatui/issues/1122>
    #[must_use]
    const fn index_of_opt(&self, position: Position) -> Option<usize> {
        let area = self.area;
        if !area.contains(position) {
            return None;
        }
        // remove offset
        let y = (position.y - self.area.y) as usize;
        let x = (position.x - self.area.x) as usize;
        let width = self.area.width as usize;
        Some(y * width + x)
    }

    /// Returns the (global) coordinates of a cell given its index.
    ///
    /// Global coordinates are offset by the Buffer's area offset (`x`/`y`).
    ///
    /// Usage discouraged, as it exposes `self.content` as a linearly indexable array, which limits
    /// potential future abstractions. See <https://github.com/ratatui/ratatui/issues/1122>.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui_core::buffer::Buffer;
    /// use ratatui_core::layout::Rect;
    ///
    /// let rect = Rect::new(200, 100, 10, 10);
    /// let buffer = Buffer::empty(rect);
    /// assert_eq!(buffer.pos_of(0), (200, 100));
    /// assert_eq!(buffer.pos_of(14), (204, 101));
    /// ```
    ///
    /// # Panics
    ///
    /// Panics when given an index that is outside the Buffer's content.
    ///
    /// ```should_panic
    /// use ratatui_core::buffer::Buffer;
    /// use ratatui_core::layout::Rect;
    ///
    /// let rect = Rect::new(0, 0, 10, 10); // 100 cells in total
    /// let buffer = Buffer::empty(rect);
    /// // Index 100 is the 101th cell, which lies outside of the area of this Buffer.
    /// buffer.pos_of(100); // Panics
    /// ```
    #[must_use]
    pub fn pos_of(&self, index: usize) -> (u16, u16) {
        debug_assert!(
            index < self.content.len(),
            "Trying to get the coords of a cell outside the buffer: i={index} len={}",
            self.content.len()
        );
        let x = index % self.area.width as usize + self.area.x as usize;
        let y = index / self.area.width as usize + self.area.y as usize;
        (
            u16::try_from(x).expect("x overflow. This should never happen as area.width is u16"),
            u16::try_from(y).expect("y overflow. This should never happen as area.height is u16"),
        )
    }

    /// Print a string, starting at the position (x, y)
    pub fn set_string<T, S>(&mut self, x: u16, y: u16, string: T, style: S)
    where
        T: AsRef<str>,
        S: Into<Style>,
    {
        self.set_stringn(x, y, string, usize::MAX, style);
    }

    /// Print at most the first n characters of a string if enough space is available
    /// until the end of the line. Skips zero-width graphemes and control characters.
    ///
    /// Use [`Buffer::set_string`] when the maximum amount of characters can be printed.
    pub fn set_stringn<T, S>(
        &mut self,
        mut x: u16,
        y: u16,
        string: T,
        max_width: usize,
        style: S,
    ) -> (u16, u16)
    where
        T: AsRef<str>,
        S: Into<Style>,
    {
        let max_width = max_width.try_into().unwrap_or(u16::MAX);
        let mut remaining_width = self.area.right().saturating_sub(x).min(max_width);
        let graphemes = UnicodeSegmentation::graphemes(string.as_ref(), true)
            .filter(|symbol| !symbol.contains(char::is_control))
            .map(|symbol| (symbol, symbol.cell_width()))
            .filter(|(_symbol, width)| *width > 0)
            .map_while(|(symbol, width)| {
                remaining_width = remaining_width.checked_sub(width)?;
                Some((symbol, width))
            });
        let style = style.into();
        for (symbol, width) in graphemes {
            self.write_measured(Position { x, y }, Some(width), |cell| {
                cell.set_symbol(symbol).set_style(style);
            });
            x += width;
        }
        (x, y)
    }

    /// Print a line, starting at the position (x, y)
    pub fn set_line(&mut self, x: u16, y: u16, line: &Line<'_>, max_width: u16) -> (u16, u16) {
        let mut remaining_width = max_width;
        let mut x = x;
        for span in line {
            if remaining_width == 0 {
                break;
            }
            let pos = self.set_stringn(
                x,
                y,
                span.content.as_ref(),
                remaining_width as usize,
                line.style.patch(span.style),
            );
            let w = pos.0.saturating_sub(x);
            x = pos.0;
            remaining_width = remaining_width.saturating_sub(w);
        }
        (x, y)
    }

    /// Print a span, starting at the position (x, y)
    pub fn set_span(&mut self, x: u16, y: u16, span: &Span<'_>, max_width: u16) -> (u16, u16) {
        self.set_stringn(x, y, &span.content, max_width as usize, span.style)
    }

    /// Set the style of all cells in the given area.
    ///
    /// `style` accepts any type that is convertible to [`Style`] (e.g. [`Style`], [`Color`], or
    /// your own type that implements [`Into<Style>`]).
    ///
    /// [`Color`]: crate::style::Color
    pub fn set_style<S: Into<Style>>(&mut self, area: Rect, style: S) {
        let style = style.into();
        let area = self.area.intersection(area);
        for position in area.positions() {
            self.style_glyph(position, style);
        }
    }

    /// Patches the style of the glyph starting at `position` and of its continuation cells,
    /// which always share the style of their glyph. Does nothing on a continuation cell.
    fn style_glyph(&mut self, position: Position, style: Style) {
        let Some(index) = self.index_of_opt(position) else {
            return;
        };
        let (anchor, rest) = self.content[index..].split_at_mut(1);
        let anchor = &mut anchor[0];
        if anchor.is_continuation() {
            return;
        }
        anchor.set_style(style);
        let style = anchor.style();
        let row_end = usize::from(self.area.width) - index % usize::from(self.area.width) - 1;
        for cell in rest[..row_end]
            .iter_mut()
            .take_while(|cell| cell.is_continuation())
        {
            cell.make_continuation(style);
        }
    }

    /// Resize the buffer so that the mapped area matches the given area and that the buffer
    /// length is equal to area.width * area.height
    ///
    /// Cells keep their offset from the top-left corner of the area. A wide glyph cut by the new
    /// right edge is clipped.
    pub fn resize(&mut self, area: Rect) {
        if area.width == self.area.width {
            self.content.resize(area.area() as usize, Cell::EMPTY);
            self.area = area;
            return;
        }
        let mut old = core::mem::replace(self, Self::empty(area));
        old.area.x = area.x;
        old.area.y = area.y;
        self.copy_from(&old);
    }

    /// Reset all cells in the buffer
    pub fn reset(&mut self) {
        for cell in &mut self.content {
            cell.reset();
        }
    }

    /// Merge an other buffer into this one
    ///
    /// The result covers both areas. Cells of `other` are written over this buffer's cells.
    pub fn merge(&mut self, other: &Self) {
        let area = self.area.union(other.area);
        let old = core::mem::replace(self, Self::empty(area));
        self.copy_from(&old);
        self.copy_from(other);
    }

    /// Appends `symbol`, typically a zero-width grapheme, to the glyph covering `position`.
    pub(crate) fn append_symbol(&mut self, position: Position, symbol: &str, style: Style) {
        let Some(index) = self.index_of_opt(position) else {
            return;
        };
        let row_start = index - index % usize::from(self.area.width);
        let anchor = self.content[row_start..=index]
            .iter()
            .rposition(|cell| !cell.is_continuation())
            .map_or(index, |offset| row_start + offset);
        let (x, y) = self.pos_of(anchor);
        self.write(Position { x, y }, |cell| {
            cell.append_symbol(symbol).set_style(style);
        });
    }

    /// Writes every glyph of `other` to the same position in this buffer.
    fn copy_from(&mut self, other: &Self) {
        let positions = other.area.positions();
        for (position, cell) in positions.zip(&other.content) {
            if !cell.is_continuation() {
                self.write(position, |c| c.clone_from(cell));
            }
        }
    }

    /// Collects the diff between `self` and `other` into a `Vec`.
    ///
    /// This is a convenience wrapper around [`diff_iter`](Self::diff_iter) that collects the
    /// results. Prefer `diff_iter` to avoid the intermediate allocation.
    ///
    /// # Panics
    ///
    /// Panics if the two buffers have different `x`, `y`, or `width` values.
    pub fn diff<'a>(&self, other: &'a Self) -> Vec<(u16, u16, &'a Cell)> {
        self.diff_iter(other).collect()
    }

    /// Builds a minimal sequence of coordinates and Cells necessary to update the UI from
    /// self to other.
    ///
    /// [Continuation](Cell::is_continuation) cells are never part of the diff: the wide glyph
    /// they belong to draws them.
    ///
    /// # Multi-width characters handling:
    ///
    /// ```text
    /// (Index:) `01`
    /// Prev:    `コ`
    /// Next:    `aa`
    /// Updates: `0: a, 1: a'
    /// ```
    ///
    /// ```text
    /// (Index:) `01`
    /// Prev:    `a `
    /// Next:    `コ`
    /// Updates: `0: コ` (index 1 is a continuation)
    /// ```
    ///
    /// ```text
    /// (Index:) `012`
    /// Prev:    `aaa`
    /// Next:    `aコ`
    /// Updates: `0: a, 1: コ` (index 2 is a continuation)
    /// ```
    /// # Panics
    ///
    /// Panics if the two buffers have different `x`, `y`, or `width` values.
    pub fn diff_iter<'prev, 'next>(&'prev self, other: &'next Self) -> BufferDiff<'prev, 'next> {
        BufferDiff::new(self, other)
    }
}

/// Write access to one [`Cell`] of a [`Buffer`], from [`Buffer::cell_mut`].
///
/// It has the same setters as [`Cell`], and dereferences to the cell for reading. Setting a
/// symbol also updates the neighbouring cells, so that a wide glyph always covers its
/// continuation cells and no cell is left half-covered: see [wide glyphs](Buffer#wide-glyphs).
/// Style setters style the whole glyph.
///
/// # Examples
///
/// ```
/// use ratatui_core::buffer::Buffer;
/// use ratatui_core::layout::Rect;
/// use ratatui_core::style::Color;
///
/// let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
/// if let Some(mut cell) = buf.cell_mut((0, 0)) {
///     cell.set_symbol("界").set_fg(Color::Red);
/// }
/// assert_eq!(buf[(0, 0)].symbol(), "界");
/// assert!(buf[(1, 0)].is_continuation());
/// ```
#[derive(Debug)]
pub struct CellMut<'a> {
    buffer: &'a mut Buffer,
    index: usize,
}

impl CellMut<'_> {
    fn position(&self) -> Position {
        let (x, y) = self.buffer.pos_of(self.index);
        Position { x, y }
    }

    fn cell(&mut self) -> &mut Cell {
        &mut self.buffer.content[self.index]
    }

    fn write(&mut self, update: impl FnOnce(&mut Cell)) -> &mut Self {
        let position = self.position();
        self.buffer.write(position, update);
        self
    }

    /// Sets the symbol of the cell, keeping its style.
    ///
    /// A wide symbol turns the cells it covers into continuations. Writing over part of a wide
    /// glyph removes that glyph.
    pub fn set_symbol(&mut self, symbol: &str) -> &mut Self {
        self.write(|cell| {
            cell.set_symbol(symbol);
        })
    }

    /// Sets the symbol of the cell to a single character, keeping its style.
    ///
    /// See [`set_symbol`](Self::set_symbol).
    pub fn set_char(&mut self, ch: char) -> &mut Self {
        self.write(|cell| {
            cell.set_char(ch);
        })
    }

    /// Sets a symbol that covers `width` columns, whatever its text says.
    ///
    /// Use it for symbols whose text width does not match what they draw, such as the escape
    /// sequence of an image protocol: the image covers `width` columns, which become
    /// continuations (up to the end of the row), so nothing else is drawn over it. Rows below
    /// an image are not covered; use [`CellDiffOption::Skip`] there.
    pub fn set_symbol_with_width(&mut self, symbol: &str, width: NonZeroU16) -> &mut Self {
        self.write(|cell| {
            cell.set_symbol(symbol).set_span(CellSpan::Fixed(width));
        })
    }

    /// Merges `symbol` with the cell's symbol. See [`Cell::merge_symbol`].
    pub fn merge_symbol(&mut self, symbol: &str, strategy: MergeStrategy) -> &mut Self {
        self.write(|cell| {
            cell.merge_symbol(symbol, strategy);
        })
    }

    /// Replaces the whole cell.
    ///
    /// A [continuation](Cell::is_continuation) cell has nothing of its own to draw, so writing
    /// one changes nothing. This lets a row of cells read from a buffer be written to another
    /// one cell by cell.
    pub fn set_cell(&mut self, cell: &Cell) -> &mut Self {
        if cell.is_continuation() {
            return self;
        }
        self.write(|c| c.clone_from(cell))
    }

    /// Resets the cell to a blank, unstyled cell. Writing over part of a wide glyph removes that
    /// glyph.
    pub fn reset(&mut self) -> &mut Self {
        self.write(Cell::reset)
    }

    /// Sets the foreground color of the cell. See [`set_style`](Self::set_style).
    pub fn set_fg(&mut self, color: Color) -> &mut Self {
        self.set_style(color)
    }

    /// Sets the background color of the cell. See [`set_style`](Self::set_style).
    pub fn set_bg(&mut self, color: Color) -> &mut Self {
        self.set_style(Style::new().bg(color))
    }

    /// Sets the style of the cell. See [`Cell::set_style`].
    ///
    /// A wide glyph has one style: setting the style of its first cell sets it for the whole
    /// glyph, and setting the style of a continuation cell does nothing.
    pub fn set_style<S: Into<Style>>(&mut self, style: S) -> &mut Self {
        let position = self.position();
        self.buffer.style_glyph(position, style.into());
        self
    }

    /// Sets the cell's [`CellDiffOption`].
    pub fn set_diff_option(&mut self, diff_option: CellDiffOption) -> &mut Self {
        self.cell().set_diff_option(diff_option);
        self
    }
}

impl Deref for CellMut<'_> {
    type Target = Cell;

    fn deref(&self) -> &Cell {
        &self.buffer.content[self.index]
    }
}

impl<P: Into<Position>> Index<P> for Buffer {
    type Output = Cell;

    /// Returns a reference to the [`Cell`] at the given position.
    ///
    /// This method accepts any value that can be converted to [`Position`] (e.g. `(x, y)` or
    /// `Position::new(x, y)`).
    ///
    /// # Panics
    ///
    /// May panic if the given position is outside the buffer's area. For a method that returns
    /// `None` instead of panicking, use [`Buffer::cell`](Self::cell).
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui_core::buffer::{Buffer, Cell};
    /// use ratatui_core::layout::{Position, Rect};
    ///
    /// let buf = Buffer::empty(Rect::new(0, 0, 10, 10));
    /// let cell = &buf[(0, 0)];
    /// let cell = &buf[Position::new(0, 0)];
    /// ```
    fn index(&self, position: P) -> &Self::Output {
        let position = position.into();
        let index = self.index_of(position.x, position.y);
        &self.content[index]
    }
}

impl fmt::Debug for Buffer {
    /// Writes a debug representation of the buffer to the given formatter.
    ///
    /// The format is like a pretty printed struct, with the following fields:
    /// * `area`: displayed as `Rect { x: 1, y: 2, width: 3, height: 4 }`
    /// * `content`: displayed as a list of strings representing the content of the buffer
    /// * `styles`: displayed as a list of: `{ x: 1, y: 2, fg: Color::Red, bg: Color::Blue,
    ///   modifier: Modifier::BOLD }` only showing a value when there is a change in style.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("Buffer {{\n    area: {:?}", self.area))?;

        if self.area.is_empty() {
            return f.write_str("\n}");
        }

        f.write_str(",\n    content: [\n")?;
        let mut last_style = None;
        let mut styles = vec![];
        for (y, line) in self.content.chunks(self.area.width as usize).enumerate() {
            f.write_str("        \"")?;
            for (x, c) in line.iter().enumerate() {
                f.write_str(c.symbol())?;
                #[cfg(feature = "underline-color")]
                {
                    let style = (c.fg, c.bg, c.underline_color, c.modifier);
                    if last_style != Some(style) {
                        last_style = Some(style);
                        styles.push((x, y, c.fg, c.bg, c.underline_color, c.modifier));
                    }
                }
                #[cfg(not(feature = "underline-color"))]
                {
                    let style = (c.fg, c.bg, c.modifier);
                    if last_style != Some(style) {
                        last_style = Some(style);
                        styles.push((x, y, c.fg, c.bg, c.modifier));
                    }
                }
            }
            f.write_str("\",\n")?;
        }
        f.write_str("    ],\n    styles: [\n")?;
        for s in styles {
            #[cfg(feature = "underline-color")]
            f.write_fmt(format_args!(
                "        x: {}, y: {}, fg: {:?}, bg: {:?}, underline: {:?}, modifier: {:?},\n",
                s.0, s.1, s.2, s.3, s.4, s.5
            ))?;
            #[cfg(not(feature = "underline-color"))]
            f.write_fmt(format_args!(
                "        x: {}, y: {}, fg: {:?}, bg: {:?}, modifier: {:?},\n",
                s.0, s.1, s.2, s.3, s.4
            ))?;
        }
        f.write_str("    ]\n}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "std"))]
    extern crate std;

    use alloc::format;
    use alloc::string::{String, ToString};
    use core::iter;
    use core::num::NonZeroU16;
    use std::{dbg, println};

    use itertools::Itertools;
    use rstest::{fixture, rstest};

    use super::*;
    use crate::buffer::CellDiffOption;
    use crate::style::{Color, Modifier, Stylize};

    #[test]
    fn debug_empty_buffer() {
        let buffer = Buffer::empty(Rect::ZERO);
        let result = format!("{buffer:?}");
        println!("{result}");
        let expected = "Buffer {\n    area: Rect { x: 0, y: 0, width: 0, height: 0 }\n}";
        assert_eq!(result, expected);
    }

    #[cfg(feature = "underline-color")]
    #[test]
    fn debug_grapheme_override() {
        let buffer = Buffer::with_lines(["a🦀b"]);
        let result = format!("{buffer:?}");
        println!("{result}");
        let expected = indoc::indoc!(
            r#"
            Buffer {
                area: Rect { x: 0, y: 0, width: 4, height: 1 },
                content: [
                    "a🦀b",
                ],
                styles: [
                    x: 0, y: 0, fg: Reset, bg: Reset, underline: Reset, modifier: NONE,
                ]
            }"#
        );
        assert_eq!(result, expected);
    }

    #[test]
    fn debug_some_example() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 2));
        buffer.set_string(0, 0, "Hello World!", Style::default());
        buffer.set_string(
            0,
            1,
            "G'day World!",
            Style::default()
                .fg(Color::Green)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
        let result = format!("{buffer:?}");
        println!("{result}");
        #[cfg(feature = "underline-color")]
        let expected = indoc::indoc!(
            r#"
            Buffer {
                area: Rect { x: 0, y: 0, width: 12, height: 2 },
                content: [
                    "Hello World!",
                    "G'day World!",
                ],
                styles: [
                    x: 0, y: 0, fg: Reset, bg: Reset, underline: Reset, modifier: NONE,
                    x: 0, y: 1, fg: Green, bg: Yellow, underline: Reset, modifier: BOLD,
                ]
            }"#
        );
        #[cfg(not(feature = "underline-color"))]
        let expected = indoc::indoc!(
            r#"
            Buffer {
                area: Rect { x: 0, y: 0, width: 12, height: 2 },
                content: [
                    "Hello World!",
                    "G'day World!",
                ],
                styles: [
                    x: 0, y: 0, fg: Reset, bg: Reset, modifier: NONE,
                    x: 0, y: 1, fg: Green, bg: Yellow, modifier: BOLD,
                ]
            }"#
        );

        assert_eq!(result, expected);
    }

    #[test]
    fn it_translates_to_and_from_coordinates() {
        let rect = Rect::new(200, 100, 50, 80);
        let buf = Buffer::empty(rect);

        // First cell is at the upper left corner.
        assert_eq!(buf.pos_of(0), (200, 100));
        assert_eq!(buf.index_of(200, 100), 0);

        // Last cell is in the lower right.
        assert_eq!(buf.pos_of(buf.content.len() - 1), (249, 179));
        assert_eq!(buf.index_of(249, 179), buf.content.len() - 1);
    }

    #[test]
    #[should_panic(expected = "outside the buffer")]
    fn pos_of_panics_on_out_of_bounds() {
        let rect = Rect::new(0, 0, 10, 10);
        let buf = Buffer::empty(rect);

        // There are a total of 100 cells; zero-indexed means that 100 would be the 101st cell.
        let _ = buf.pos_of(100);
    }

    #[rstest]
    #[case::left(9, 10)]
    #[case::top(10, 9)]
    #[case::right(20, 10)]
    #[case::bottom(10, 20)]
    #[should_panic(
        expected = "index outside of buffer: the area is Rect { x: 10, y: 10, width: 10, height: 10 } but index is"
    )]
    fn index_of_panics_on_out_of_bounds(#[case] x: u16, #[case] y: u16) {
        let _ = Buffer::empty(Rect::new(10, 10, 10, 10)).index_of(x, y);
    }

    #[test]
    fn test_cell() {
        let buf = Buffer::with_lines(["Hello", "World"]);

        let mut expected = Cell::default();
        expected.set_symbol("H");

        assert_eq!(buf.cell((0, 0)), Some(&expected));
        assert_eq!(buf.cell((10, 10)), None);
        assert_eq!(buf.cell(Position::new(0, 0)), Some(&expected));
        assert_eq!(buf.cell(Position::new(10, 10)), None);
    }

    #[test]
    fn test_cell_mut() {
        let mut buf = Buffer::with_lines(["Hello", "World"]);

        let mut expected = Cell::default();
        expected.set_symbol("H");

        assert_eq!(buf.cell_mut((0, 0)).as_deref(), Some(&expected));
        assert!(buf.cell_mut((10, 10)).is_none());
        assert_eq!(
            buf.cell_mut(Position::new(0, 0)).as_deref(),
            Some(&expected)
        );
        assert!(buf.cell_mut(Position::new(10, 10)).is_none());
    }

    #[test]
    fn index() {
        let buf = Buffer::with_lines(["Hello", "World"]);

        let mut expected = Cell::default();
        expected.set_symbol("H");

        assert_eq!(buf[(0, 0)], expected);
    }

    #[rstest]
    #[case::left(9, 10)]
    #[case::top(10, 9)]
    #[case::right(20, 10)]
    #[case::bottom(10, 20)]
    #[should_panic(
        expected = "index outside of buffer: the area is Rect { x: 10, y: 10, width: 10, height: 10 } but index is"
    )]
    fn index_out_of_bounds_panics(#[case] x: u16, #[case] y: u16) {
        let rect = Rect::new(10, 10, 10, 10);
        let buf = Buffer::empty(rect);
        let _ = buf[(x, y)];
    }

    /// The cells of row `y`, as a terminal shows them: symbols, with `>` for continuations.
    fn row_view(buf: &Buffer, y: u16) -> String {
        (buf.area.left()..buf.area.right())
            .map(|x| &buf[(x, y)])
            .map(|cell| {
                if cell.is_continuation() {
                    ">"
                } else {
                    cell.symbol()
                }
            })
            .collect()
    }

    #[test]
    fn wide_write_marks_continuations() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        buf.set_string(0, 0, "界a😀", Style::new());
        assert_eq!(row_view(&buf, 0), "界>a😀>");
        assert_eq!(buf[(1, 0)].symbol(), "");
        assert_eq!(buf[(1, 0)].cell_width(), 0);

        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf.cell_mut((0, 0)).unwrap().set_symbol("❤️");
        assert_eq!(row_view(&buf, 0), "❤️> ");
    }

    /// Writing over the first cell of a wide glyph removes the glyph: its continuation becomes a
    /// blank in the glyph's style.
    #[test]
    fn overwriting_anchor_resets_glyph() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf.set_string(0, 0, "界", Style::new().bg(Color::Blue));
        buf.cell_mut((0, 0)).unwrap().set_symbol("x");
        assert_eq!(row_view(&buf, 0), "x  ");
        assert!(!buf[(1, 0)].is_continuation());
        assert_eq!(buf[(1, 0)].bg, Color::Blue);
    }

    /// Writing over a continuation removes the glyph it belongs to, like a terminal does.
    #[test]
    fn overwriting_continuation_resets_glyph() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf.set_string(0, 0, "界", Style::new().bg(Color::Blue));
        buf.cell_mut((1, 0)).unwrap().set_symbol("x");
        assert_eq!(row_view(&buf, 0), " x ");
        assert_eq!(buf[(0, 0)].bg, Color::Blue);

        // `reset`, as used by `Clear`, does the same.
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf.set_string(0, 0, "界", Style::new());
        buf.cell_mut((1, 0)).unwrap().reset();
        assert_eq!(buf, Buffer::empty(Rect::new(0, 0, 3, 1)));
    }

    /// A wide glyph written across another one removes it, including its continuations past the
    /// new glyph.
    #[test]
    fn wide_glyph_over_wide_glyphs() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        buf.set_string(0, 0, "界界", Style::new());
        buf.set_string(1, 0, "漢", Style::new());
        assert_eq!(row_view(&buf, 0), " 漢>  ");

        // A narrow glyph over a narrow cell leaves its neighbours alone.
        let mut buf = Buffer::with_lines(["界ab"]);
        buf.set_string(2, 0, "x", Style::new());
        assert_eq!(row_view(&buf, 0), "界>xb");
    }

    /// A wide glyph that does not fit in its row is clipped: it covers the columns that are left
    /// and says so, and never covers a cell of the next row.
    #[test]
    fn wide_glyph_clipped_at_row_end() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 2));
        buf.cell_mut((2, 0)).unwrap().set_symbol("界");
        assert_eq!(buf[(2, 0)].symbol(), "界");
        assert_eq!(buf[(2, 0)].cell_width(), 1);
        assert!(!buf[(0, 1)].is_continuation());

        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        let width = NonZeroU16::new(3).unwrap();
        buf.cell_mut((1, 0))
            .unwrap()
            .set_symbol_with_width("\x1b[s", width);
        assert_eq!(row_view(&buf, 0), " \x1b[s>> ");
        assert_eq!(buf[(1, 0)].cell_width(), 3);
        buf.cell_mut((3, 0))
            .unwrap()
            .set_symbol_with_width("\x1b[s", width);
        assert_eq!(row_view(&buf, 0), "   \x1b[s>");
        assert_eq!(buf[(3, 0)].cell_width(), 2);
    }

    /// A wide glyph has one style: styling its first cell styles the glyph, styling its
    /// continuation does nothing.
    #[test]
    fn wide_glyph_has_one_style() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf.set_string(0, 0, "界", Style::new());
        buf.cell_mut((0, 0)).unwrap().set_bg(Color::Red);
        assert_eq!(buf[(1, 0)].bg, Color::Red);
        buf.cell_mut((1, 0)).unwrap().set_bg(Color::Blue);
        assert_eq!(buf[(1, 0)].bg, Color::Red);

        // An area that starts in the middle of the glyph leaves it alone.
        buf.set_style(Rect::new(1, 0, 2, 1), Style::new().bg(Color::Green));
        assert_eq!((buf[(0, 0)].bg, buf[(1, 0)].bg), (Color::Red, Color::Red));
        assert_eq!(buf[(2, 0)].bg, Color::Green);
        // An area that ends in the middle of the glyph styles all of it.
        buf.set_style(Rect::new(0, 0, 1, 1), Style::new().bg(Color::Yellow));
        assert_eq!(
            (buf[(0, 0)].bg, buf[(1, 0)].bg),
            (Color::Yellow, Color::Yellow)
        );
    }

    /// Copying a row cell by cell copies its wide glyphs whole: writing a continuation is a
    /// no-op, the glyph before it already wrote it.
    #[test]
    fn set_cell_copies_wide_glyphs() {
        let source = Buffer::with_lines(["a界b😀"]);
        let mut copy = Buffer::with_lines(["xxxxxx"]);
        for (position, cell) in source.area.positions().zip(source.content()) {
            copy.cell_mut(position).unwrap().set_cell(cell);
        }
        assert_eq!(copy, source);

        let mut buf = Buffer::with_lines(["xyz"]);
        buf.cell_mut((1, 0)).unwrap().set_cell(&source[(2, 0)]);
        assert_eq!(buf, Buffer::with_lines(["xyz"]));
    }

    #[test]
    fn merge_keeps_wide_glyphs_whole() {
        let mut buf = Buffer::with_lines(["界界"]);
        let mut other = Buffer::empty(Rect::new(1, 0, 2, 1));
        other.set_string(1, 0, "ab", Style::new());
        buf.merge(&other);
        assert_eq!(row_view(&buf, 0), " ab ");
    }

    #[test]
    fn resize_clips_wide_glyph() {
        let mut buf = Buffer::with_lines(["a界", "界b"]);
        buf.resize(Rect::new(0, 0, 2, 2));
        assert_eq!(row_view(&buf, 0), "a界");
        assert_eq!(buf[(1, 0)].cell_width(), 1);
        assert_eq!(row_view(&buf, 1), "界>");
    }

    #[test]
    fn filled_with_wide_cell() {
        let buf = Buffer::filled(Rect::new(0, 0, 5, 2), Cell::new("界"));
        assert_eq!(row_view(&buf, 0), "界>界>界");
        assert_eq!(row_view(&buf, 1), "界>界>界");
    }

    #[test]
    fn cell_mut_writes() {
        let mut buf = Buffer::with_lines(["Cat", "Dog"]);
        buf.cell_mut((0, 0)).unwrap().set_symbol("B");
        buf.cell_mut(Position::new(0, 1)).unwrap().set_char('L');
        assert_eq!(buf, Buffer::with_lines(["Bat", "Log"]));
    }

    #[test]
    fn set_string() {
        let area = Rect::new(0, 0, 5, 1);
        let mut buffer = Buffer::empty(area);

        // Zero-width
        buffer.set_stringn(0, 0, "aaa", 0, Style::default());
        assert_eq!(buffer, Buffer::with_lines(["     "]));

        buffer.set_string(0, 0, "aaa", Style::default());
        assert_eq!(buffer, Buffer::with_lines(["aaa  "]));

        // Width limit:
        buffer.set_stringn(0, 0, "bbbbbbbbbbbbbb", 4, Style::default());
        assert_eq!(buffer, Buffer::with_lines(["bbbb "]));

        buffer.set_string(0, 0, "12345", Style::default());
        assert_eq!(buffer, Buffer::with_lines(["12345"]));

        // Width truncation:
        buffer.set_string(0, 0, "123456", Style::default());
        assert_eq!(buffer, Buffer::with_lines(["12345"]));

        // multi-line
        buffer = Buffer::empty(Rect::new(0, 0, 5, 2));
        buffer.set_string(0, 0, "12345", Style::default());
        buffer.set_string(0, 1, "67890", Style::default());
        assert_eq!(buffer, Buffer::with_lines(["12345", "67890"]));
    }

    #[test]
    fn set_string_multi_width_overwrite() {
        let area = Rect::new(0, 0, 5, 1);
        let mut buffer = Buffer::empty(area);

        // multi-width overwrite
        buffer.set_string(0, 0, "aaaaa", Style::default());
        buffer.set_string(0, 0, "称号", Style::default());
        assert_eq!(buffer, Buffer::with_lines(["称号a"]));
    }

    #[test]
    fn set_string_zero_width() {
        assert_eq!("\u{200B}".cell_width(), 0);

        let area = Rect::new(0, 0, 1, 1);
        let mut buffer = Buffer::empty(area);

        // Leading grapheme with zero width
        let s = "\u{200B}a";
        buffer.set_stringn(0, 0, s, 1, Style::default());
        assert_eq!(buffer, Buffer::with_lines(["a"]));

        // Trailing grapheme with zero with
        let s = "a\u{200B}";
        buffer.set_stringn(0, 0, s, 1, Style::default());
        assert_eq!(buffer, Buffer::with_lines(["a"]));
    }

    #[test]
    fn set_string_double_width() {
        let area = Rect::new(0, 0, 5, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "コン", Style::default());
        assert_eq!(buffer, Buffer::with_lines(["コン "]));

        // Only 1 space left.
        buffer.set_string(0, 0, "コンピ", Style::default());
        assert_eq!(buffer, Buffer::with_lines(["コン "]));
    }

    #[test]
    fn set_string_halfwidth_katakana_with_dakuten() {
        let area = Rect::new(0, 0, 5, 1);

        // Fullwidth katakana: 2 cells
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "ガ", Style::default());
        let mut expected = Buffer::empty(area);
        expected.set_string(0, 0, "ガ", Style::default());
        assert_eq!(buffer, expected);

        // Halfwidth katakana (no dakuten): 1 cell
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "ｶ", Style::default());
        assert_eq!(buffer.content[0].symbol(), "ｶ");
        assert_eq!(buffer.content[1].symbol(), " ");

        // Halfwidth katakana + non-combining dakuten (U+FF9E): grapheme cluster takes 2 cells
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "ｶﾞ", Style::default());
        // The whole grapheme cluster "ｶﾞ" is placed in cell[0], and cell[1] is its continuation
        // (width=2)
        assert_eq!(buffer.content[0].symbol(), "ｶﾞ");
        assert!(buffer.content[1].is_continuation());
        assert_eq!(buffer.content[2].symbol(), " ");

        // Halfwidth katakana + non-combining handakuten (U+FF9F): grapheme cluster takes 2 cells
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "ﾊﾟ", Style::default());
        assert_eq!(buffer.content[0].symbol(), "ﾊﾟ");
        assert!(buffer.content[1].is_continuation());
        assert_eq!(buffer.content[2].symbol(), " ");

        // Multiple halfwidth katakana with dakuten: each cluster takes 2 cells (4 total)
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "ｶﾞｷﾞ", Style::default());
        assert_eq!(buffer.content[0].symbol(), "ｶﾞ"); // first cluster: width 2
        assert!(buffer.content[1].is_continuation());
        assert_eq!(buffer.content[2].symbol(), "ｷﾞ"); // second cluster: width 2
        assert!(buffer.content[3].is_continuation());
        assert_eq!(buffer.content[4].symbol(), " ");

        // Overflow: only first 2 grapheme clusters fit (4 cells out of 5)
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "ｶﾞｷﾞｸﾞ", Style::default());
        assert_eq!(buffer.content[0].symbol(), "ｶﾞ");
        assert!(buffer.content[1].is_continuation());
        assert_eq!(buffer.content[2].symbol(), "ｷﾞ");
        assert!(buffer.content[3].is_continuation());
        assert_eq!(buffer.content[4].symbol(), " ");
    }

    #[test]
    fn set_string_combining_vs_halfwidth_dakuten() {
        let area = Rect::new(0, 0, 5, 1);

        // Combining dakuten (U+3099): forms 1-cell grapheme cluster with width 1
        let mut buffer1 = Buffer::empty(area);
        let (x1, _) = buffer1.set_stringn(0, 0, "ｶ゙", usize::MAX, Style::default());
        // The combining dakuten merges with ｶ into a single cell (width 1)
        assert_eq!(buffer1.content[0].symbol(), "ｶ゙");
        assert_eq!(buffer1.content[0].cell_width(), 1);
        assert_eq!(x1, 1);

        // Non-combining halfwidth dakuten (U+FF9E): grapheme cluster with width 2
        let mut buffer2 = Buffer::empty(area);
        let (x2, _) = buffer2.set_stringn(0, 0, "ｶﾞ", usize::MAX, Style::default());
        // The grapheme cluster "ｶﾞ" is stored in cell[0], but takes 2 cells width
        assert_eq!(buffer2.content[0].symbol(), "ｶﾞ");
        assert_eq!(buffer2.content[0].cell_width(), 2);
        assert_eq!(x2, 2);
    }

    #[fixture]
    fn small_one_line_buffer() -> Buffer {
        Buffer::empty(Rect::new(0, 0, 5, 1))
    }

    #[rstest]
    #[case::empty("", "     ")]
    #[case::one("1", "1    ")]
    #[case::full("12345", "12345")]
    #[case::overflow("123456", "12345")]
    fn set_line_raw(
        mut small_one_line_buffer: Buffer,
        #[case] content: &str,
        #[case] expected: &str,
    ) {
        let line = Line::raw(content);
        small_one_line_buffer.set_line(0, 0, &line, 5);

        // note: testing with empty / set_string here instead of with_lines because with_lines calls
        // set_line
        let mut expected_buffer = Buffer::empty(small_one_line_buffer.area);
        expected_buffer.set_string(0, 0, expected, Style::default());
        assert_eq!(small_one_line_buffer, expected_buffer);
    }

    #[rstest]
    #[case::empty("", "     ")]
    #[case::one("1", "1    ")]
    #[case::full("12345", "12345")]
    #[case::overflow("123456", "12345")]
    fn set_line_styled(
        mut small_one_line_buffer: Buffer,
        #[case] content: &str,
        #[case] expected: &str,
    ) {
        let color = Color::Blue;
        let line = Line::styled(content, color);
        small_one_line_buffer.set_line(0, 0, &line, 5);

        // note: manually testing the contents here as the Buffer::with_lines calls set_line
        let actual_contents = small_one_line_buffer
            .content
            .iter()
            .map(Cell::symbol)
            .join("");
        let actual_styles = small_one_line_buffer
            .content
            .iter()
            .map(|c| c.fg)
            .collect_vec();

        // set_line only sets the style for non-empty cells (unlike Line::render which sets the
        // style for all cells)
        let expected_styles = iter::repeat_n(color, content.len().min(5))
            .chain(iter::repeat_n(
                Color::default(),
                5_usize.saturating_sub(content.len()),
            ))
            .collect_vec();
        assert_eq!(actual_contents, expected);
        assert_eq!(actual_styles, expected_styles);
    }

    #[test]
    fn set_style() {
        let mut buffer = Buffer::with_lines(["aaaaa", "bbbbb", "ccccc"]);
        buffer.set_style(Rect::new(0, 1, 5, 1), Style::new().red());
        #[rustfmt::skip]
        let expected = Buffer::with_lines([
            "aaaaa".into(),
            "bbbbb".red(),
            "ccccc".into(),
        ]);
        assert_eq!(buffer, expected);
    }

    #[test]
    fn set_style_does_not_panic_when_out_of_area() {
        let mut buffer = Buffer::with_lines(["aaaaa", "bbbbb", "ccccc"]);
        buffer.set_style(Rect::new(0, 1, 10, 3), Style::new().red());
        #[rustfmt::skip]
        let expected = Buffer::with_lines([
            "aaaaa".into(),
            "bbbbb".red(),
            "ccccc".red(),
        ]);
        assert_eq!(buffer, expected);
    }

    #[test]
    fn with_lines() {
        #[rustfmt::skip]
        let buffer = Buffer::with_lines([
            "┌────────┐",
            "│コンピュ│",
            "│ーa 上で│",
            "└────────┘",
        ]);
        assert_eq!(buffer.area.x, 0);
        assert_eq!(buffer.area.y, 0);
        assert_eq!(buffer.area.width, 10);
        assert_eq!(buffer.area.height, 4);
    }

    #[test]
    fn diff_empty_empty() {
        let area = Rect::new(0, 0, 40, 40);
        let prev = Buffer::empty(area);
        let next = Buffer::empty(area);
        let diff = prev.diff(&next);
        assert_eq!(diff, []);
    }

    #[test]
    fn diff_empty_filled() {
        let area = Rect::new(0, 0, 40, 40);
        let prev = Buffer::empty(area);
        let next = Buffer::filled(area, Cell::new("a"));
        let diff = prev.diff_iter(&next);
        assert_eq!(diff.count(), 40 * 40);
    }

    #[test]
    fn diff_filled_filled() {
        let area = Rect::new(0, 0, 40, 40);
        let prev = Buffer::filled(area, Cell::new("a"));
        let next = Buffer::filled(area, Cell::new("a"));
        let diff = prev.diff(&next);
        assert_eq!(diff, []);
    }

    #[test]
    fn diff_single_width() {
        let prev = Buffer::with_lines([
            "          ",
            "┌Title─┐  ",
            "│      │  ",
            "│      │  ",
            "└──────┘  ",
        ]);
        let next = Buffer::with_lines([
            "          ",
            "┌TITLE─┐  ",
            "│      │  ",
            "│      │  ",
            "└──────┘  ",
        ]);
        let diff = prev.diff(&next);
        assert_eq!(
            diff,
            [
                (2, 1, &Cell::new("I")),
                (3, 1, &Cell::new("T")),
                (4, 1, &Cell::new("L")),
                (5, 1, &Cell::new("E")),
            ]
        );
    }

    #[test]
    fn diff_multi_width() {
        #[rustfmt::skip]
        let prev = Buffer::with_lines([
            "┌Title─┐  ",
            "└──────┘  ",
        ]);
        #[rustfmt::skip]
        let next = Buffer::with_lines([
            "┌称号──┐  ",
            "└──────┘  ",
        ]);
        let diff = prev.diff(&next);
        assert_eq!(
            diff,
            [
                (1, 0, &Cell::new("称")),
                // Skipped "i"
                (3, 0, &Cell::new("号")),
                // Skipped "l"
                (5, 0, &Cell::new("─")),
            ]
        );
    }

    #[test]
    fn diff_multi_width_offset() {
        let prev = Buffer::with_lines(["┌称号──┐"]);
        let next = Buffer::with_lines(["┌─称号─┐"]);

        let diff = prev.diff(&next);
        assert_eq!(
            diff,
            [
                (1, 0, &Cell::new("─")),
                (2, 0, &Cell::new("称")),
                (4, 0, &Cell::new("号")),
            ]
        );
    }

    #[test]
    fn merge_diff_idempotent() {
        let mut prev = Buffer::with_lines(["123"]);
        let next = Buffer::with_lines(["456"]);
        prev.merge(&next);

        let diff = prev.diff(&next);
        assert_eq!(diff, []);
    }

    #[test]
    fn merge_diff_forcedwidth() {
        let mut prev = Buffer::with_lines(["123"]);
        let mut next = Buffer::empty(Rect::new(0, 0, 3, 1));
        next.cell_mut((0, 0))
            .unwrap()
            .set_symbol_with_width("456", NonZeroU16::new(3).unwrap());
        prev.merge(&next);

        let diff = prev.diff(&next);
        assert_eq!(diff, []);
    }

    #[test]
    fn merge_diff_link() {
        let mut prev = Buffer::with_lines(["_".repeat(4)]);
        let mut next = Buffer::empty(Rect::new(0, 0, 4, 1));
        // Squeeze everything into the first cell, remaining cells are irrelevant.
        next.cell_mut((0, 0)).unwrap().set_symbol_with_width(
            "\x1b]8;;http://example.com\x1b\\link\x1b]8;;\x1b\\",
            NonZeroU16::new(4).unwrap(),
        );
        prev.merge(&next);

        let diff = prev.diff(&next);
        assert_eq!(diff, []);
    }

    #[test]
    fn merge_diff_split_link() {
        let mut prev = Buffer::with_lines(["_".repeat(8)]);
        let mut next = Buffer::empty(Rect::new(0, 0, 8, 1));
        // Squeeze starting sequence and first character in first cell.
        next.cell_mut((0, 0)).unwrap().set_symbol_with_width(
            "\x1b]8;;http://example.com\x1b\\🔗",
            NonZeroU16::new(2).unwrap(),
        );
        // Set inner characters normally.
        next.cell_mut((2, 0)).unwrap().set_symbol("l");
        next.cell_mut((3, 0)).unwrap().set_symbol("i");
        next.cell_mut((4, 0)).unwrap().set_symbol("n");
        next.cell_mut((5, 0)).unwrap().set_symbol("k");
        // Squeeze closing part into last cell.
        next.cell_mut((7, 0))
            .unwrap()
            .set_symbol_with_width("🔗\x1b]8;;\x1b\\", NonZeroU16::new(2).unwrap());
        prev.merge(&next);

        let diff = prev.diff(&next);
        assert_eq!(diff, []);
    }

    #[test]
    fn merge_diff_image_sequences() {
        let mut prev = Buffer::empty(Rect::new(0, 0, 20, 1));

        let mut kitty_image_placeholder_start = String::new();
        // The first control sequence is to store the cursors state, including foreground color.
        // It is necessary because we need to introduce a foreground color, but restore later to
        // the previous foreground color, to which we don't have access in this buffer.
        // At this point, the symbol width is already greater than one and would cause the next
        // cell to be skipped, if we don't `set_forced_width(Some(1))`.
        kitty_image_placeholder_start.push_str("\x1b[s");
        // The second control sequence sets the foreground color to some 24 bit value, which is an
        // imaginary "kitty protocol image id".
        kitty_image_placeholder_start.push_str("\x1b[38;2;0;0;0m");
        // The following are some special (reserved) unicode placeholder symbols.
        // 10EEEE is the actual placeholder, and the 3 following characters would contain some row,
        // column, and extra "image id" data.
        kitty_image_placeholder_start.push_str("\u{10EEEE}\u{305}\u{305}\u{305}");

        prev.cell_mut((0, 0))
            .unwrap()
            .set_symbol_with_width(&kitty_image_placeholder_start, NonZeroU16::new(1).unwrap());

        // Add two follow up placeholder symbols that have a natural width of 1.
        prev.cell_mut((1, 0)).unwrap().set_char('\u{10EEEE}');
        prev.cell_mut((2, 0)).unwrap().set_char('\u{10EEEE}');
        // The last symbol also has a control sequence which restores the style, so we need to
        // force the width again to 1 to avoid skipping the next cell.
        prev.cell_mut((3, 0))
            .unwrap()
            .set_symbol_with_width("\u{10EEEE}\x1b[u", NonZeroU16::new(1).unwrap());

        let mut buffer = Buffer::filled(Rect::new(0, 0, 20, 1), Cell::new("x"));
        buffer.merge(&prev);

        let diff = buffer.diff(&prev);
        assert_eq!(diff, []);
    }

    #[test]
    fn diff_skip() {
        let prev = Buffer::with_lines(["123"]);
        let mut next = Buffer::with_lines(["456"]);
        for cell in &mut next.content[1..3] {
            cell.set_diff_option(CellDiffOption::Skip);
        }

        let diff = prev.diff(&next);
        assert_eq!(diff, [(0, 0, &Cell::new("4"))],);
    }

    #[rstest]
    #[case(Rect::new(0, 0, 2, 2), Rect::new(0, 2, 2, 2), ["11", "11", "22", "22"])]
    #[case(Rect::new(2, 2, 2, 2), Rect::new(0, 0, 2, 2), ["22  ", "22  ", "  11", "  11"])]
    fn merge<'line, Lines>(#[case] one: Rect, #[case] two: Rect, #[case] expected: Lines)
    where
        Lines: IntoIterator,
        Lines::Item: Into<Line<'line>>,
    {
        let mut one = Buffer::filled(one, Cell::new("1"));
        let two = Buffer::filled(two, Cell::new("2"));
        one.merge(&two);
        assert_eq!(one, Buffer::with_lines(expected));
    }

    #[test]
    fn merge_with_offset() {
        let mut one = Buffer::filled(
            Rect {
                x: 3,
                y: 3,
                width: 2,
                height: 2,
            },
            Cell::new("1"),
        );
        let two = Buffer::filled(
            Rect {
                x: 1,
                y: 1,
                width: 3,
                height: 4,
            },
            Cell::new("2"),
        );
        one.merge(&two);
        let mut expected = Buffer::with_lines(["222 ", "222 ", "2221", "2221"]);
        expected.area = Rect {
            x: 1,
            y: 1,
            width: 4,
            height: 4,
        };
        assert_eq!(one, expected);
    }

    #[rstest]
    #[case(CellDiffOption::None, CellDiffOption::Skip, [CellDiffOption::None, CellDiffOption::None, CellDiffOption::Skip, CellDiffOption::Skip, CellDiffOption::Skip, CellDiffOption::Skip])]
    #[case(CellDiffOption::Skip, CellDiffOption::None, [CellDiffOption::Skip, CellDiffOption::Skip, CellDiffOption::None, CellDiffOption::None, CellDiffOption::None, CellDiffOption::None])]
    fn merge_skip(
        #[case] skip_one: CellDiffOption,
        #[case] skip_two: CellDiffOption,
        #[case] expected: [CellDiffOption; 6],
    ) {
        let mut one = {
            let area = Rect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            };
            let mut cell = Cell::new("1");
            cell.diff_option = skip_one;
            Buffer::filled(area, cell)
        };
        let two = {
            let area = Rect {
                x: 0,
                y: 1,
                width: 2,
                height: 2,
            };
            let mut cell = Cell::new("2");
            cell.diff_option = skip_two;
            Buffer::filled(area, cell)
        };
        one.merge(&two);
        let skipped = one
            .content()
            .iter()
            .map(|c| c.diff_option)
            .collect::<Vec<_>>();
        assert_eq!(skipped, expected);
    }

    #[test]
    fn with_lines_accepts_into_lines() {
        use crate::style::Stylize;
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 2));
        buf.set_string(0, 0, "foo", Style::new().red());
        buf.set_string(0, 1, "bar", Style::new().blue());
        assert_eq!(buf, Buffer::with_lines(["foo".red(), "bar".blue()]));
    }

    #[test]
    fn control_sequence_rendered_full() {
        let text = "I \x1b[0;36mwas\x1b[0m here!";

        let mut buffer = Buffer::filled(Rect::new(0, 0, 25, 3), Cell::new("x"));
        buffer.set_string(1, 1, text, Style::new());

        let expected = Buffer::with_lines([
            "xxxxxxxxxxxxxxxxxxxxxxxxx",
            "xI [0;36mwas[0m here!xxxx",
            "xxxxxxxxxxxxxxxxxxxxxxxxx",
        ]);
        assert_eq!(buffer, expected);
    }

    #[test]
    fn control_sequence_rendered_partially() {
        let text = "I \x1b[0;36mwas\x1b[0m here!";

        let mut buffer = Buffer::filled(Rect::new(0, 0, 11, 3), Cell::new("x"));
        buffer.set_string(1, 1, text, Style::new());

        #[rustfmt::skip]
        let expected = Buffer::with_lines([
            "xxxxxxxxxxx",
            "xI [0;36mwa",
            "xxxxxxxxxxx",
        ]);
        assert_eq!(buffer, expected);
    }

    /// Emojis normally contain various characters which should stay part of the Emoji.
    /// This should work fine by utilizing `unicode_segmentation` but a testcase is probably helpful
    /// due to the nature of never perfect Unicode implementations and all of its quirks.
    #[rstest]
    // Shrug without gender or skintone. Has a width of 2 like all emojis have.
    #[case::shrug("🤷", "🤷xxxxx")]
    // Technically this is a (brown) bear, a zero-width joiner and a snowflake
    // As it is joined its a single emoji and should therefore have a width of 2.
    // Prior to unicode-width 0.2, this was incorrectly detected as width 4 for some reason
    #[case::polarbear("🐻‍❄️", "🐻‍❄️xxxxx")]
    // Technically this is an eye, a zero-width joiner and a speech bubble
    // Both eye and speech bubble include a 'display as emoji' variation selector
    // Prior to unicode-width 0.2, this was incorrectly detected as width 4 for some reason
    #[case::eye_speechbubble("👁️‍🗨️", "👁️‍🗨️xxxxx")]
    // Keyboard keycap emoji: base symbol + VS16 for emoji presentation
    // This should render as a single grapheme with width 2.
    #[case::keyboard_emoji("⌨️", "⌨️xxxxx")]
    fn renders_emoji(#[case] input: &str, #[case] expected: &str) {
        use unicode_width::UnicodeWidthChar;

        dbg!(input);
        dbg!(input.len());
        dbg!(
            input
                .graphemes(true)
                .map(|symbol| (
                    symbol,
                    symbol.escape_unicode().to_string(),
                    symbol.cell_width()
                ))
                .collect::<Vec<_>>()
        );
        dbg!(
            input
                .chars()
                .map(|char| (
                    char,
                    char.escape_unicode().to_string(),
                    char.width(),
                    char.is_control()
                ))
                .collect::<Vec<_>>()
        );

        let mut buffer = Buffer::filled(Rect::new(0, 0, 7, 1), Cell::new("x"));
        buffer.set_string(0, 0, input, Style::new());

        let expected = Buffer::with_lines([expected]);
        assert_eq!(buffer, expected);
    }

    /// Regression test for <https://github.com/ratatui/ratatui/issues/1441>
    ///
    /// Previously the `pos_of` function would incorrectly cast the index to a u16 value instead of
    /// using the index as is. This caused incorrect rendering of any buffer with an length > 65535.
    #[test]
    fn index_pos_of_u16_max() {
        let buffer = Buffer::empty(Rect::new(0, 0, 256, 256 + 1));
        assert_eq!(buffer.index_of(255, 255), 65535);
        assert_eq!(buffer.pos_of(65535), (255, 255));

        assert_eq!(buffer.index_of(0, 256), 65536);
        assert_eq!(buffer.pos_of(65536), (0, 256)); // previously (0, 0)

        assert_eq!(buffer.index_of(1, 256), 65537);
        assert_eq!(buffer.pos_of(65537), (1, 256)); // previously (1, 0)

        assert_eq!(buffer.index_of(255, 256), 65791);
        assert_eq!(buffer.pos_of(65791), (255, 256)); // previously (255, 0)
    }

    #[test]
    fn diff_clears_trailing_cell_for_wide_grapheme() {
        // Reproduce: write "ab", then overwrite with a wide emoji like "⌨️"
        let prev = Buffer::with_lines(["ab"]); // width 2 area inferred
        assert_eq!(prev.area.width, 2);

        let mut next = Buffer::with_lines(["  "]); // start with blanks
        next.set_string(0, 0, "⌨️", Style::new());

        // The next buffer contains a wide grapheme occupying cell 0 and implicitly cell 1.
        // The debug formatting shows the hidden trailing space.
        let expected_next = Buffer::with_lines(["⌨️"]);
        assert_eq!(next, expected_next);

        // The diff should include an update for (0,0) to draw the emoji. Depending on
        // terminal behavior, it may or may not be necessary to explicitly clear (1,0).
        // At minimum, ensure the first cell is updated and nothing incorrect is emitted.
        let diff = prev.diff(&next);
        assert!(
            diff.iter()
                .any(|(x, y, c)| *x == 0 && *y == 0 && c.symbol() == "⌨️")
        );
        // And it should explicitly clear the trailing cell (1,0) to avoid leftovers on terminals
        // that don't automatically clear the following cell for wide characters.
        assert!(
            diff.iter()
                .any(|(x, y, c)| *x == 1 && *y == 0 && c.symbol() == " ")
        );
    }

    /// Regression test for a bug where trailing cells of wide VS16 emojis would generate
    /// unnecessary diff updates when only the style (not the symbol) changed.
    ///
    /// This caused visual artifacts when:
    /// 1. A previous widget applied a foreground color to the entire area
    /// 2. A border with VS16 emojis (like ⚠️) was rendered on top
    /// 3. The trailing cells had different styles but the same symbol (space)
    ///
    /// Only a column that shows something on a blank (a symbol, a background, ...) is cleared
    /// before the emoji is drawn; a foreground color on a blank is not visible.
    #[test]
    fn diff_ignores_style_only_changes_in_trailing_cells() {
        use crate::style::Color;

        // Previous buffer: spaces with specific fg color
        let mut prev = Buffer::empty(Rect::new(0, 0, 3, 1));
        prev.set_string(0, 0, "  ", Style::new().fg(Color::LightBlue)); // <-- style A
        prev.set_string(2, 0, "x", Style::new());

        // Next buffer: emoji with a DIFFERENT style; its second column is a continuation
        let mut next = Buffer::empty(Rect::new(0, 0, 3, 1));
        next.set_string(0, 0, "⚠️", Style::new()); // <-- style B (DIFFERENT from prev!)
        next.set_string(2, 0, "x", Style::new());

        let diff = prev.diff(&next);

        // The first cell (0,0) SHOULD be in the diff because the symbol changed
        assert!(
            diff.iter().any(|(x, y, _)| *x == 0 && *y == 0),
            "Diff should include first cell (0,0) because the symbol changed"
        );

        // The diff should NOT contain an update for the trailing cell (1, 0)
        // because the symbol is the same (space) - only the style changed,
        // and the style of a hidden trailing cell is not visible.
        assert!(
            !diff.iter().any(|(x, y, _)| *x == 1 && *y == 0),
            "Diff should not include trailing cell (1,0) when only style changed. \
             Found updates: {:?}",
            diff.iter()
                .map(|(x, y, c)| (x, y, c.symbol(), c.fg))
                .collect::<Vec<_>>()
        );
    }
}
