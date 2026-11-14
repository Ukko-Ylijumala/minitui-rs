// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A search in a text that is on screen: [`TextSearch`] keeps what is looked
for (a [`Filter`], so it is typed and kept and dropped as one), where it
was found in the lines as they are shown, and which of the matches `n`
and `N` step from. It marks the matches in the lines it is given back and
brings the current one into the view's [`Scroll`], sideways or by a
wrapped line's rows.

The text is the app's and stays with it: lines are handed in where they
are needed, as the [`TextView`](crate::TextView) is handed them.

From the file viewers of wp-scanner and the malware scanner, which had
this twice: the search of `less`, case-blind until a capital is typed,
with a cap on the matches so a `e` in a megabyte of text returns.
*/

use std::ops::Range;

use crossterm::event::KeyEvent;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::{Filter, FilterBar, FilterOutcome, Scroll, tab_step, wrap_lines};

/* ===== consts and statics ================================================= */

/// How many matches are found before the search stops looking, by default.
const MATCH_CAP: usize = 10_000;
static LABEL: &str = "search";

/* ===== types ============================================================== */

/**
A search in the lines of a text view.

```no_run
# use ratatui::Frame;
# use ratatui::layout::{Constraint, Layout};
# use ratatui::text::Line;
# use crossterm::event::{KeyCode, KeyEvent};
use minitui::{FilterOutcome, Scroll, TextSearch, TextView};

struct Viewer {
    lines: Vec<Line<'static>>,
    scroll: Scroll,
    search: TextSearch,
    wrap: bool,
    /// the text's columns on the last frame, for a wrapped line's rows
    cols: u16,
}

fn key(v: &mut Viewer, key: KeyEvent) {
    let wrap = v.wrap.then_some(v.cols);
    if v.search.is_editing() {
        // typed: searched again on every key, the first match brought into view
        if v.search.handle_key(key, &v.lines, &mut v.scroll, wrap) == FilterOutcome::Ignored {
            v.scroll.key(key); // ↑ ↓ still scroll
        }
        return;
    }
    match key.code {
        KeyCode::Char('/') => v.search.start(),
        KeyCode::Char('n') => _ = v.search.step(1, &v.lines, &mut v.scroll, wrap),
        KeyCode::Char('N') => _ = v.search.step(-1, &v.lines, &mut v.scroll, wrap),
        KeyCode::Esc => v.search.clear(),
        _ => _ = v.scroll.key(key),
    }
}

fn draw(frame: &mut Frame, v: &mut Viewer) {
    let [body, bar] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    v.cols = body.width;
    // the matches marked, the spans borrowed: no text is copied for a frame
    let view = TextView::new(v.search.marked(&v.lines)).wrap(v.wrap).sideways(!v.wrap);
    frame.render_stateful_widget(view, body, &mut v.scroll);
    frame.render_widget(v.search.bar(), bar); // "search needle     3 of 17"
}
```
*/
#[derive(Debug, Clone)]
pub struct TextSearch {
    filter: Filter,
    /// where it was found: the line, and the bytes of the line as shown
    matches: Vec<(usize, Range<usize>)>,
    /// the match `n` and `N` step from, into `matches`
    current: Option<usize>,
    cap: usize,
    found_style: Option<Style>,
    current_style: Style,
}

/* ===== TextSearch ========================================================= */

impl Default for TextSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSearch {
    pub fn new() -> Self {
        Self {
            filter: Filter::new(),
            matches: Vec::new(),
            current: None,
            cap: MATCH_CAP,
            found_style: Some(Style::new().fg(Color::Black).bg(Color::Yellow)),
            current_style: Style::new().fg(Color::Black).bg(Color::Cyan),
        }
    }

    /// How many matches are found before the search stops looking (10,000
    /// by default; at least one). [`is_capped`](Self::is_capped) says it did.
    #[must_use]
    pub fn cap(mut self, cap: usize) -> Self {
        self.cap = cap.max(1);
        self
    }

    /// The style of every match (black on yellow by default), over the
    /// text's own; `None` to mark the current one only.
    #[must_use]
    pub fn found_style(mut self, style: impl Into<Option<Style>>) -> Self {
        self.found_style = style.into();
        self
    }

    /// The style of the match `n` and `N` are on (black on cyan by default).
    #[must_use]
    pub fn current_style(mut self, style: impl Into<Style>) -> Self {
        self.current_style = style.into();
        self
    }

    /// Start typing the search: the app's `/`. What was searched for stays to be edited.
    pub fn start(&mut self) {
        self.filter.start();
    }

    /// Whether the search is being typed: every character is then its text.
    pub fn is_editing(&self) -> bool {
        self.filter.is_editing()
    }

    /// Whether there is a search: one being typed, or one kept.
    pub fn is_active(&self) -> bool {
        self.filter.is_active()
    }

    /// What is searched for.
    pub fn text(&self) -> &str {
        self.filter.text()
    }

    /// Drop the search and its matches: the app's Esc once the typing is done.
    pub fn clear(&mut self) {
        self.filter.clear();
        self.matches.clear();
        self.current = None;
    }

    /// Where the search was found: the line, and the bytes of the line's text.
    pub fn matches(&self) -> &[(usize, Range<usize>)] {
        &self.matches
    }

    /// The match `n` and `N` step from.
    pub fn current(&self) -> Option<&(usize, Range<usize>)> {
        self.current.and_then(|at| self.matches.get(at))
    }

    /// Which match that is, from 1; 0 without one. For `3 of 17`.
    pub fn position(&self) -> usize {
        self.current.map_or(0, |at| at + 1)
    }

    /// Whether the search stopped at its [`cap`](Self::cap): there may be more.
    pub fn is_capped(&self) -> bool {
        self.matches.len() >= self.cap
    }

    /// The search's bar, labelled `search` and counting `current of found`;
    /// shown while there is a search ([`FilterBar::is_shown`]).
    pub fn bar(&self) -> FilterBar<'_> {
        self.filter
            .bar()
            .label(LABEL)
            .count(self.position(), self.matches.len())
    }

    /**
    A key while the search is typed. A change of the text searches `lines`
    again and brings the first match from the top of the view on into it;
    Enter keeps the search ([`Done`](FilterOutcome::Done): the app may say
    that nothing was found, or that the search was capped), Esc drops it.
    A key that is not the search's comes back
    [`Ignored`](FilterOutcome::Ignored), for the view to scroll by.

    `wrap` is the text's columns when the view wraps its lines, `None`
    when it scrolls sideways: a wrapped line's rows are counted to find
    the match's.
    */
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        lines: &[Line<'_>],
        scroll: &mut Scroll,
        wrap: Option<u16>,
    ) -> FilterOutcome {
        let outcome = self.filter.handle_key(key);
        if matches!(outcome, FilterOutcome::Changed | FilterOutcome::Cleared) {
            self.find(lines, scroll, wrap);
        }
        outcome
    }

    /**
    Search `lines` again: after the text changed under a search that is
    kept (another file, a reload), or for a search set by the app. Every
    match in the lines as shown, up to the cap; case-blind in ASCII unless
    the search has a capital, as `less -i` is. The current match is the
    first from the view's top line on, and is brought into view.
    */
    pub fn find(&mut self, lines: &[Line<'_>], scroll: &mut Scroll, wrap: Option<u16>) {
        self.matches.clear();
        self.current = None;
        let needle = self.filter.text();
        if needle.is_empty() {
            return;
        }
        let fold = !needle.chars().any(|c| c.is_ascii_uppercase());
        let needle = match fold {
            true => needle.to_ascii_lowercase(),
            false => needle.to_string(),
        };
        'lines: for (n, line) in lines.iter().enumerate() {
            let mut shown = shown(line);
            if fold {
                // byte for byte: the ranges stay the shown text's
                shown.make_ascii_lowercase();
            }
            for (at, found) in shown.match_indices(needle.as_str()) {
                if self.matches.len() >= self.cap {
                    break 'lines;
                }
                self.matches.push((n, at..at + found.len()));
            }
        }
        // a wrapped view's offset counts rows, not lines: from the start then
        let top = match wrap {
            Some(_) => 0,
            None => scroll.offset(),
        };
        self.current = self
            .matches
            .iter()
            .position(|(n, _)| *n >= top)
            .or((!self.matches.is_empty()).then_some(0));
        self.reveal(lines, scroll, wrap);
    }

    /// To the next match (`delta` 1) or the previous (-1), around the end,
    /// and into view. `false` without a match to step to.
    pub fn step(
        &mut self,
        delta: isize,
        lines: &[Line<'_>],
        scroll: &mut Scroll,
        wrap: Option<u16>,
    ) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        let at = self.current.unwrap_or(0);
        self.current = Some(tab_step(at, self.matches.len(), delta));
        self.reveal(lines, scroll, wrap);
        true
    }

    /// The current match in view: its line, and its columns unless wrapped.
    /// Done by [`find`](Self::find) and [`step`](Self::step); for a view
    /// that changed its wrapping or its width in between.
    pub fn reveal(&self, lines: &[Line<'_>], scroll: &mut Scroll, wrap: Option<u16>) {
        let Some((n, range)) = self.current() else {
            return;
        };
        let Some(shown) = lines.get(*n).map(shown) else {
            return;
        };
        let (Some(before), Some(found)) = (shown.get(..range.start), shown.get(range.clone()))
        else {
            // the lines are not the ones that were searched
            return;
        };
        let Some(cols) = wrap else {
            let (before, found) = (cols_of(before), cols_of(found));
            scroll.reveal(*n, before..before + found);
            return;
        };
        /*
        The rows of the lines above, and the match's row in its own. Lines
        wrap at words, greedily: the line up to the end of the word the
        match begins in breaks as the whole line does that far, and ends
        on the match's row. A word wider than the text is cut wherever the
        row ends, so there the line up to the match itself is taken.
        */
        let cols = cols.max(1);
        let above: usize = lines[..*n]
            .iter()
            .map(|l| usize::from(wrap_lines(borrowed(l), cols)))
            .sum();
        let word_start = before.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        let word_end = shown[range.start..]
            .find(char::is_whitespace)
            .map_or(shown.len(), |at| range.start + at);
        let upto = match cols_of(&shown[word_start..word_end]) <= usize::from(cols) {
            true => word_end,
            false => range.start + found.chars().next().map_or(0, char::len_utf8),
        };
        let rows = usize::from(wrap_lines(&shown[..upto], cols));
        scroll.reveal(above + rows.saturating_sub(1), 0..0);
    }

    /**
    `lines` for the view, the matches marked: every one in the
    [found style](Self::found_style), the current one in
    [its own](Self::current_style), each over the style the text has
    there. The spans are borrowed and only the lines with a match are cut
    up, so a frame of a large text copies none of it.
    */
    pub fn marked<'a>(&self, lines: &'a [Line<'_>]) -> Vec<Line<'a>> {
        let mut out: Vec<Line<'a>> = lines.iter().map(borrowed).collect();
        let current = self.current();
        let mut at = 0;
        while at < self.matches.len() {
            let n = self.matches[at].0;
            let end = at + self.matches[at..].iter().take_while(|(m, _)| *m == n).count();
            let of_line = &self.matches[at..end];
            at = end;
            let now = current.filter(|(m, _)| *m == n).map(|(_, range)| range);
            if let Some(line) = lines.get(n)
                && (self.found_style.is_some() || now.is_some())
            {
                out[n] = self.mark(line, of_line, now);
            }
        }
        out
    }

    /// `line` with the byte ranges of its matches marked, `now` (one of them)
    /// apart: each span cut where a range starts or ends.
    fn mark<'a>(
        &self,
        line: &'a Line<'_>,
        ranges: &[(usize, Range<usize>)],
        now: Option<&Range<usize>>,
    ) -> Line<'a> {
        let mut spans: Vec<Span<'a>> = Vec::with_capacity(line.spans.len() + 2 * ranges.len());
        let mut start = 0;
        for span in &line.spans {
            let text: &'a str = span.content.as_ref();
            let end = start + text.len();
            // the cuts within this span: its edges and every range edge between them
            let mut cuts = vec![start, end];
            cuts.extend(
                ranges
                    .iter()
                    .flat_map(|(_, r)| [r.start, r.end])
                    .filter(|&c| c > start && c < end && text.is_char_boundary(c - start)),
            );
            cuts.sort_unstable();
            cuts.dedup();
            for pair in cuts.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let within = ranges.iter().map(|(_, r)| r).find(|r| r.start <= a && b <= r.end);
                let style = match within {
                    Some(r) if now == Some(r) => span.style.patch(self.current_style),
                    Some(_) => self.found_style.map_or(span.style, |s| span.style.patch(s)),
                    None => span.style,
                };
                spans.push(Span::styled(&text[a - start..b - start], style));
            }
            start = end;
        }
        Line {
            spans,
            style: line.style,
            alignment: line.alignment,
        }
    }
}

/* ===== free functions ===================================================== */

/// The text of `line` as it is shown: its spans, one after another.
fn shown(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// `line` again, its spans borrowed and not copied.
fn borrowed<'a>(line: &'a Line<'_>) -> Line<'a> {
    let spans = line
        .spans
        .iter()
        .map(|s| Span::styled(s.content.as_ref(), s.style))
        .collect();
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

/// The display columns of `text`, as the lines' columns count.
fn cols_of(text: &str) -> usize {
    Line::raw(text).width()
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::style::Stylize;

    use crate::{TextView, buffer_rows, render_stateful_buffer};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn text(lines: &[&'static str]) -> Vec<Line<'static>> {
        lines.iter().map(|l| Line::raw(*l)).collect()
    }

    /// A search for `needle`, typed and kept.
    fn searched(needle: &str, lines: &[Line<'_>], scroll: &mut Scroll) -> TextSearch {
        let mut search = TextSearch::new();
        search.start();
        for c in needle.chars() {
            search.handle_key(press(KeyCode::Char(c)), lines, scroll, None);
        }
        assert_eq!(
            search.handle_key(press(KeyCode::Enter), lines, scroll, None),
            FilterOutcome::Done
        );
        search
    }

    /// A scroll that has drawn `lines` in a view `rows` by `cols`.
    fn drawn(lines: &[Line<'static>], cols: u16, rows: u16, wrap: bool) -> Scroll {
        let mut scroll = Scroll::new();
        let view = TextView::new(lines.to_vec()).wrap(wrap).sideways(!wrap);
        render_stateful_buffer(view, cols, rows, &mut scroll);
        scroll
    }

    #[test]
    fn found_in_every_line_as_shown() {
        let lines = text(&["one foo", "none", "foo and FOO", "food"]);
        let mut scroll = Scroll::new();
        let search = searched("foo", &lines, &mut scroll);
        assert_eq!(
            search.matches(),
            [(0, 4..7), (2, 0..3), (2, 8..11), (3, 0..3)]
        );
        assert_eq!((search.position(), search.is_capped()), (1, false));
        assert_eq!(search.current(), Some(&(0, 4..7)));
        assert!(search.is_active() && !search.is_editing());
        assert_eq!(search.text(), "foo");
    }

    #[test]
    fn a_capital_makes_it_exact() {
        let lines = text(&["foo Foo FOO"]);
        let mut scroll = Scroll::new();
        assert_eq!(searched("foo", &lines, &mut scroll).matches().len(), 3);
        assert_eq!(searched("Foo", &lines, &mut scroll).matches(), [(0, 4..7)]);
        // folding is ASCII's: the ranges stay the text's own bytes
        let lines = text(&["Ärger ärger"]);
        assert_eq!(searched("ärger", &lines, &mut scroll).matches(), [(0, 7..13)]);
        assert_eq!(searched("rger", &lines, &mut scroll).matches().len(), 2);
    }

    #[test]
    fn across_the_spans_of_a_line() {
        let lines = vec![Line::from(vec!["<?php ev".red(), "al(".blue(), "$x);".into()])];
        let mut scroll = Scroll::new();
        let search = searched("eval($", &lines, &mut scroll);
        assert_eq!(search.matches(), [(0, 6..12)]);
        let marked = search.marked(&lines);
        let now = Style::new().fg(Color::Black).bg(Color::Cyan);
        let parts: Vec<(&str, Style)> = marked[0]
            .spans
            .iter()
            .map(|s| (s.content.as_ref(), s.style))
            .collect();
        assert_eq!(
            parts,
            [
                ("<?php ", Style::new().red()),
                ("ev", Style::new().red().patch(now)),
                ("al(", Style::new().blue().patch(now)),
                ("$", now),
                ("x);", Style::new()),
            ]
        );
        assert_eq!(marked[0].to_string(), lines[0].to_string());
    }

    #[test]
    fn the_current_match_apart_from_the_others() {
        let lines = text(&["a b a", "none", "a"]);
        let mut scroll = Scroll::new();
        let mut search = searched("a", &lines, &mut scroll);
        let found = Style::new().fg(Color::Black).bg(Color::Yellow);
        let now = Style::new().fg(Color::Black).bg(Color::Cyan);
        let styles = |search: &TextSearch| -> Vec<Vec<Style>> {
            let marked = search.marked(&lines);
            marked
                .iter()
                .map(|l| l.spans.iter().map(|s| s.style).collect())
                .collect()
        };
        assert_eq!(
            styles(&search),
            [vec![now, Style::new(), found], vec![Style::new()], vec![found]]
        );
        assert!(search.step(1, &lines, &mut scroll, None));
        assert_eq!(
            styles(&search),
            [vec![found, Style::new(), now], vec![Style::new()], vec![found]]
        );
        // only the current one
        let search = search.found_style(None).current_style(Color::Red);
        assert_eq!(
            styles(&search),
            [
                vec![Style::new(), Style::new(), Style::new().fg(Color::Red)],
                vec![Style::new()],
                vec![Style::new()],
            ]
        );
    }

    #[test]
    fn steps_around_the_end() {
        let lines = text(&["x", "x", "x"]);
        let mut scroll = Scroll::new();
        let mut search = searched("x", &lines, &mut scroll);
        let mut seen = vec![search.position()];
        for delta in [1, 1, 1, -1, -1] {
            assert!(search.step(delta, &lines, &mut scroll, None));
            seen.push(search.position());
        }
        assert_eq!(seen, [1, 2, 3, 1, 3, 2]);

        let mut none = searched("y", &lines, &mut scroll);
        assert!(!none.step(1, &lines, &mut scroll, None));
        assert_eq!((none.position(), none.current()), (0, None));
    }

    #[test]
    fn stops_at_its_cap() {
        let lines = text(&["aaaa", "aaaa"]);
        let mut scroll = Scroll::new();
        let mut search = TextSearch::new().cap(5);
        search.start();
        search.handle_key(press(KeyCode::Char('a')), &lines, &mut scroll, None);
        assert_eq!(search.matches().len(), 5);
        assert!(search.is_capped());
        assert!(!searched("a", &lines, &mut scroll).is_capped());
    }

    #[test]
    fn cleared_by_esc_and_by_the_app() {
        let lines = text(&["foo", "foo"]);
        let mut scroll = Scroll::new();
        let mut search = searched("foo", &lines, &mut scroll);
        search.start();
        assert_eq!(
            search.handle_key(press(KeyCode::Esc), &lines, &mut scroll, None),
            FilterOutcome::Cleared
        );
        assert!(search.matches().is_empty() && !search.is_active());

        let mut search = searched("foo", &lines, &mut scroll);
        search.clear();
        assert!(search.matches().is_empty() && search.current().is_none());
        assert!(!search.bar().is_shown());
        // a key that is not the search's is the view's
        search.start();
        assert_eq!(
            search.handle_key(press(KeyCode::Down), &lines, &mut scroll, None),
            FilterOutcome::Ignored
        );
    }

    #[test]
    fn the_first_match_from_the_top_of_the_view_on() {
        let lines: Vec<Line<'static>> = (0..40)
            .map(|n| Line::raw(if n % 10 == 3 { "hit" } else { "-" }))
            .collect();
        let mut scroll = drawn(&lines, 20, 5, false);
        scroll.set_offset(20);
        let search = searched("hit", &lines, &mut scroll);
        // 3 and 13 are above the view: 23 it is
        assert_eq!(search.current(), Some(&(23, 0..3)));
        assert_eq!(search.position(), 3);
        // below the last match: around to the first
        let mut scroll = drawn(&lines, 20, 5, false);
        scroll.set_offset(35);
        let search = searched("hit", &lines, &mut scroll);
        assert_eq!(search.current(), Some(&(3, 0..3)));
    }

    #[test]
    fn a_match_out_of_view_is_brought_into_it() {
        let mut lines: Vec<Line<'static>> = (0..60).map(|n| Line::raw(format!("line {n}"))).collect();
        lines[40] = Line::raw(format!("{}needle", " ".repeat(100)));
        let mut scroll = drawn(&lines, 30, 6, false);
        let search = searched("needle", &lines, &mut scroll);
        assert_eq!(search.current(), Some(&(40, 100..106)));
        // drawn again, the match is on screen
        let view = TextView::new(search.marked(&lines)).sideways(true);
        let buf = render_stateful_buffer(view, 30, 6, &mut scroll);
        assert!(
            buffer_rows(&buf).iter().any(|row| row.contains("needle")),
            "{:?}",
            buffer_rows(&buf)
        );
        assert!((scroll.offset()..scroll.offset() + 6).contains(&40));
    }

    #[test]
    fn a_match_in_a_wrapped_line_by_its_row() {
        // 20 columns: the first line takes three rows, the match is on the second line's second
        let lines = text(&[
            "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj kkkk",
            "one two three four five needle six",
            "tail",
        ]);
        let cols = 20;
        let mut scroll = drawn(&lines, cols, 2, true);
        let mut search = TextSearch::new();
        search.start();
        for c in "needle".chars() {
            search.handle_key(press(KeyCode::Char(c)), &lines, &mut scroll, Some(cols));
        }
        let view = TextView::new(search.marked(&lines)).wrap(true);
        let buf = render_stateful_buffer(view, cols, 2, &mut scroll);
        assert!(
            buffer_rows(&buf).iter().any(|row| row.contains("needle")),
            "{:?}",
            buffer_rows(&buf)
        );
    }

    #[test]
    fn lines_that_are_not_the_ones_searched() {
        let lines = text(&["a long line with a needle in it"]);
        let mut scroll = Scroll::new();
        let mut search = searched("needle", &lines, &mut scroll);
        // the text changed under the search: nothing to mark, nothing to panic about
        let other = text(&["short"]);
        search.reveal(&other, &mut scroll, None);
        search.reveal(&[], &mut scroll, Some(10));
        assert_eq!(search.marked(&other)[0].to_string(), "short");
        assert!(search.marked(&[]).is_empty());
        // searched again, it is the new text's
        search.find(&other, &mut scroll, None);
        assert!(search.matches().is_empty());
    }

    #[test]
    fn the_bar_counts_where_it_is() {
        let lines = text(&["x x x"]);
        let mut scroll = Scroll::new();
        let mut search = searched("x", &lines, &mut scroll);
        search.step(1, &lines, &mut scroll, None);
        let bar = search.bar();
        assert!(bar.is_shown());
        let line = bar.line(40).to_string();
        assert!(line.starts_with("search") && line.contains('x') && line.trim_end().ends_with("2 of 3"), "{line:?}");
    }
}
