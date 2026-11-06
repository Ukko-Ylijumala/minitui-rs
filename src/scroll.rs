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
*/

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph, StatefulWidget, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::list::page_step;
use crate::{NavOutcome, range_label};

/* ===== consts and statics ================================================= */

/// What the position indicator counts, by default.
static DEFAULT_WHAT: &str = "lines";
/// Columns around the indicator's text: a space either side, the corners.
const INDICATOR_CHROME: usize = 4;

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
struct Chrome<'a> {
    block: Option<Block<'a>>,
    /// What the indicator counts; `None` for no indicator.
    what: Option<&'a str>,
    style: Style,
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

    /// Back to the top: for other content in the same place (a new selection).
    pub fn reset(&mut self) {
        self.offset = 0;
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

    /**
    `{what} a–b of N` (`lines 21–40 of 300`) for the last
    [`Scroll::window`], for a title; `None` when it all fit.
    */
    pub fn label(&self, what: &str) -> Option<String> {
        let shown = self.offset..(self.offset + self.rows).min(self.len);
        self.overflows().then(|| range_label(what, shown, self.len))
    }

    fn max_offset(&self) -> usize {
        self.len.saturating_sub(self.rows.max(1))
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
            chrome: Chrome::default(),
        }
    }

    /// Word-wrap the lines to the width (scrolling then goes by wrapped rows).
    pub fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
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
        if self.wrap {
            let wrapped = Paragraph::new(text).wrap(Wrap { trim: false });
            let len = wrapped.line_count(inner.width.max(1));
            let shown = scroll.window(len, inner.height.into());
            // Paragraph scrolls by u16: past that many wrapped rows it stops
            let offset = u16::try_from(shown.start).unwrap_or(u16::MAX);
            self.chrome.render(area, buf, scroll);
            wrapped.scroll((offset, 0)).render(inner, buf);
        } else {
            // only the lines in view are handed on
            let shown = scroll.window(text.lines.len(), inner.height.into());
            text.lines.truncate(shown.end);
            text.lines.drain(..shown.start);
            self.chrome.render(area, buf, scroll);
            Paragraph::new(text).render(inner, buf);
        }
    }
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
    fn inner(&self, area: Rect) -> Rect {
        self.block.as_ref().map_or(area, |b| b.inner(area))
    }

    /// The block, with the indicator in its bottom border when the view overflows.
    fn render(self, area: Rect, buf: &mut Buffer, scroll: &Scroll) {
        let Some(mut block) = self.block else {
            return;
        };
        if let Some(label) = self.what.and_then(|what| scroll.label(what)) {
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

    fn rows(buf: &Buffer) -> Vec<String> {
        let a = buf.area;
        (a.y..a.bottom())
            .map(|y| (a.x..a.right()).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
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
        let rows = rows(&buf);
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
        assert_eq!(rows(&buf), ["dddd ", "eeee "]);
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
        assert_eq!(rows(&buf), ["┌───┐", "│r8 │", "│r9 │", "└───┘"]);
    }
}
