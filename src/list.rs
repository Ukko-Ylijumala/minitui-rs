// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Selection and scrolling for a list or table: [`ListCursor`]. Every app had
its own, each with part of it right:
- zfs-browser kept the scroll offset across frames (a fresh `ListState` per
  frame, as nft-browser still makes, pins the highlight to the bottom row
  once past the first page) and built only the visible rows;
- mping-rs predicted the offset ratatui would settle on before rendering,
  so the row data it gathered was for the rows actually shown;
- squawk and zfs-browser paged by the rows of the last draw, where
  nft-browser guessed from the terminal height minus a hand-counted chrome;
- squawk and mping-rs kept the selection on the same item across a refresh
  or a re-sort; mping-rs and wiremeup clamped it after a delete;
- nft-browser skipped its separator rows; wiremeup wrapped around.
*/

use std::iter;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::widgets::{ListState, TableState};

/* ===== consts and statics ================================================= */

/// The page for PgUp / PgDn before the first draw tells the real one.
const FALLBACK_PAGE: usize = 10;

/* ===== types ============================================================== */

/**
The selected row of a list (or a table) and its scroll offset, kept across
frames, plus the rows the last frame showed: the page for PgUp / PgDn. It
holds no items, so it fits whatever the app keeps them in; every call that
needs the length takes it.

Per frame, [`ListCursor::window`] clamps the selection to the list, scrolls
just enough to keep it in view (the last page always full) and returns the
rows to show, before anything is rendered: the app formats only those rows,
or renders the whole list with [`ListCursor::list_state`] /
[`ListCursor::table_state`]. Keys go through [`ListCursor::nav`].

```no_run
# use ratatui::Frame;
# use ratatui::layout::Rect;
# use ratatui::widgets::{Block, List, ListState};
# use crossterm::event::KeyEvent;
use minitui::{ListCursor, NavOutcome};

# fn demo(frame: &mut Frame, area: Rect, key: KeyEvent, names: &[String], cursor: &mut ListCursor) {
// keys: ↑ ↓ Home End PgUp PgDn; Moved when the selection changed
if cursor.nav(names.len()).key(key) == NavOutcome::Moved { /* fetch the detail */ }

// a frame: only the visible rows are formatted
let block = Block::bordered();
let rows = cursor.window(names.len(), block.inner(area).height.into());
let list = List::new(names[rows].iter().map(String::as_str)).block(block);
let mut state = ListState::default().with_selected(cursor.selected_in_window());
frame.render_stateful_widget(list, area, &mut state);
# }
```
*/
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListCursor {
    selected: Option<usize>,
    /// The first row shown.
    offset: usize,
    /// Rows shown on the last [`ListCursor::window`]; 0 before the first.
    rows: usize,
}

/**
A navigation over a [`ListCursor`] for a list of `len` rows, from
[`ListCursor::nav`]: optionally wrapping around, optionally skipping rows
that can't be selected, then one move.
*/
#[must_use]
pub struct Nav<'a, F = fn(usize) -> bool> {
    cursor: &'a mut ListCursor,
    len: usize,
    wrap: bool,
    selectable: F,
}

/// What a navigation did to a [`ListCursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavOutcome {
    /// The selection changed (an app fetching the selected item's detail
    /// fetches it now).
    Moved,
    /// A navigation key that left the selection as it was: already at the edge.
    Handled,
    /// Not a navigation key: another key, a modifier chord, a key release.
    Ignored,
}

/* ===== ListCursor ========================================================= */

impl ListCursor {
    /// Nothing selected yet, scrolled to the top.
    pub fn new() -> Self {
        Self::default()
    }

    /// A cursor on row `index`.
    pub fn with_selected(index: Option<usize>) -> Self {
        Self {
            selected: index,
            ..Self::default()
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Select row `index` (or nothing); [`ListCursor::window`] scrolls to it.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
    }

    /// The first row shown on the last [`ListCursor::window`].
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Rows shown on the last [`ListCursor::window`]: the page; `None` before the first.
    pub fn page(&self) -> Option<usize> {
        (self.rows > 0).then_some(self.rows)
    }

    /// Back to the top, nothing selected: for a different list in the same place.
    pub fn reset(&mut self) {
        *self = Self {
            rows: self.rows,
            ..Self::default()
        };
    }

    /// Keep the selection in a list of `len` rows: after a delete, the last row.
    pub fn clamp(&mut self, len: usize) {
        self.selected = match len {
            0 => None,
            _ => self.selected.map(|i| i.min(len - 1)),
        };
    }

    /**
    After the list changed under the selection (a refresh, a re-sort):
    select `found`, where the selected item is now, or clamp when it is
    gone. Whether it was found: an app about to act on the selection may
    want to refuse when it wasn't, since the highlight now is on another
    item.

    ```no_run
    # use minitui::ListCursor;
    # struct Agent { id: String }
    # fn refresh(cursor: &mut ListCursor, agents: &mut Vec<Agent>, fresh: Vec<Agent>) {
    let id = cursor.selected().map(|i| agents[i].id.clone());
    *agents = fresh;
    let found = id.and_then(|id| agents.iter().position(|a| a.id == id));
    cursor.reselect(found, agents.len());
    # }
    ```
    */
    pub fn reselect(&mut self, found: Option<usize>, len: usize) -> bool {
        match found {
            Some(i) if i < len => {
                self.selected = Some(i);
                true
            }
            _ => {
                self.clamp(len);
                false
            }
        }
    }

    /// Navigate a list of `len` rows; see [`Nav`].
    pub fn nav(&mut self, len: usize) -> Nav<'_> {
        Nav {
            cursor: self,
            len,
            wrap: false,
            selectable: any_row,
        }
    }

    /**
    The rows of a list of `len` to show in `rows` lines, for this frame:
    the selection clamped to the list, the offset moved just enough to
    show it, never past where the last page is full. Remembers `rows` as
    the page. Call it before formatting the rows, every frame.
    */
    pub fn window(&mut self, len: usize, rows: usize) -> Range<usize> {
        self.rows = rows;
        self.clamp(len);
        let rows = rows.max(1);
        if let Some(sel) = self.selected {
            if sel < self.offset {
                self.offset = sel;
            } else if sel >= self.offset + rows {
                self.offset = sel + 1 - rows;
            }
        }
        self.offset = self.offset.min(len.saturating_sub(rows));
        self.offset..(self.offset + rows).min(len)
    }

    /// The selection's index within the last [`ListCursor::window`], for
    /// rendering only the rows in it.
    pub fn selected_in_window(&self) -> Option<usize> {
        self.selected.map(|i| i.saturating_sub(self.offset))
    }

    /// A `ListState` for rendering the whole list, at this offset and selection.
    pub fn list_state(&self) -> ListState {
        ListState::default()
            .with_offset(self.offset)
            .with_selected(self.selected)
    }

    /// A `TableState` for rendering the whole table, at this offset and selection.
    pub fn table_state(&self) -> TableState {
        TableState::default()
            .with_offset(self.offset)
            .with_selected(self.selected)
    }

    /// One PgUp / PgDn: a page less a row, so the edge row stays in sight.
    fn page_step(&self) -> isize {
        let page = self.page().unwrap_or(FALLBACK_PAGE);
        isize::try_from(page.saturating_sub(1).max(1)).unwrap_or(isize::MAX)
    }
}

/* ===== Nav ================================================================ */

impl<'a, F: Fn(usize) -> bool> Nav<'a, F> {
    /// ↑ on the first row goes to the last and ↓ on the last to the first
    /// (single steps only: Home, End and paging stop at the ends).
    pub fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    /**
    Only rows for which `selectable` is true can be selected (separators,
    headings can't): a move lands on the next selectable row its way, or
    the nearest one back when there is none. A list with none at all
    leaves the selection alone.
    */
    pub fn selectable<G: Fn(usize) -> bool>(self, selectable: G) -> Nav<'a, G> {
        Nav {
            cursor: self.cursor,
            len: self.len,
            wrap: self.wrap,
            selectable,
        }
    }

    /// Move `delta` rows (down if positive); from no selection, ↓ selects the
    /// first row and ↑ the last.
    pub fn by(self, delta: isize) -> NavOutcome {
        let target = match self.cursor.selected {
            None if delta >= 0 => 0,
            None => self.last_index(),
            Some(i) => isize_of(i).saturating_add(delta),
        };
        let wrap = self.wrap && delta.unsigned_abs() == 1;
        let dir = if delta < 0 { -1 } else { 1 };
        self.go(target, dir, wrap)
    }

    pub fn first(self) -> NavOutcome {
        self.go(0, 1, false)
    }

    pub fn last(self) -> NavOutcome {
        let last = self.last_index();
        self.go(last, -1, false)
    }

    /// Up by a page less a row (the page of the last draw).
    pub fn page_up(self) -> NavOutcome {
        let step = self.cursor.page_step();
        self.by_clamped(-step)
    }

    /// Down by a page less a row (the page of the last draw).
    pub fn page_down(self) -> NavOutcome {
        let step = self.cursor.page_step();
        self.by_clamped(step)
    }

    /**
    One key: ↑ ↓ Home End PgUp PgDn, without modifiers (a Shift / Ctrl / Alt
    chord is the app's, e.g. Shift-↑ to sort). Anything else is
    [`NavOutcome::Ignored`]; a held-down key repeats.
    */
    pub fn key(self, key: KeyEvent) -> NavOutcome {
        if key.kind == KeyEventKind::Release || !key.modifiers.is_empty() {
            return NavOutcome::Ignored;
        }
        match key.code {
            KeyCode::Up => self.by(-1),
            KeyCode::Down => self.by(1),
            KeyCode::Home => self.first(),
            KeyCode::End => self.last(),
            KeyCode::PageUp => self.page_up(),
            KeyCode::PageDown => self.page_down(),
            _ => NavOutcome::Ignored,
        }
    }

    fn by_clamped(self, delta: isize) -> NavOutcome {
        Self { wrap: false, ..self }.by(delta)
    }

    /**
    Select `target`, or the nearest selectable row from it going `dir`
    (+1 / -1): wrapping around, or stopping at the edge and looking back the
    other way. Outside the list, the target is clamped (or wrapped) first.
    */
    fn go(self, target: isize, dir: isize, wrap: bool) -> NavOutcome {
        let before = self.cursor.selected;
        if self.len == 0 {
            self.cursor.selected = None;
            return outcome(before, None);
        }
        let last = self.last_index();
        let start = match wrap {
            true => target.rem_euclid(last + 1),
            false => target.clamp(0, last),
        };
        let found = match wrap {
            true => (0..=last)
                .map(|k| (start + k * dir).rem_euclid(last + 1))
                .find(|&i| (self.selectable)(index_of(i))),
            false => {
                let ahead = steps(start, dir, last).find(|&i| (self.selectable)(index_of(i)));
                ahead.or_else(|| steps(start, -dir, last).find(|&i| (self.selectable)(index_of(i))))
            }
        };
        if let Some(i) = found {
            self.cursor.selected = Some(index_of(i));
        }
        outcome(before, self.cursor.selected)
    }

    fn last_index(&self) -> isize {
        isize_of(self.len.saturating_sub(1))
    }
}

/* ===== helpers ============================================================ */

fn any_row(_: usize) -> bool {
    true
}

fn outcome(before: Option<usize>, after: Option<usize>) -> NavOutcome {
    match before == after {
        true => NavOutcome::Handled,
        false => NavOutcome::Moved,
    }
}

/// `start`, then on by `dir` up to the edge of `0..=last`.
fn steps(start: isize, dir: isize, last: isize) -> impl Iterator<Item = isize> {
    let mut at = Some(start);
    iter::from_fn(move || {
        let here = at?;
        let next = here + dir;
        at = (0..=last).contains(&next).then_some(next);
        Some(here)
    })
}

fn isize_of(i: usize) -> isize {
    isize::try_from(i).unwrap_or(isize::MAX)
}

/// A row index already within `0..len`.
fn index_of(i: isize) -> usize {
    usize::try_from(i).unwrap_or(0)
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn at(i: usize) -> ListCursor {
        ListCursor::with_selected(Some(i))
    }

    #[test]
    fn moves_stop_at_the_ends() {
        let mut c = at(0);
        assert_eq!(c.nav(3).by(-1), NavOutcome::Handled);
        assert_eq!(c.nav(3).by(1), NavOutcome::Moved);
        assert_eq!(c.nav(3).by(5), NavOutcome::Moved);
        assert_eq!(c.selected(), Some(2));
        assert_eq!(c.nav(3).by(1), NavOutcome::Handled);
        assert_eq!(c.nav(3).first(), NavOutcome::Moved);
        assert_eq!(c.selected(), Some(0));
        c.nav(3).last();
        assert_eq!(c.selected(), Some(2));
    }

    #[test]
    fn from_no_selection_down_is_first_and_up_is_last() {
        let mut c = ListCursor::new();
        assert_eq!(c.nav(4).by(1), NavOutcome::Moved);
        assert_eq!(c.selected(), Some(0));
        let mut c = ListCursor::new();
        c.nav(4).by(-1);
        assert_eq!(c.selected(), Some(3));
        let mut c = ListCursor::new();
        assert_eq!(c.nav(0).by(1), NavOutcome::Handled, "nothing to select");
        assert_eq!(c.selected(), None);
    }

    #[test]
    fn an_empty_list_drops_the_selection() {
        let mut c = at(2);
        assert_eq!(c.nav(0).by(1), NavOutcome::Moved);
        assert_eq!(c.selected(), None);
    }

    #[test]
    fn wrapping_single_steps_only() {
        // wiremeup's panes
        let mut c = at(2);
        assert_eq!(c.nav(3).wrap().by(1), NavOutcome::Moved);
        assert_eq!(c.selected(), Some(0));
        c.nav(3).wrap().by(-1);
        assert_eq!(c.selected(), Some(2));
        c.nav(3).wrap().first();
        c.nav(3).wrap().page_up();
        assert_eq!(c.selected(), Some(0), "paging doesn't wrap");
    }

    #[test]
    fn unselectable_rows_are_skipped() {
        // nft-browser's separators: rows 0, 2, 3 and 6 can't be selected
        let sep = |i: usize| ![0, 2, 3, 6].contains(&i);
        let mut c = at(1);
        c.nav(7).selectable(sep).by(1);
        assert_eq!(c.selected(), Some(4));
        c.nav(7).selectable(sep).by(-1);
        assert_eq!(c.selected(), Some(1));
        // off the top: row 0 can't be had, the nearest the other way can
        c.nav(7).selectable(sep).first();
        assert_eq!(c.selected(), Some(1));
        c.nav(7).selectable(sep).last();
        assert_eq!(c.selected(), Some(5));
        assert_eq!(c.nav(7).selectable(sep).by(1), NavOutcome::Handled);
        // wrapping skips them too
        c.nav(7).selectable(sep).wrap().by(1);
        assert_eq!(c.selected(), Some(1));
        // none selectable at all: left alone
        assert_eq!(c.nav(7).selectable(|_| false).by(1), NavOutcome::Handled);
        assert_eq!(c.selected(), Some(1));
    }

    #[test]
    fn paging_follows_the_last_draw() {
        let mut c = at(0);
        c.nav(100).page_down();
        assert_eq!(c.selected(), Some(FALLBACK_PAGE - 1), "before any draw");
        c.window(100, 20);
        c.select(Some(0));
        c.nav(100).page_down();
        assert_eq!(c.selected(), Some(19), "a page less a row");
        c.nav(100).page_up();
        assert_eq!(c.selected(), Some(0));
        c.window(100, 1);
        c.nav(100).page_down();
        assert_eq!(c.selected(), Some(1), "at least a row");
    }

    #[test]
    fn keys_without_modifiers_only() {
        let mut c = at(1);
        assert_eq!(c.nav(5).key(key(KeyCode::Down)), NavOutcome::Moved);
        assert_eq!(c.nav(5).key(key(KeyCode::End)), NavOutcome::Moved);
        assert_eq!(c.selected(), Some(4));
        assert_eq!(c.nav(5).key(key(KeyCode::Home)), NavOutcome::Moved);
        assert_eq!(c.nav(5).key(key(KeyCode::Char('j'))), NavOutcome::Ignored);
        let shift_up = KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(c.nav(5).key(shift_up), NavOutcome::Ignored, "mping's sort key");
        let mut release = key(KeyCode::Down);
        release.kind = KeyEventKind::Release;
        assert_eq!(c.nav(5).key(release), NavOutcome::Ignored);
        let mut repeat = key(KeyCode::Down);
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(c.nav(5).key(repeat), NavOutcome::Moved, "a held-down arrow scrolls");
    }

    #[test]
    fn the_window_keeps_its_offset_across_frames() {
        // nft-browser's bug: a fresh state per frame pinned the highlight to the bottom
        let mut c = at(0);
        assert_eq!(c.window(100, 10), 0..10);
        c.nav(100).by(15);
        assert_eq!(c.window(100, 10), 6..16, "scrolled just enough");
        c.nav(100).by(-1);
        assert_eq!(c.window(100, 10), 6..16, "moving up moves the highlight, not the list");
        assert_eq!(c.selected_in_window(), Some(8));
        c.nav(100).by(-10);
        assert_eq!(c.window(100, 10), 4..14);
    }

    #[test]
    fn the_last_page_stays_full() {
        let mut c = at(99);
        assert_eq!(c.window(100, 10), 90..100);
        // the list shrank under it (mping's removed targets)
        assert_eq!(c.window(20, 10), 10..20);
        assert_eq!(c.selected(), Some(19));
        // the terminal grew
        assert_eq!(c.window(20, 30), 0..20);
        assert_eq!(c.window(0, 10), 0..0);
        assert_eq!(c.selected(), None);
        assert_eq!(ListCursor::new().window(5, 0), 0..1, "0 rows still shows one");
    }

    #[test]
    fn states_carry_offset_and_selection() {
        let mut c = at(30);
        c.window(100, 10);
        let list = c.list_state();
        assert_eq!((list.offset(), list.selected()), (21, Some(30)));
        let table = c.table_state();
        assert_eq!((table.offset(), table.selected()), (21, Some(30)));
    }

    #[test]
    fn clamp_after_a_delete() {
        let mut c = at(4);
        c.clamp(4);
        assert_eq!(c.selected(), Some(3), "the last row went: the new last");
        c.clamp(10);
        assert_eq!(c.selected(), Some(3));
        c.clamp(0);
        assert_eq!(c.selected(), None);
        let mut none = ListCursor::new();
        none.clamp(5);
        assert_eq!(none.selected(), None, "clamping doesn't select");
    }

    #[test]
    fn reselect_follows_the_item_or_clamps() {
        let mut c = at(1);
        let ids = ["a", "b", "c"];
        let id = c.selected().map(|i| ids[i]);
        let resorted = ["c", "a", "b"];
        let found = id.and_then(|id| resorted.iter().position(|&r| r == id));
        assert!(c.reselect(found, resorted.len()));
        assert_eq!(c.selected(), Some(2));
        // gone after a refresh: clamped, and the app is told
        assert!(!c.reselect(None, 2));
        assert_eq!(c.selected(), Some(1));
        assert!(!c.reselect(Some(9), 2), "a stale position is no find");
    }

    #[test]
    fn reset_keeps_the_page() {
        let mut c = at(50);
        c.window(100, 20);
        c.reset();
        assert_eq!((c.selected(), c.offset(), c.page()), (None, 0, Some(20)));
    }
}
