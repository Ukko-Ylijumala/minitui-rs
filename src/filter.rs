// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A list's filter typed as you go: [`Filter`] keeps the text in a
[`LineEditor`] and whether it is being typed, and decides what Enter and
Esc mean for it; [`FilterBar`] draws it with how many rows it leaves.
What the text matches is the app's, as is keeping the selection on its
item while the rows change ([`ListCursor::reselect`](crate::ListCursor::reselect)).
Keys that are not editing keys (↑ ↓ PgUp PgDn) come back
[`Ignored`](FilterOutcome::Ignored), so the list moves while the filter
is typed.
*/

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::{EditOutcome, LineEditor};

/* ===== consts and statics ================================================= */

static DEFAULT_LABEL: &str = "filter";
/// Columns between the label and the editor, and before the count.
const GAP: usize = 1;

/* ===== types ============================================================== */

/// What a key did to a [`Filter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOutcome {
    /// The text changed: filter the rows again.
    Changed,
    /// Enter: the typing is done, the filter stays.
    Done,
    /// Esc: the filter is gone: show every row again.
    Cleared,
    /// An editing key that changed nothing (a cursor move).
    Handled,
    /// Not the filter's key, or the filter isn't being typed: the app's.
    Ignored,
}

/**
The filter's text and whether it is being typed. [`start`](Filter::start)
on the app's key (`/`); while typing, Enter keeps the filter and Esc drops
it; afterwards the app clears it with [`clear`](Filter::clear) (Esc, say).

```no_run
# use crossterm::event::{KeyCode, KeyEvent};
use minitui::{Filter, FilterOutcome};

# fn demo(key: KeyEvent, filter: &mut Filter, refilter: impl Fn(&str)) {
match filter.handle_key(key) {
    FilterOutcome::Changed | FilterOutcome::Cleared => refilter(filter.text()),
    FilterOutcome::Done | FilterOutcome::Handled => {}
    FilterOutcome::Ignored if key.code == KeyCode::Char('/') => filter.start(),
    FilterOutcome::Ignored => { /* the list's keys */ }
}
# }
```
*/
#[derive(Debug, Clone, Default)]
pub struct Filter {
    editor: LineEditor,
    editing: bool,
}

/**
The filter in one row: `filter > text` (the cursor cell while typing),
and on the right how many rows it leaves of how many. Draws nothing for
a filter that is neither typed nor set.
*/
#[derive(Debug, Clone)]
pub struct FilterBar<'a> {
    filter: &'a Filter,
    label: &'a str,
    count: Option<(usize, usize)>,
    label_style: Style,
    count_style: Style,
}

/* ===== Filter ============================================================= */

impl Filter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start typing, the text kept and the cursor at its end.
    pub fn start(&mut self) {
        self.editing = true;
        self.editor.set_cursor(self.editor.text().len());
    }

    /// Whether it is being typed.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Whether it filters anything: some text.
    pub fn is_active(&self) -> bool {
        !self.editor.is_empty()
    }

    pub fn text(&self) -> &str {
        self.editor.text()
    }

    /// No filter, and no typing.
    pub fn clear(&mut self) {
        self.editor.clear();
        self.editing = false;
    }

    /// One key; see [`FilterOutcome`]. Every key is [`Ignored`](FilterOutcome::Ignored) when not typing.
    pub fn handle_key(&mut self, key: KeyEvent) -> FilterOutcome {
        if !self.editing || key.kind == KeyEventKind::Release {
            return FilterOutcome::Ignored;
        }
        match key.code {
            KeyCode::Enter => {
                self.editing = false;
                FilterOutcome::Done
            }
            KeyCode::Esc => {
                self.clear();
                FilterOutcome::Cleared
            }
            _ => match self.editor.handle_key(key) {
                EditOutcome::Changed => FilterOutcome::Changed,
                EditOutcome::Handled => FilterOutcome::Handled,
                EditOutcome::Ignored => FilterOutcome::Ignored,
            },
        }
    }

    /// The bar that shows it.
    pub fn bar(&self) -> FilterBar<'_> {
        FilterBar {
            filter: self,
            label: DEFAULT_LABEL,
            count: None,
            label_style: Style::new().fg(Color::Cyan),
            count_style: Style::new().fg(Color::DarkGray),
        }
    }
}

/* ===== FilterBar ========================================================== */

impl<'a> FilterBar<'a> {
    /// Before the text (`filter`).
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    /// `shown of total` on the right.
    pub fn count(mut self, shown: usize, total: usize) -> Self {
        self.count = Some((shown, total));
        self
    }

    pub fn label_style(mut self, style: impl Into<Style>) -> Self {
        self.label_style = style.into();
        self
    }

    pub fn count_style(mut self, style: impl Into<Style>) -> Self {
        self.count_style = style.into();
        self
    }

    /// Whether there is anything to draw: a filter typed or set.
    pub fn is_shown(&self) -> bool {
        self.filter.editing || self.filter.is_active()
    }

    /// The row for `width` columns; empty when there is nothing to show.
    pub fn line(&self, width: usize) -> Line<'static> {
        if !self.is_shown() {
            return Line::default();
        }
        let count: String = self.count.map_or(String::new(), |(shown, total)| format!("{shown} of {total}"));
        let label_w = self.label.width() + GAP;
        let count_w = if count.is_empty() { 0 } else { count.width() + GAP };
        // the text first: the count only where the whole text leaves room for it
        let room16 = u16::try_from(width.saturating_sub(label_w)).unwrap_or(u16::MAX);
        let edited: Line = match self.filter.editing {
            true => self.filter.editor.line(room16),
            false => self.filter.editor.idle_line(room16),
        };
        let used = label_w + edited.width();
        let mut spans: Vec<Span> = vec![Span::styled(format!("{} ", self.label), self.label_style)];
        spans.extend(edited.spans);
        if !count.is_empty() && used + count_w <= width {
            spans.push(Span::raw(" ".repeat(width - used - count.width())));
            spans.push(Span::styled(count, self.count_style));
        }
        Line::from(spans)
    }
}

impl Widget for FilterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let row = Rect { height: 1, ..area };
        self.line(area.width.into()).render(row, buf);
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{buffer_text, render_buffer};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn typed(filter: &mut Filter, text: &str) {
        for c in text.chars() {
            assert_eq!(filter.handle_key(key(KeyCode::Char(c))), FilterOutcome::Changed);
        }
    }

    #[test]
    fn typed_kept_and_dropped() {
        let mut filter = Filter::new();
        assert_eq!(filter.handle_key(key(KeyCode::Char('a'))), FilterOutcome::Ignored, "not typing yet");
        filter.start();
        typed(&mut filter, "akis");
        assert_eq!((filter.text(), filter.is_editing(), filter.is_active()), ("akis", true, true));
        assert_eq!(filter.handle_key(key(KeyCode::Down)), FilterOutcome::Ignored, "the list's key");
        assert_eq!(filter.handle_key(key(KeyCode::Left)), FilterOutcome::Handled);
        assert_eq!(filter.handle_key(key(KeyCode::Enter)), FilterOutcome::Done);
        assert_eq!((filter.text(), filter.is_editing()), ("akis", false));
        // typed again: the text kept, more added at its end
        filter.start();
        typed(&mut filter, "met");
        assert_eq!(filter.text(), "akismet");
        assert_eq!(filter.handle_key(key(KeyCode::Esc)), FilterOutcome::Cleared);
        assert_eq!((filter.text(), filter.is_editing(), filter.is_active()), ("", false, false));
    }

    #[test]
    fn the_bar() {
        let mut filter = Filter::new();
        assert!(!filter.bar().is_shown());
        assert_eq!(buffer_text(&render_buffer(filter.bar(), 30, 1)), " ".repeat(30));
        filter.start();
        typed(&mut filter, "akismet");
        let text = buffer_text(&render_buffer(filter.bar().count(3, 120), 40, 1));
        assert_eq!(text, "filter > akismet                3 of 120");
        filter.handle_key(key(KeyCode::Enter));
        let text = buffer_text(&render_buffer(filter.bar().label("/").count(3, 120), 30, 1));
        assert_eq!(text, "/ > akismet           3 of 120", "set, not typed: no cursor cell");
        // too narrow for the count: it goes
        let text = buffer_text(&render_buffer(filter.bar().count(3, 120), 18, 1));
        assert_eq!(text, "filter > akismet  ");
    }
}
