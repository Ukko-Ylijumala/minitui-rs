// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Scrolling a view taller than its area: [`Scroll`], the offset kept across
frames, and two widgets that render through it: [`TextView`] for text, and
[`CanvasView`] for anything drawn on an off-screen buffer. The apps each
kept a bare offset, each with part of it right:
- squawk drew its host view on a buffer as tall as the panels need and
  copied the rows in view, so a layout of charts and tables scrolls by line
  however small the terminal, with "lines a–b of N" when it overflows;
- squawk, zfs-browser and nft-browser kept the last page full, and squawk
  and zfs-browser paged by the rows of the last draw where nft-browser
  guessed them from the terminal height;
- mping-rs paged its log popup by a page less a line, so the line at the
  edge stays in sight;
- wiremeup's preview offset grew past the end with each ↓ (it was clamped
  only for drawing), so ↑ did nothing until it had come back.

A [`TextView`] can also scroll sideways, for text whose lines are longer
than any terminal (a minified file), and number its lines in a gutter
that stays put; wp-scanner's file viewer was the first user.
*/

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph, StatefulWidget, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::list::page_step;
use crate::text::skip_cols;
use crate::{NavOutcome, range_label};

/* ===== consts and statics ================================================= */

/// What the position indicator counts, by default.
static DEFAULT_WHAT: &str = "lines";
/// What the sideways indicator counts.
static COLS_WHAT: &str = "cols";
/// Between the indicator's lines and columns.
static INDICATOR_SEP: &str = " · ";
/// Columns around the indicator's text: a space either side, the corners.
const INDICATOR_CHROME: usize = 4;
/// Columns between the line numbers and the text.
const GUTTER_GAP: usize = 1;
/// A line brought into view ([`Scroll::reveal`]) lands this far down the page: a third.
const REVEAL_DIV: usize = 3;

/* ===== types ============================================================== */

/**
The scroll offset of a view, kept across frames, plus the rows and the
total lines of the last draw: the page for PgUp / PgDn and the furthest it
can go. Per frame, [`Scroll::window`] clamps the offset so the last page is
full and returns the lines to show; the widgets here call it themselves.

```no_run
# use ratatui::Frame;
# use ratatui::layout::Rect;
# use ratatui::widgets::{Block, Row, Table};
# use crossterm::event::{KeyEvent, KeyModifiers};
use minitui::Scroll;

# fn demo(frame: &mut Frame, area: Rect, key: KeyEvent, rows: &[Row<'static>], scroll: &mut Scroll) {
// Shift-↑ ↓ PgUp PgDn Home End scroll the detail pane; the plain keys are the list's
scroll.key_with(key, KeyModifiers::SHIFT);

// a frame: only the rows in view are built
let block = Block::bordered();
let shown = scroll.window(rows.len(), block.inner(area).height.into());
let title = scroll.label("rows").unwrap_or_default();
let table = Table::default().rows(rows[shown].to_vec()).block(block.title_bottom(title));
frame.render_widget(table, area);
# }
```
*/
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scroll {
    /// The first line shown.
    offset: usize,
    /// Rows shown on the last [`Scroll::window`]; 0 before the first.
    rows: usize,
    /// Lines in all on the last [`Scroll::window`].
    len: usize,
    /// The first column shown, scrolled sideways.
    col: usize,
    /// Columns shown on the last [`Scroll::window_cols`]; 0 before the first.
    cols: usize,
    /// The widest line's columns on the last [`Scroll::window_cols`].
    width: usize,
}

/**
Text in a scrolling view, optionally word-wrapped, rendered through a
[`Scroll`] (as a `StatefulWidget`). With a block, the bottom border says
which lines are in view when it doesn't all fit: ` lines 21–40 of 300 `.

```no_run
# use ratatui::Frame;
# use ratatui::widgets::Block;
use minitui::{Scroll, TextView};

# fn draw(frame: &mut Frame, conf: &str, scroll: &mut Scroll) {
let view = TextView::new(conf).block(Block::bordered().title(" wg0.conf "));
frame.render_stateful_widget(view, frame.area(), scroll);
# }
```
*/
#[derive(Debug, Clone)]
#[must_use]
pub struct TextView<'a> {
    text: Text<'a>,
    wrap: bool,
    sideways: bool,
    numbers: bool,
    number_style: Style,
    chrome: Chrome<'a>,
}

/**
A view onto an off-screen buffer drawn as tall as its content needs,
rendered through a [`Scroll`] (as a `StatefulWidget`): the rows in view are
copied into the frame, so a whole layout of panels scrolls by line. Draw
on a buffer as wide as the block's inner area. With a block, the bottom
border says which lines are in view when it doesn't all fit.

```no_run
# use ratatui::Frame;
# use ratatui::buffer::Buffer;
# use ratatui::layout::Rect;
# use ratatui::widgets::{Block, Paragraph, Widget};
use minitui::{CanvasView, Scroll};

# fn draw(frame: &mut Frame, area: Rect, scroll: &mut Scroll) {
let block = Block::bordered().title(" host ");
let inner = block.inner(area);
let mut canvas = Buffer::empty(Rect::new(0, 0, inner.width, 60));
Paragraph::new("the panels, laid out for 60 rows").render(canvas.area, &mut canvas);
frame.render_stateful_widget(CanvasView::new(&canvas).block(block), area, scroll);
# }
```
*/
#[derive(Debug, Clone)]
#[must_use]
pub struct CanvasView<'a> {
    canvas: &'a Buffer,
    chrome: Chrome<'a>,
}

/// The block around a view and its position indicator, shared by the views.
#[derive(Debug, Clone)]
pub(crate) struct Chrome<'a> {
    pub(crate) block: Option<Block<'a>>,
    /// What the indicator counts; `None` for no indicator.
    pub(crate) what: Option<&'a str>,
    pub(crate) style: Style,
}

/* ===== Scroll ============================================================= */

impl Scroll {
    /// At the top.
    pub fn new() -> Self {
        Self::default()
    }

    /// The first line shown (as of the last [`Scroll::window`] or move).
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Start at line `offset`; [`Scroll::window`] keeps the last page full.
    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }

    /// Rows shown on the last [`Scroll::window`]: the page; `None` before the first.
    pub fn page(&self) -> Option<usize> {
        (self.rows > 0).then_some(self.rows)
    }

    /// Lines in all, as of the last [`Scroll::window`].
    pub fn len(&self) -> usize {
        self.len
    }

    /// No lines, as of the last [`Scroll::window`].
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the last [`Scroll::window`] had more lines than rows.
    pub fn overflows(&self) -> bool {
        self.len > self.rows
    }

    /// Whether the last line is in view (as of the last [`Scroll::window`] or move).
    pub fn at_bottom(&self) -> bool {
        self.offset >= self.max_offset()
    }

    /// Back to the top (and the left edge): for other content in the same place (a new selection).
    pub fn reset(&mut self) {
        self.offset = 0;
        self.col = 0;
    }

    /// The first column shown, scrolled sideways (as of the last [`Scroll::window_cols`] or move).
    pub fn col(&self) -> usize {
        self.col
    }

    /// Start at column `col`; [`Scroll::window_cols`] keeps the widest line's end in view.
    pub fn set_col(&mut self, col: usize) {
        self.col = col;
    }

    /// Whether the last [`Scroll::window_cols`] had a line wider than the columns.
    pub fn overflows_sideways(&self) -> bool {
        self.width > self.cols
    }

    /**
    The lines of `len` to show in `rows`, for this frame: the offset
    clamped so the last page is full. Remembers `rows` as the page and
    `len` as the end for the keys. Call it before building the lines, every
    frame (the widgets here do).
    */
    pub fn window(&mut self, len: usize, rows: usize) -> Range<usize> {
        (self.rows, self.len) = (rows, len);
        self.offset = self.offset.min(self.max_offset());
        self.offset..(self.offset + rows).min(len)
    }

    /**
    The columns of lines up to `width` wide to show in `cols`, for this
    frame: the offset clamped so the widest line's end is at the right edge
    at the furthest. Remembers both for the keys, as [`Scroll::window`]
    does the rows. [`TextView::sideways`] calls it.
    */
    pub fn window_cols(&mut self, width: usize, cols: usize) -> Range<usize> {
        (self.cols, self.width) = (cols, width);
        self.col = self.col.min(self.max_col());
        self.col..(self.col + cols).min(width)
    }

    /**
    Bring `line` and its columns `cols` into view: a line out of view is put
    a third of the way down the page, the columns scrolled to as little as
    shows them (their start, when they are wider than the view). For a
    search's match; by the last draw's page, in lines as
    [`Scroll::window`] counts them (wrapped rows, for wrapped text).
    */
    pub fn reveal(&mut self, line: usize, cols: Range<usize>) -> NavOutcome {
        let before = (self.offset, self.col);
        let rows = self.rows.max(1);
        if !(self.offset..self.offset + rows).contains(&line) {
            self.offset = line.saturating_sub(rows / REVEAL_DIV);
        }
        let shown = self.cols.max(1);
        if cols.start < self.col {
            self.col = cols.start;
        } else if cols.end > self.col + shown {
            self.col = cols.end.saturating_sub(shown).min(cols.start);
        }
        match (self.offset, self.col) == before {
            true => NavOutcome::Handled,
            false => NavOutcome::Moved,
        }
    }

    /// Scroll `delta` lines (down if positive), within the last draw's lines.
    pub fn by(&mut self, delta: isize) -> NavOutcome {
        let target = self.offset.saturating_add_signed(delta);
        self.go(target)
    }

    pub fn top(&mut self) -> NavOutcome {
        self.go(0)
    }

    pub fn bottom(&mut self) -> NavOutcome {
        self.go(usize::MAX)
    }

    /// Up by a page less a line (the page of the last draw).
    pub fn page_up(&mut self) -> NavOutcome {
        self.go(self.offset.saturating_sub(page_step(self.page())))
    }

    /// Down by a page less a line (the page of the last draw).
    pub fn page_down(&mut self) -> NavOutcome {
        self.go(self.offset.saturating_add(page_step(self.page())))
    }

    /**
    One key: ↑ ↓ Home End PgUp PgDn, without modifiers. Anything else is
    [`NavOutcome::Ignored`]; a held-down key repeats. `Moved` when the view
    scrolled, `Handled` at the end already.
    */
    pub fn key(&mut self, key: KeyEvent) -> NavOutcome {
        self.key_with(key, KeyModifiers::NONE)
    }

    /**
    As [`Scroll::key`], with exactly `modifiers` held: Shift for a detail
    pane whose plain keys move the list beside it. Any other chord is
    [`NavOutcome::Ignored`].
    */
    pub fn key_with(&mut self, key: KeyEvent, modifiers: KeyModifiers) -> NavOutcome {
        if key.kind == KeyEventKind::Release || key.modifiers != modifiers {
            return NavOutcome::Ignored;
        }
        match key.code {
            KeyCode::Up => self.by(-1),
            KeyCode::Down => self.by(1),
            KeyCode::Home => self.top(),
            KeyCode::End => self.bottom(),
            KeyCode::PageUp => self.page_up(),
            KeyCode::PageDown => self.page_down(),
            _ => NavOutcome::Ignored,
        }
    }

    /// Scroll `delta` columns sideways (right if positive), within the last draw's widest line.
    pub fn by_cols(&mut self, delta: isize) -> NavOutcome {
        let before = self.col;
        self.col = self.col.saturating_add_signed(delta).min(self.max_col());
        match self.col == before {
            true => NavOutcome::Handled,
            false => NavOutcome::Moved,
        }
    }

    /**
    One key sideways: ← → by half the columns of the last draw (as `less`
    does), without modifiers. Apart from [`Scroll::key`], since many apps
    take ← for going back; an app scrolling sideways handles both. Anything
    else is [`NavOutcome::Ignored`]; `Handled` at the edge already.
    */
    pub fn key_sideways(&mut self, key: KeyEvent) -> NavOutcome {
        if key.kind == KeyEventKind::Release || key.modifiers != KeyModifiers::NONE {
            return NavOutcome::Ignored;
        }
        // usize::MAX / 2 columns fit in an isize
        let step = isize::try_from((self.cols / 2).max(1)).unwrap_or(isize::MAX);
        match key.code {
            KeyCode::Left => self.by_cols(-step),
            KeyCode::Right => self.by_cols(step),
            _ => NavOutcome::Ignored,
        }
    }

    /**
    `{what} a–b of N` (`lines 21–40 of 300`) for the last
    [`Scroll::window`], for a title; `None` when it all fit.
    */
    pub fn label(&self, what: &str) -> Option<String> {
        let shown = self.offset..(self.offset + self.rows).min(self.len);
        self.overflows().then(|| range_label(what, shown, self.len))
    }

    /**
    `{what} a–b of N` (`cols 81–160 of 2048`) for the last
    [`Scroll::window_cols`], for a title; `None` when every line fit.
    */
    pub fn label_cols(&self, what: &str) -> Option<String> {
        let shown = self.col..(self.col + self.cols).min(self.width);
        self.overflows_sideways().then(|| range_label(what, shown, self.width))
    }

    fn max_offset(&self) -> usize {
        self.len.saturating_sub(self.rows.max(1))
    }

    fn max_col(&self) -> usize {
        self.width.saturating_sub(self.cols.max(1))
    }

    fn go(&mut self, target: usize) -> NavOutcome {
        let before = self.offset;
        self.offset = target.min(self.max_offset());
        match self.offset == before {
            true => NavOutcome::Handled,
            false => NavOutcome::Moved,
        }
    }
}

/* ===== TextView =========================================================== */

impl<'a> TextView<'a> {
    pub fn new(text: impl Into<Text<'a>>) -> Self {
        Self {
            text: text.into(),
            wrap: false,
            sideways: false,
            numbers: false,
            number_style: Style::new().dim(),
            chrome: Chrome::default(),
        }
    }

    /// Word-wrap the lines to the width (scrolling then goes by wrapped rows).
    pub fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    /**
    Lines wider than the view scroll sideways ([`Scroll::key_sideways`],
    [`Scroll::set_col`]) instead of being cut at the right edge, and the
    indicator says which columns are in view. Wrapped text has nothing to
    the side: with [`TextView::wrap`] this does nothing.
    */
    pub fn sideways(mut self, sideways: bool) -> Self {
        self.sideways = sideways;
        self
    }

    /// Number the lines in a gutter on the left, which stays put sideways; a wrapped line is numbered on its first row.
    pub fn line_numbers(mut self, numbers: bool) -> Self {
        self.numbers = numbers;
        self
    }

    /// The line numbers' style (dim by default).
    pub fn line_number_style(mut self, style: impl Into<Style>) -> Self {
        self.number_style = style.into();
        self
    }

    /// The block around the text; the position indicator goes in its bottom border.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.chrome.block = Some(block);
        self
    }

    /// What the position indicator counts (`lines`, the default); `None` for no
    /// indicator, e.g. with a block that has no bottom border.
    pub fn indicator(mut self, what: impl Into<Option<&'a str>>) -> Self {
        self.chrome.what = what.into();
        self
    }

    /// The indicator's style (dim by default).
    pub fn indicator_style(mut self, style: impl Into<Style>) -> Self {
        self.chrome.style = style.into();
        self
    }
}

impl StatefulWidget for TextView<'_> {
    type State = Scroll;

    fn render(self, area: Rect, buf: &mut Buffer, scroll: &mut Scroll) {
        let inner = self.chrome.inner(area);
        let mut text = self.text;
        let len = text.lines.len();
        let gutter = match self.numbers {
            true => u16::try_from(number_cols(len) + GUTTER_GAP).unwrap_or(u16::MAX).min(inner.width),
            false => 0,
        };
        let body = Rect { x: inner.x + gutter, width: inner.width - gutter, ..inner };
        let rows = usize::from(body.height);
        // (line, row) of each line number in view
        let mut numbered: Vec<(usize, u16)> = Vec::new();
        if self.wrap {
            // each line wraps on its own: its rows, to find the lines in view
            let width = body.width.max(1);
            let counts: Vec<usize> = text.lines.iter().map(|l| wrapped_rows(l, width)).collect();
            let shown = scroll.window(counts.iter().sum(), rows);
            scroll.window_cols(0, body.width.into());
            let (mut first, mut skip) = (0, shown.start);
            while first < len && skip >= counts[first] {
                skip -= counts[first];
                first += 1;
            }
            // from the first line in view, as many as reach the bottom
            let mut end = first;
            let mut row = 0;
            while end < len && row < skip + rows {
                if row >= skip {
                    numbered.push((end, to_u16(row - skip)));
                }
                row += counts[end];
                end += 1;
            }
            text.lines.truncate(end);
            text.lines.drain(..first);
            self.chrome.render(area, buf, scroll);
            // under a line's rows, a u16: only a line wrapped past 65535 rows stops short
            let offset = to_u16(skip);
            Paragraph::new(text).wrap(Wrap { trim: false }).scroll((offset, 0)).render(body, buf);
        } else {
            // the widest of all the lines, not only those in view: the offset stays as the view moves
            let widest = match self.sideways {
                true => text.lines.iter().map(Line::width).max().unwrap_or(0),
                false => 0,
            };
            // only the lines in view are handed on
            let shown = scroll.window(len, rows);
            text.lines.truncate(shown.end);
            text.lines.drain(..shown.start);
            numbered.extend((shown.start..shown.end).zip(0..));
            let cols = scroll.window_cols(widest, body.width.into());
            if cols.start > 0 {
                text.lines = text.lines.into_iter().map(|l| skip_cols(l, cols.start)).collect();
            }
            self.chrome.render(area, buf, scroll);
            Paragraph::new(text).render(body, buf);
        }
        if gutter > 0 {
            let digits = usize::from(gutter).saturating_sub(GUTTER_GAP);
            for (line, row) in numbered {
                let number = Span::styled(format!("{:>digits$}", line + 1), self.number_style);
                buf.set_span(inner.x, inner.y + row, &number, gutter);
            }
        }
    }
}

/// The columns the numbers of `len` lines take, the widest being `len`.
fn number_cols(len: usize) -> usize {
    len.max(1).ilog10() as usize + 1
}

/// The rows `line` takes word-wrapped to `width`, as `Paragraph` wraps it; its spans borrowed, not copied.
fn wrapped_rows(line: &Line<'_>, width: u16) -> usize {
    let spans: Vec<Span> = line.spans.iter().map(|s| Span::styled(s.content.as_ref(), s.style)).collect();
    let borrowed = Line { style: line.style, alignment: line.alignment, spans };
    Paragraph::new(borrowed).wrap(Wrap { trim: false }).line_count(width)
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/* ===== CanvasView ========================================================= */

impl<'a> CanvasView<'a> {
    pub fn new(canvas: &'a Buffer) -> Self {
        Self {
            canvas,
            chrome: Chrome::default(),
        }
    }

    /// The block around the view; the position indicator goes in its bottom border.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.chrome.block = Some(block);
        self
    }

    /// What the position indicator counts (`lines`, the default); `None` for no
    /// indicator, e.g. with a block that has no bottom border.
    pub fn indicator(mut self, what: impl Into<Option<&'a str>>) -> Self {
        self.chrome.what = what.into();
        self
    }

    /// The indicator's style (dim by default).
    pub fn indicator_style(mut self, style: impl Into<Style>) -> Self {
        self.chrome.style = style.into();
        self
    }
}

impl StatefulWidget for CanvasView<'_> {
    type State = Scroll;

    fn render(self, area: Rect, buf: &mut Buffer, scroll: &mut Scroll) {
        let inner = self.chrome.inner(area);
        let src = self.canvas.area;
        let shown = scroll.window(src.height.into(), inner.height.into());
        self.chrome.render(area, buf, scroll);
        let cols = inner.width.min(src.width);
        for (dy, y) in (0..).zip(shown) {
            // y < src.height, a u16
            let sy = src.y + y as u16;
            for dx in 0..cols {
                if let (Some(from), Some(to)) = (
                    self.canvas.cell((src.x + dx, sy)),
                    buf.cell_mut((inner.x + dx, inner.y + dy)),
                ) {
                    *to = from.clone();
                }
            }
        }
    }
}

/* ===== Chrome ============================================================= */

/// Columns a border needs to show the default indicator in full for `len`
/// lines, at its widest.
pub(crate) fn indicator_width(len: usize) -> usize {
    let label = range_label(DEFAULT_WHAT, len.saturating_sub(1)..len, len);
    label.width() + INDICATOR_CHROME
}

impl Default for Chrome<'_> {
    fn default() -> Self {
        Self {
            block: None,
            what: Some(DEFAULT_WHAT),
            style: Style::new().dim(),
        }
    }
}

impl Chrome<'_> {
    pub(crate) fn inner(&self, area: Rect) -> Rect {
        self.block.as_ref().map_or(area, |b| b.inner(area))
    }

    /// The block, with the indicator in its bottom border when the view overflows.
    pub(crate) fn render(self, area: Rect, buf: &mut Buffer, scroll: &Scroll) {
        let Some(mut block) = self.block else {
            return;
        };
        let lines: Option<String> = self.what.and_then(|what| scroll.label(what));
        let cols: Option<String> = self.what.and_then(|_| scroll.label_cols(COLS_WHAT));
        let label: Option<String> = match (lines, cols) {
            (Some(lines), Some(cols)) => Some(format!("{lines}{INDICATOR_SEP}{cols}")),
            (lines, cols) => lines.or(cols),
        };
        if let Some(label) = label {
            block = block.title_bottom(
                Line::from(format!(" {label} "))
                    .style(self.style)
                    .right_aligned(),
            );
        }
        block.render(area, buf);
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    use crate::buffer_rows;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    /// 100 lines in 10 rows, at `offset`.
    fn at(offset: usize) -> Scroll {
        let mut s = Scroll::new();
        s.set_offset(offset);
        s.window(100, 10);
        s
    }

    #[test]
    fn the_last_page_stays_full() {
        let mut s = at(95);
        assert_eq!(s.offset(), 90);
        assert_eq!(s.window(100, 20), 80..100, "taller rows: scrolls back");
        assert_eq!(s.window(5, 20), 0..5, "it all fits: the top");
    }

    #[test]
    fn moves_stay_within_the_last_draw() {
        let mut s = at(0);
        assert_eq!(s.by(-1), NavOutcome::Handled);
        assert_eq!(s.by(3), NavOutcome::Moved);
        assert_eq!(s.offset(), 3);
        assert_eq!(s.bottom(), NavOutcome::Moved);
        assert_eq!(s.offset(), 90);
        assert_eq!(s.by(1), NavOutcome::Handled, "no growing past the end");
        assert_eq!(s.by(-1), NavOutcome::Moved, "so ↑ moves at once");
        assert_eq!(s.top(), NavOutcome::Moved);
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn pages_overlap_by_a_line() {
        let mut s = at(0);
        s.page_down();
        assert_eq!(s.offset(), 9);
        s.page_up();
        assert_eq!(s.offset(), 0);
        // before any draw: nothing known to scroll through
        assert_eq!(Scroll::new().page_down(), NavOutcome::Handled);
    }

    #[test]
    fn keys_with_exactly_the_modifiers() {
        let mut s = at(0);
        assert_eq!(s.key(key(KeyCode::End)), NavOutcome::Moved);
        let shift_up = KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(s.key(shift_up), NavOutcome::Ignored);
        assert_eq!(s.key_with(shift_up, KeyModifiers::SHIFT), NavOutcome::Moved);
        assert_eq!(
            s.key_with(key(KeyCode::Up), KeyModifiers::SHIFT),
            NavOutcome::Ignored
        );
        let mut release = key(KeyCode::Up);
        release.kind = KeyEventKind::Release;
        assert_eq!(s.key(release), NavOutcome::Ignored);
        assert_eq!(s.key(key(KeyCode::Char('j'))), NavOutcome::Ignored);
    }

    #[test]
    fn label_only_when_it_overflows() {
        assert_eq!(at(20).label("lines").as_deref(), Some("lines 21–30 of 100"));
        let mut s = Scroll::new();
        s.window(10, 10);
        assert_eq!(s.label("lines"), None);
    }

    #[test]
    fn text_view_shows_the_window_and_the_indicator() {
        let text: Vec<Line> = (1..=20).map(|i| Line::from(format!("l{i}"))).collect();
        let mut buf = Buffer::empty(Rect::new(0, 0, 24, 7));
        let mut s = Scroll::new();
        s.set_offset(3);
        TextView::new(text)
            .block(Block::bordered())
            .render(buf.area, &mut buf, &mut s);
        let rows = buffer_rows(&buf);
        assert!(rows[1].starts_with("│l4 "), "{rows:?}");
        assert!(rows[5].starts_with("│l8 "), "{rows:?}");
        assert!(rows[6].contains(" lines 4–8 of 20 "), "{rows:?}");
    }

    #[test]
    fn text_view_scrolls_wrapped_rows() {
        // three lines of 10 columns each wrap to two rows of 5
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 2));
        let mut s = Scroll::new();
        s.set_offset(3);
        TextView::new("aaaa bbbb\ncccc dddd\neeee ffff")
            .wrap(true)
            .render(buf.area, &mut buf, &mut s);
        assert_eq!(s.len(), 6);
        assert_eq!(buffer_rows(&buf), ["dddd ", "eeee "]);
    }

    /// Lines up to 100 columns wide in 20, at column `col`.
    fn at_col(col: usize) -> Scroll {
        let mut s = Scroll::new();
        s.set_col(col);
        s.window_cols(100, 20);
        s
    }

    #[test]
    fn sideways_the_widest_end_stays_at_the_edge() {
        let mut s = at_col(95);
        assert_eq!(s.col(), 80);
        assert_eq!(s.window_cols(100, 40), 60..100, "wider view: scrolls back");
        assert_eq!(s.window_cols(10, 40), 0..10, "it all fits: the left edge");
        assert!(!s.overflows_sideways());
    }

    #[test]
    fn sideways_keys_go_half_the_width() {
        let mut s = at_col(0);
        assert_eq!(s.key_sideways(key(KeyCode::Left)), NavOutcome::Handled, "at the edge");
        assert_eq!(s.key_sideways(key(KeyCode::Right)), NavOutcome::Moved);
        assert_eq!(s.col(), 10);
        assert_eq!(s.by_cols(1000), NavOutcome::Moved);
        assert_eq!(s.col(), 80, "no further than the widest line's end");
        assert_eq!(s.key_sideways(key(KeyCode::Right)), NavOutcome::Handled);
        assert_eq!(s.key_sideways(key(KeyCode::Up)), NavOutcome::Ignored);
        let ctrl_left = KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(s.key_sideways(ctrl_left), NavOutcome::Ignored);
        assert_eq!(s.key(key(KeyCode::Left)), NavOutcome::Ignored, "key stays up and down");
        s.reset();
        assert_eq!(s.col(), 0);
    }

    #[test]
    fn reveal_brings_a_line_and_its_columns_into_view() {
        let mut s = at(0);
        s.window_cols(100, 20);
        assert_eq!(s.reveal(5, 2..6), NavOutcome::Handled, "in view already");
        assert_eq!(s.reveal(50, 2..6), NavOutcome::Moved);
        assert_eq!(s.offset(), 47, "a third of the page above it");
        s.reveal(50, 60..64);
        assert_eq!(s.col(), 44, "just far enough to show it");
        s.reveal(50, 50..90);
        assert_eq!(s.col(), 50, "wider than the view: its start");
        s.reveal(0, 3..4);
        assert_eq!((s.offset(), s.col()), (0, 3));
    }

    #[test]
    fn label_cols_only_when_a_line_overflows() {
        assert_eq!(at_col(20).label_cols("cols").as_deref(), Some("cols 21–40 of 100"));
        let mut s = Scroll::new();
        s.window_cols(20, 20);
        assert_eq!(s.label_cols("cols"), None);
    }

    #[test]
    fn text_view_numbers_its_lines_and_scrolls_sideways() {
        let abc = "abcdefghijklmnopqrstuvwxyz".repeat(3);
        let text: Vec<Line> = (1..=12).map(|i| Line::from(format!("{i:02}-{abc}"))).collect();
        let mut buf = Buffer::empty(Rect::new(0, 0, 50, 5));
        let mut s = Scroll::new();
        s.set_offset(8);
        s.set_col(3);
        TextView::new(text.clone())
            .sideways(true)
            .line_numbers(true)
            .block(Block::bordered())
            .render(buf.area, &mut buf, &mut s);
        let rows = buffer_rows(&buf);
        // the gutter is 2 digits and a space, the text 45 columns from column 3
        let shown = &abc[..45];
        assert_eq!(rows[1], format!("│ 9 {shown}│"), "{rows:?}");
        assert_eq!(rows[3], format!("│11 {shown}│"), "{rows:?}");
        assert!(rows[4].contains(" lines 9–11 of 12 · cols 4–48 of 81 "), "{rows:?}");
        // not sideways: cut at the edge, the offset left alone
        let mut buf = Buffer::empty(Rect::new(0, 0, 16, 5));
        TextView::new(text).render(buf.area, &mut buf, &mut s);
        assert_eq!(buffer_rows(&buf)[0], "08-abcdefghijklm");
        assert!(!s.overflows_sideways());
    }

    #[test]
    fn text_view_numbers_a_wrapped_line_once() {
        // `aaaa bbbb` wraps to two rows of 5 beside a 2-column gutter
        let mut buf = Buffer::empty(Rect::new(0, 0, 7, 4));
        let mut s = Scroll::new();
        s.set_offset(1);
        TextView::new("aaaa bbbb\ncc\ndddd eeee")
            .wrap(true)
            .line_numbers(true)
            .render(buf.area, &mut buf, &mut s);
        assert_eq!(s.len(), 5);
        assert_eq!(buffer_rows(&buf), ["  bbbb ", "2 cc   ", "3 dddd ", "  eeee "]);
    }

    #[test]
    fn canvas_view_copies_the_rows_in_view() {
        let mut canvas = Buffer::empty(Rect::new(0, 0, 3, 10));
        for y in 0..10 {
            canvas.set_string(0, y, format!("r{y}"), Style::new());
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 4));
        let mut s = Scroll::new();
        s.set_offset(99);
        CanvasView::new(&canvas)
            .block(Block::bordered())
            .indicator(None)
            .render(buf.area, &mut buf, &mut s);
        assert_eq!(s.offset(), 8);
        assert_eq!(buffer_rows(&buf), ["┌───┐", "│r8 │", "│r9 │", "└───┘"]);
    }
}
