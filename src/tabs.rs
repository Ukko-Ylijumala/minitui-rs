// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A row of tabs: [`TabBar`] shows each tab's title (the app's own line, a
count or a badge styled as it likes), the selected one highlighted, a
divider between them; when they don't all fit, the selected one alone
between `‹ ›` and its place among them. squawk's host view had the only
tab bar, ratatui's `Tabs` with the selected one yellow and bold and
nothing for a narrow terminal; wiremeup drew a form's choice as
`‹ option ›`, which is what a tab bar too narrow for its tabs becomes.
*/

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::text::{Cut, fit_line};

/* ===== consts and statics ================================================= */

static DIVIDER: &str = " │ ";
static PREV: &str = "‹ ";
static NEXT: &str = " ›";
/// Columns before the first tab, as in a block's title.
const LEAD: usize = 1;

/* ===== types ============================================================== */

/**
The tabs in one row. The selection and the keys are the app's (Tab /
Shift-Tab, number keys): [`tab_step`] moves through them wrapping.

```no_run
# use ratatui::Frame;
# use ratatui::layout::Rect;
use minitui::TabBar;

# fn draw(frame: &mut Frame, area: Rect, selected: usize, plugins: usize) {
let tabs = TabBar::new(["overview".to_string(), format!("plugins {plugins}")]).select(selected);
frame.render_widget(tabs, area);
# }
```
*/
#[derive(Debug, Clone)]
pub struct TabBar<'a> {
    tabs: Vec<Line<'a>>,
    selected: usize,
    divider: &'a str,
    style: Style,
    selected_style: Style,
}

/* ===== TabBar ============================================================= */

impl<'a> TabBar<'a> {
    /// The tabs dim, the first selected (yellow, bold), ` │ ` between them.
    pub fn new<L: Into<Line<'a>>>(tabs: impl IntoIterator<Item = L>) -> Self {
        Self {
            tabs: tabs.into_iter().map(Into::into).collect(),
            selected: 0,
            divider: DIVIDER,
            style: Style::new().fg(Color::DarkGray),
            selected_style: Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        }
    }

    /// The selected tab; past the last, the last.
    pub fn select(mut self, selected: usize) -> Self {
        self.selected = selected;
        self
    }

    pub fn divider(mut self, divider: &'a str) -> Self {
        self.divider = divider;
        self
    }

    /// Under the tabs that are not selected, and the divider.
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// Patched over the selected tab.
    pub fn selected_style(mut self, style: impl Into<Style>) -> Self {
        self.selected_style = style.into();
        self
    }

    /// Columns all the tabs take in a row.
    pub fn width(&self) -> usize {
        let dividers = self.divider.width() * self.tabs.len().saturating_sub(1);
        LEAD + self.tabs.iter().map(Line::width).sum::<usize>() + dividers
    }

    /// The row for `width` columns: every tab, or the selected one alone when they don't fit.
    pub fn line(&self, width: usize) -> Line<'a> {
        let Some(last) = self.tabs.len().checked_sub(1) else {
            return Line::default();
        };
        let selected = self.selected.min(last);
        let tab = |i: usize| -> Vec<Span<'a>> {
            let style = match i == selected {
                true => self.style.patch(self.selected_style),
                false => self.style,
            };
            // the tab's own styles over the bar's, so a coloured count stays coloured
            let line = &self.tabs[i];
            let base = style.patch(line.style);
            line.spans.iter().map(|span| Span::styled(span.content.clone(), base.patch(span.style))).collect()
        };
        let mut spans: Vec<Span<'a>> = vec![Span::raw(" ".repeat(LEAD))];
        if self.width() <= width {
            for i in 0..=last {
                if i > 0 {
                    spans.push(Span::styled(self.divider, self.style));
                }
                spans.extend(tab(i));
            }
            return Line::from(spans);
        }
        // the selected tab alone, between arrows dimmed at either end, and its place
        let arrow = |text: &'a str, live: bool| match live {
            true => Span::styled(text, self.style.patch(self.selected_style).remove_modifier(Modifier::BOLD)),
            false => Span::styled(text, self.style),
        };
        spans.push(arrow(PREV, selected > 0));
        spans.extend(tab(selected));
        spans.push(arrow(NEXT, selected < last));
        // the place only when it fits whole
        let place = Span::styled(format!(" {}/{}", selected + 1, last + 1), self.style);
        if Line::from(spans.clone()).width() + place.width() <= width {
            spans.push(place);
        }
        fit_line(Line::from(spans), width, Cut::End)
    }
}

impl Widget for TabBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let row = Rect { height: 1, ..area };
        self.line(area.width.into()).render(row, buf);
    }
}

/// The tab `delta` steps from `selected` among `len`, wrapping; 0 for none.
pub fn tab_step(selected: usize, len: usize, delta: isize) -> usize {
    match len {
        0 => 0,
        n => (selected.min(n - 1) as isize + delta).rem_euclid(n as isize) as usize,
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{buffer_text, render_buffer};

    fn tabs() -> TabBar<'static> {
        TabBar::new([
            Line::from("overview"),
            Line::from(vec![Span::raw("plugins "), Span::styled("24", Style::new().fg(Color::Cyan))]),
            Line::from("themes 3"),
        ])
    }

    #[test]
    fn all_of_them_or_the_selected_one() {
        let bar = tabs().select(1);
        assert_eq!(bar.width(), 33);
        assert_eq!(buffer_text(&render_buffer(bar.clone(), 36, 1)), " overview │ plugins 24 │ themes 3   ");
        let buf = render_buffer(bar.clone(), 36, 1);
        assert_eq!(buf[(1, 0)].fg, Color::DarkGray);
        assert_eq!(buf[(12, 0)].fg, Color::Yellow, "the selected tab");
        assert!(buf[(12, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(20, 0)].fg, Color::Cyan, "its own styled count keeps its colour");
        // narrow: the selected one, its neighbours behind the arrows
        assert_eq!(buffer_text(&render_buffer(bar.clone(), 24, 1)), " ‹ plugins 24 › 2/3     ");
        assert_eq!(buffer_text(&render_buffer(bar.select(0), 14, 1)), " ‹ overview › ");
        assert_eq!(buffer_text(&render_buffer(TabBar::new(Vec::<Line>::new()), 5, 1)), "     ");
    }

    #[test]
    fn stepping_wraps() {
        assert_eq!(tab_step(0, 3, 1), 1);
        assert_eq!(tab_step(2, 3, 1), 0);
        assert_eq!(tab_step(0, 3, -1), 2);
        assert_eq!(tab_step(9, 3, 0), 2, "past the end is the last");
        assert_eq!(tab_step(0, 0, 1), 0);
    }
}
