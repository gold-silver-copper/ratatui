use core::num::NonZeroU16;

use compact_str::CompactString;
use unicode_segmentation::UnicodeSegmentation;

use crate::buffer::cell_width::CellWidth;
use crate::style::{Color, Modifier, Style};
use crate::symbols::merge::MergeStrategy;

/// Cell diffing options
#[derive(Debug, Default, Clone, Eq, PartialEq, Hash, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CellDiffOption {
    #[default]
    /// No special option.
    None,
    /// Skip this cell when diffing.
    ///
    /// This is helpful when it is necessary to prevent the buffer from overwriting a cell that is
    /// covered by something from an escape sequence, such as graphics or links.
    Skip,
    /// Always update this cell when diffing.
    ///
    /// This bypasses the equality check against the previous buffer. Use it when another
    /// renderer may draw over the same area, such as an external image pipeline, so Ratatui can
    /// redraw text there on the next render.
    AlwaysUpdate,
}

/// How many columns a cell occupies.
///
/// Private so that only [`Buffer`](super::Buffer) can make a cell a continuation, and it does so
/// only while writing the wide glyph to its left.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum Span {
    /// As many columns as the symbol is wide.
    #[default]
    Symbol,
    /// A declared number of columns, for symbols whose text width says nothing about their size
    /// on screen (escape sequences), or for a wide glyph clipped by the right edge of the buffer.
    Fixed(NonZeroU16),
    /// Covered by the wide glyph anchored to the left of this cell.
    Continuation,
}

/// A buffer cell
#[derive(Debug, Default, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cell {
    /// The string to be drawn in the cell.
    ///
    /// This accepts unicode grapheme clusters which might take up more than one cell.
    ///
    /// This is a [`CompactString`] which is a wrapper around [`String`] that uses a small inline
    /// buffer for short strings.
    ///
    /// See <https://github.com/ratatui/ratatui/pull/601> for more information.
    symbol: Option<CompactString>,

    /// The foreground color of the cell.
    pub fg: Color,

    /// The background color of the cell.
    pub bg: Color,

    /// The underline color of the cell.
    #[cfg(feature = "underline-color")]
    pub underline_color: Color,

    /// The modifier of the cell.
    pub modifier: Modifier,

    /// Special option applied when copying (diffing) the buffer to the screen (or another buffer).
    pub diff_option: CellDiffOption,

    /// Whether the cell should be skipped when copying (diffing) the buffer to the screen.
    ///
    /// Use [`CellDiffOption::Skip`] via [`set_diff_option`](Self::set_diff_option) instead.
    #[deprecated(
        since = "0.30.1",
        note = "use `set_diff_option(CellDiffOption::Skip)` instead"
    )]
    pub skip: bool,

    /// How many columns the cell occupies, or whether it is covered by the glyph to its left.
    span: Span,
}

impl Cell {
    /// An empty `Cell`
    #[allow(deprecated)]
    pub const EMPTY: Self = Self {
        symbol: None,
        fg: Color::Reset,
        bg: Color::Reset,
        #[cfg(feature = "underline-color")]
        underline_color: Color::Reset,
        modifier: Modifier::empty(),
        diff_option: CellDiffOption::None,
        skip: false,
        span: Span::Symbol,
    };

    /// Creates a new `Cell` with the given symbol.
    ///
    /// This works at compile time and puts the symbol onto the stack. Fails to build when the
    /// symbol doesn't fit onto the stack and requires to be placed on the heap. Use
    /// `Self::default().set_symbol()` in that case. See [`CompactString::const_new`] for more
    /// details on this.
    pub const fn new(symbol: &'static str) -> Self {
        Self {
            symbol: Some(CompactString::const_new(symbol)),
            ..Self::EMPTY
        }
    }

    /// Gets the symbol of the cell.
    ///
    /// If the cell has no symbol, returns a single space character. A
    /// [continuation](Self::is_continuation) cell draws nothing of its own and returns an empty
    /// string.
    #[must_use]
    pub fn symbol(&self) -> &str {
        match (&self.symbol, self.span) {
            (_, Span::Continuation) => "",
            (Some(symbol), _) => symbol.as_str(),
            (None, _) => " ",
        }
    }

    /// Returns `true` if this cell is covered by the wide glyph to its left.
    ///
    /// A wide glyph, such as `界` or an emoji, is stored in the cell where it starts (its
    /// anchor); the other columns it covers hold continuation cells. They have no symbol of their
    /// own and are never sent to the terminal: the anchor draws them. Only a [`Buffer`] write of
    /// a wide glyph creates them, and any write to a part of the glyph removes the whole glyph,
    /// so a continuation always follows the anchor (or another continuation) of its glyph.
    ///
    /// They share the style of their glyph, which draws it on all its columns. When the glyph is
    /// overwritten, they become blank cells in that style.
    ///
    /// [`Buffer`]: super::Buffer
    #[must_use]
    pub const fn is_continuation(&self) -> bool {
        matches!(self.span, Span::Continuation)
    }

    /// Returns how many columns the cell's glyph occupies in the buffer, at least one.
    ///
    /// This is [`cell_width`](CellWidth::cell_width) for a cell that draws a glyph, and one for a
    /// continuation cell or a zero-width symbol, which still fill their column.
    pub(crate) fn columns(&self) -> u16 {
        self.cell_width().max(1)
    }

    /// Turns this cell into a continuation of a glyph drawn with `style`.
    pub(crate) fn make_continuation(&mut self, style: Style) {
        self.reset();
        self.set_style(style);
        self.span = Span::Continuation;
    }

    /// Turns this cell, part of a glyph that was overwritten, into a blank keeping its style.
    pub(crate) fn make_blank(&mut self) {
        self.symbol = None;
        self.span = Span::Symbol;
    }

    /// Declares how many columns the cell occupies.
    pub(crate) const fn set_span(&mut self, span: Span) {
        self.span = span;
    }

    /// Merges the symbol of the cell with the one already on the cell, using the provided
    /// [`MergeStrategy`].
    ///
    /// Merges [Box Drawing Unicode block] characters to create a single character representing
    /// their combination, useful for [border collapsing]. Currently limited to box drawing
    /// characters, with potential future support for others.
    ///
    /// Merging may not be perfect due to Unicode limitations; some symbol combinations might not
    /// produce a valid character. [`MergeStrategy`] defines how to handle such cases, e.g.,
    /// `Exact` for valid merges only, or `Fuzzy` for close matches.
    ///
    /// If the cell has no symbol set, it will set the symbol to the provided one rather than
    /// merging.
    ///
    /// # Example
    ///
    /// ```
    /// # use ratatui_core::buffer::Cell;
    /// use ratatui_core::symbols::merge::MergeStrategy;
    ///
    /// assert_eq!(
    ///     Cell::new("┘")
    ///         .merge_symbol("┏", MergeStrategy::Exact)
    ///         .symbol(),
    ///     "╆",
    /// );
    ///
    /// assert_eq!(
    ///     Cell::new("╭")
    ///         .merge_symbol("┘", MergeStrategy::Fuzzy)
    ///         .symbol(),
    ///     "┼",
    /// );
    /// ```
    ///
    /// [border collapsing]: https://ratatui.rs/recipes/layout/collapse-borders/
    /// [Box Drawing Unicode block]: https://en.wikipedia.org/wiki/Box_Drawing
    pub fn merge_symbol(&mut self, symbol: &str, strategy: MergeStrategy) -> &mut Self {
        let merged_symbol = self
            .symbol
            .as_ref()
            .map_or(symbol, |s| strategy.merge(s, symbol));
        self.symbol = Some(CompactString::new(merged_symbol));
        self.span = Span::Symbol;
        self
    }

    /// Sets the symbol of the cell.
    ///
    /// The cell then occupies as many columns as the symbol is wide. This changes a cell value;
    /// to write to a [`Buffer`](super::Buffer), use [`Buffer::cell_mut`](super::Buffer::cell_mut),
    /// which also updates the columns the symbol covers.
    pub fn set_symbol(&mut self, symbol: &str) -> &mut Self {
        self.symbol = Some(CompactString::new(symbol));
        self.span = Span::Symbol;
        self
    }

    /// Appends a symbol to the cell.
    ///
    /// This is particularly useful for adding zero-width characters to the cell.
    pub(crate) fn append_symbol(&mut self, symbol: &str) -> &mut Self {
        self.symbol.get_or_insert_default().push_str(symbol);
        self.span = Span::Symbol;
        self
    }

    /// Sets the symbol of the cell to a single character.
    pub fn set_char(&mut self, ch: char) -> &mut Self {
        let mut buf = [0; 4];
        self.symbol = Some(CompactString::new(ch.encode_utf8(&mut buf)));
        self.span = Span::Symbol;
        self
    }

    /// Sets the foreground color of the cell.
    pub const fn set_fg(&mut self, color: Color) -> &mut Self {
        self.fg = color;
        self
    }

    /// Sets the background color of the cell.
    pub const fn set_bg(&mut self, color: Color) -> &mut Self {
        self.bg = color;
        self
    }

    /// Sets the style of the cell.
    ///
    ///  `style` accepts any type that is convertible to [`Style`] (e.g. [`Style`], [`Color`], or
    /// your own type that implements [`Into<Style>`]).
    pub fn set_style<S: Into<Style>>(&mut self, style: S) -> &mut Self {
        let style = style.into();
        if let Some(c) = style.fg {
            self.fg = c;
        }
        if let Some(c) = style.bg {
            self.bg = c;
        }
        #[cfg(feature = "underline-color")]
        if let Some(c) = style.underline_color {
            self.underline_color = c;
        }
        self.modifier.insert(style.add_modifier);
        self.modifier.remove(style.sub_modifier);
        self
    }

    /// Returns the style of the cell.
    #[must_use]
    pub const fn style(&self) -> Style {
        Style {
            fg: Some(self.fg),
            bg: Some(self.bg),
            #[cfg(feature = "underline-color")]
            underline_color: Some(self.underline_color),
            add_modifier: self.modifier,
            sub_modifier: Modifier::empty(),
        }
    }

    /// Sets the cell to be skipped when copying (diffing) the buffer to the screen.
    ///
    /// This is helpful when it is necessary to prevent the buffer from overwriting a cell that is
    /// covered by an image from some terminal graphics protocol (Sixel / iTerm / Kitty ...).
    #[deprecated(
        since = "0.30.1",
        note = "use `set_diff_option(CellDiffOption::Skip)` instead"
    )]
    #[allow(deprecated)]
    pub const fn set_skip(&mut self, skip: bool) -> &mut Self {
        self.skip = skip;
        self
    }

    /// Sets cell [`CellDiffOption`].
    ///
    /// The diff options are for cells that should always be updated, or that should not be
    /// updated at all (for example because an image drawn by an escape sequence covers them).
    pub const fn set_diff_option(&mut self, diff_option: CellDiffOption) -> &mut Self {
        self.diff_option = diff_option;
        self
    }

    /// Resets the cell to the empty state.
    #[allow(deprecated)]
    pub fn reset(&mut self) {
        *self = Self::EMPTY;
    }
}

impl PartialEq for Cell {
    /// Compares two `Cell`s for equality.
    ///
    /// Note that cells with no symbol (i.e., `Cell::EMPTY`) are considered equal to cells with a
    /// single space symbol. This is to ensure that empty cells are treated uniformly,
    /// regardless of how they were created
    fn eq(&self, other: &Self) -> bool {
        // Treat None and Some(" ") as equal
        let symbols_eq = self.symbol() == other.symbol();

        #[cfg(feature = "underline-color")]
        let underline_color_eq = self.underline_color == other.underline_color;
        #[cfg(not(feature = "underline-color"))]
        let underline_color_eq = true;

        #[allow(deprecated)]
        let skip_eq = self.skip == other.skip;

        symbols_eq
            && underline_color_eq
            && skip_eq
            && self.fg == other.fg
            && self.bg == other.bg
            && self.modifier == other.modifier
            && self.diff_option == other.diff_option
            && self.span == other.span
    }
}

impl Eq for Cell {}

impl core::hash::Hash for Cell {
    /// Hashes the cell.
    ///
    /// This treats symbols with Some(" ") as equal to None, so that empty cells are
    /// treated uniformly, regardless of how they were created.
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.symbol().hash(state);
        self.fg.hash(state);
        self.bg.hash(state);
        #[cfg(feature = "underline-color")]
        self.underline_color.hash(state);
        self.modifier.hash(state);
        self.diff_option.hash(state);
        #[allow(deprecated)]
        self.skip.hash(state);
        self.span.hash(state);
    }
}

impl From<char> for Cell {
    fn from(ch: char) -> Self {
        let mut cell = Self::EMPTY;
        cell.set_char(ch);
        cell
    }
}

impl CellWidth for Cell {
    /// Returns the number of columns the cell's glyph covers.
    ///
    /// This is the declared width for a cell written with
    /// [`CellMut::set_symbol_with_width`](super::CellMut::set_symbol_with_width) (or clipped by the
    /// edge of the buffer), zero for a [continuation](Self::is_continuation) cell, and otherwise
    /// the width of the cell's symbol.
    ///
    /// A cell holds a single grapheme cluster, so the unicode width of its symbol is the number
    /// of columns it takes on screen. Low-level callers can store anything in a symbol though,
    /// and image protocols store a whole escape sequence there while still occupying a single
    /// column. Such a symbol has no meaningful unicode width, so it counts as one column.
    fn cell_width(&self) -> u16 {
        match self.span {
            Span::Fixed(width) => width.get(),
            Span::Continuation => 0,
            Span::Symbol => {
                let symbol = self.symbol();
                let width = symbol.cell_width();
                if width > 1 && !is_single_grapheme(symbol) {
                    1
                } else {
                    width
                }
            }
        }
    }
}

/// Returns `true` if `symbol` is a single grapheme cluster.
///
/// Stops after the second cluster, so this stays cheap for the long escape payloads that make
/// the check necessary in the first place.
fn is_single_grapheme(symbol: &str) -> bool {
    let mut graphemes = symbol.graphemes(true);
    graphemes.next().is_some() && graphemes.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_width_of_wide_grapheme() {
        assert_eq!(Cell::new("あ").cell_width(), 2);
        assert_eq!(Cell::new("a").cell_width(), 1);
    }

    /// Image protocols keep a whole escape sequence in one symbol. Its unicode width says
    /// hundreds of columns, but the cell covers one, so it must not be reported as wide.
    #[test]
    fn cell_width_of_escape_sequence_symbol() {
        let payload = "\u{1b}[s".repeat(400);
        let mut cell = Cell::EMPTY;
        cell.set_symbol(&payload);
        assert_eq!(cell.cell_width(), 1);
    }

    #[test]
    fn cell_width_of_fixed_span() {
        let payload = "\u{1b}[s".repeat(400);
        let mut cell = Cell::EMPTY;
        cell.set_symbol(&payload);
        cell.set_span(Span::Fixed(NonZeroU16::new(24).unwrap()));
        assert_eq!(cell.cell_width(), 24);
        // A new symbol is measured again.
        cell.set_symbol("a");
        assert_eq!(cell.cell_width(), 1);
    }

    #[test]
    #[allow(deprecated)]
    fn new() {
        let cell = Cell::new("あ");
        assert_eq!(
            cell,
            Cell {
                symbol: Some(CompactString::const_new("あ")),
                fg: Color::Reset,
                bg: Color::Reset,
                #[cfg(feature = "underline-color")]
                underline_color: Color::Reset,
                modifier: Modifier::empty(),
                diff_option: CellDiffOption::None,
                skip: false,
                span: Span::Symbol,
            }
        );
    }

    #[test]
    fn empty() {
        let cell = Cell::EMPTY;
        assert_eq!(cell.symbol(), " ");
    }

    #[test]
    fn set_symbol() {
        let mut cell = Cell::EMPTY;
        cell.set_symbol("あ"); // Multi-byte character
        assert_eq!(cell.symbol(), "あ");
        cell.set_symbol("👨‍👩‍👧‍👦"); // Multiple code units combined with ZWJ
        assert_eq!(cell.symbol(), "👨‍👩‍👧‍👦");
    }

    #[test]
    fn append_symbol() {
        let mut cell = Cell::EMPTY;
        cell.set_symbol("あ"); // Multi-byte character
        cell.append_symbol("\u{200B}"); // zero-width space
        assert_eq!(cell.symbol(), "あ\u{200B}");
    }

    #[test]
    fn set_char() {
        let mut cell = Cell::EMPTY;
        cell.set_char('あ'); // Multi-byte character
        assert_eq!(cell.symbol(), "あ");
    }

    #[test]
    fn set_fg() {
        let mut cell = Cell::EMPTY;
        cell.set_fg(Color::Red);
        assert_eq!(cell.fg, Color::Red);
    }

    #[test]
    fn set_bg() {
        let mut cell = Cell::EMPTY;
        cell.set_bg(Color::Red);
        assert_eq!(cell.bg, Color::Red);
    }

    #[test]
    fn set_style() {
        let mut cell = Cell::EMPTY;
        cell.set_style(Style::new().fg(Color::Red).bg(Color::Blue));
        assert_eq!(cell.fg, Color::Red);
        assert_eq!(cell.bg, Color::Blue);
    }

    #[test]
    fn set_skip() {
        let mut cell = Cell::EMPTY;
        cell.set_diff_option(CellDiffOption::Skip);
        assert_eq!(cell.diff_option, CellDiffOption::Skip);
    }

    #[test]
    fn set_always_update() {
        let mut cell = Cell::EMPTY;
        cell.set_diff_option(CellDiffOption::AlwaysUpdate);
        assert_eq!(cell.diff_option, CellDiffOption::AlwaysUpdate);
    }

    #[test]
    fn reset() {
        let mut cell = Cell::EMPTY;
        cell.set_symbol("あ");
        cell.set_fg(Color::Red);
        cell.set_bg(Color::Blue);
        cell.set_diff_option(CellDiffOption::Skip);
        cell.reset();
        assert_eq!(cell.symbol(), " ");
        assert_eq!(cell.fg, Color::Reset);
        assert_eq!(cell.bg, Color::Reset);
        assert_eq!(cell.diff_option, CellDiffOption::None);
    }

    #[test]
    fn style() {
        let cell = Cell::EMPTY;
        assert_eq!(
            cell.style(),
            Style {
                fg: Some(Color::Reset),
                bg: Some(Color::Reset),
                #[cfg(feature = "underline-color")]
                underline_color: Some(Color::Reset),
                add_modifier: Modifier::empty(),
                sub_modifier: Modifier::empty(),
            }
        );
    }

    #[test]
    fn default() {
        let cell = Cell::default();
        assert_eq!(cell.symbol(), " ");
    }

    #[test]
    fn cell_eq() {
        let cell1 = Cell::new("あ");
        let cell2 = Cell::new("あ");
        assert_eq!(cell1, cell2);
    }

    #[test]
    fn cell_ne() {
        let cell1 = Cell::new("あ");
        let cell2 = Cell::new("い");
        assert_ne!(cell1, cell2);
    }
}
