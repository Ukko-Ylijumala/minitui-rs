// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A table that states its own width: [`GridTable`] is as wide as its columns
have ever needed to be, and the layout around it takes what that leaves;
a row and a column are selected in it ([`GridState`]). Where
[`ColumnTable`](crate::ColumnTable) is given a width and fits its columns
into it, this one is asked for its width first. From mping-rs's target
table:
- its columns were as wide as their widest cell had ever been and never
  narrower again, so the table didn't twitch as the numbers changed; the
  table's width came first, and the panes beside it took the rest;
- it measured the cells in bytes, which overstates any text that isn't
  ASCII;
- a row and a column were selected: the row for the panes beside the
  table, the column for the sort keys to sort by;
- the rows were formatted before the draw and outside the locks the draw
  holds, only those in view, so a list of thousands cost a screenful;
- the scrollbar was drawn over the right border, where it costs no column.

What is the same as in a [`ColumnTable`](crate::ColumnTable) is the same
code: the [`Column`]s, the [`TableRow`]s, the [`Sort`] marker, and a table
too wide for its area leaving out columns by priority.
*/

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget};

use crate::table::{HEADER_ROWS, Lanes, content_widths, needs, total_width, with_notes};
use crate::{Column, ListCursor, NavOutcome, Sort, TableRow, Width, range_label};

/* ===== consts and statics ================================================= */

const DEFAULT_GAP: u16 = 1;
/// Under the selected column: a grey that a reversed row still shows on.
const COLUMN_BG: Color = Color::Indexed(240);
/// An area larger than any block's borders and padding, to measure them in.
const PROBE: Rect = Rect::new(0, 0, u16::MAX, u16::MAX);

/* ===== types ============================================================== */

/**
What a [`GridTable`] keeps across frames: the selected row and the scroll
offset (a [`ListCursor`], whose moves and keys are the row's), the
selected column, and how wide each column has been.

```no_run
# use crossterm::event::KeyEvent;
use minitui::{GridState, NavOutcome};

# fn demo(key: KeyEvent, state: &mut GridState, hosts: usize, columns: usize) {
// ↑ ↓ Home End PgUp PgDn move the row, ← → the column
if state.key(key, hosts, columns) == NavOutcome::Moved { /* another row: its detail */ }
// or each on its own
state.cursor.nav(hosts).by(1);
state.next_column(columns);
# }
```
*/
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridState {
    /// The selected row and the scroll offset.
    pub cursor: ListCursor,
    column: Option<usize>,
    /// The widest each column has been; empty before the first measure.
    widths: Vec<u16>,
}

/**
The table: `columns` and the `rows` already formatted, a header naming the
columns (the [`Sort`] column marked ▲ / ▼), the [`GridState`]'s row and
column highlighted, a scrollbar over the block's right border when the
rows don't all show. A [`Width::Fit`] column ([`Width::CONTENT`]) is as
wide as its header and any row it has been given, and stays that wide when
the row is gone ([`GridState::reset_widths`] forgets).

[`measure`](GridTable::measure) says how wide the table wants to be, before
the layout is made:

```no_run
# use ratatui::Frame;
# use ratatui::layout::{Constraint, Layout};
# use ratatui::widgets::Block;
use minitui::{Column, GridState, GridTable, TableRow, Width};

# fn draw(frame: &mut Frame, hosts: &[(String, u64)], state: &mut GridState) {
let columns = [
    Column::new("address", Width::CONTENT),
    Column::new("sent", Width::CONTENT).right(),
];
let rows = hosts.iter().map(|(addr, sent)| TableRow::new([addr.clone(), sent.to_string()]));
let table = GridTable::new(&columns, rows).gap(2).block(Block::bordered());
// the table first, the detail pane what is left
let want = Constraint::Max(table.measure(state));
let [left, _detail] = Layout::horizontal([want, Constraint::Fill(1)]).areas(frame.area());
frame.render_stateful_widget(table, left, state);
# }
```

Given all the rows, it shows those the state's cursor has in view. A long
list is better formatted in view only: ask [`GridState::window`] which
rows those are, format them, and say how long the list is with
[`windowed`](GridTable::windowed).
*/
pub struct GridTable<'a> {
    columns: &'a [Column<'a>],
    rows: Vec<TableRow<'a>>,
    /// The whole list's length when `rows` are only the window's.
    len: Option<usize>,
    sort: Option<Sort>,
    block: Option<Block<'a>>,
    indicator: Option<&'a str>,
    gap: u16,
    header_style: Style,
    highlight_style: Style,
    column_style: Style,
    note_style: Style,
    scrollbar: Option<Style>,
}

/* ===== GridState ========================================================== */

impl GridState {
    /// Nothing selected, scrolled to the top, no widths remembered.
    pub fn new() -> Self {
        Self::default()
    }

    /// The selected column.
    pub fn column(&self) -> Option<usize> {
        self.column
    }

    /// Select column `index` (or none); the table clamps it to its columns.
    pub fn select_column(&mut self, index: Option<usize>) {
        self.column = index;
    }

    /// The column to the right of `columns`, stopping at the last; the first
    /// when none is selected.
    pub fn next_column(&mut self, columns: usize) {
        let last: usize = columns.saturating_sub(1);
        self.column = (columns > 0).then(|| self.column.map_or(0, |c| (c + 1).min(last)));
    }

    /// The column to the left of `columns`, stopping at the first; the last
    /// when none is selected.
    pub fn previous_column(&mut self, columns: usize) {
        let last: usize = columns.saturating_sub(1);
        self.column = (columns > 0).then(|| self.column.map_or(last, |c| c.saturating_sub(1).min(last)));
    }

    /**
    One key, for a table of `len` rows and `columns` columns: ← → move the
    column, the rest is [`Nav::key`](crate::Nav::key) on the cursor (↑ ↓
    Home End PgUp PgDn). Without modifiers only: a chord is the app's.
    [`NavOutcome::Moved`] says the row changed; a column move is
    [`NavOutcome::Handled`].
    */
    pub fn key(&mut self, key: KeyEvent, len: usize, columns: usize) -> NavOutcome {
        if key.kind == KeyEventKind::Release || !key.modifiers.is_empty() {
            return NavOutcome::Ignored;
        }
        match key.code {
            KeyCode::Left => self.previous_column(columns),
            KeyCode::Right => self.next_column(columns),
            _ => return self.cursor.nav(len).key(key),
        }
        NavOutcome::Handled
    }

    /// No row and no column selected, scrolled to the top. The widths stay.
    pub fn deselect(&mut self) {
        self.cursor.reset();
        self.column = None;
    }

    /**
    The rows of a list of `len` to format this frame, for a table whose
    area inside its block is `height` lines (the header is one of them):
    [`ListCursor::window`] on the rows left. For
    [`GridTable::windowed`]; call it before formatting, every frame.
    */
    pub fn window(&mut self, len: usize, height: u16) -> Range<usize> {
        self.cursor.window(len, height.saturating_sub(HEADER_ROWS).into())
    }

    /// The widest each column has been, in the columns' order; empty before
    /// the first [`GridTable::measure`] or draw.
    pub fn widths(&self) -> &[u16] {
        &self.widths
    }

    /// Forget how wide the columns have been: the next rows decide anew.
    pub fn reset_widths(&mut self) {
        self.widths.clear();
    }

    /**
    Each column's width now: a [`Width::Fit`] one grown to `rows` and
    never narrower than it has been, the others as their [`Width`] says.
    */
    fn grow(&mut self, columns: &[Column], rows: &[TableRow]) -> &[u16] {
        if self.widths.len() != columns.len() {
            self.widths = vec![0; columns.len()];
        }
        let need: Vec<u16> = needs(columns, &content_widths(rows, columns.len()));
        for ((seen, need), column) in self.widths.iter_mut().zip(need).zip(columns) {
            *seen = match column.width {
                Width::Fit { min, max } => need.max(*seen).clamp(min, max.max(min)),
                Width::Fixed(_) | Width::Fill { .. } => need,
            };
        }
        &self.widths
    }
}

/* ===== GridTable ========================================================== */

impl<'a> GridTable<'a> {
    /// `rows` of `columns`: all of them, or with [`windowed`](GridTable::windowed)
    /// those in view.
    pub fn new(columns: &'a [Column<'a>], rows: impl IntoIterator<Item = TableRow<'a>>) -> Self {
        Self {
            columns,
            rows: rows.into_iter().collect(),
            len: None,
            sort: None,
            block: None,
            indicator: None,
            gap: DEFAULT_GAP,
            header_style: Style::new().add_modifier(Modifier::BOLD),
            highlight_style: Style::new().add_modifier(Modifier::REVERSED),
            column_style: Style::new().bg(COLUMN_BG),
            note_style: Style::new().add_modifier(Modifier::DIM),
            scrollbar: Some(Style::new()),
        }
    }

    /**
    The rows are only those [`GridState::window`] returned this frame, of
    a list of `len`: the table draws them as they are and leaves the
    cursor alone.
    */
    pub fn windowed(mut self, len: usize) -> Self {
        self.len = Some(len);
        self
    }

    /// The column the rows are sorted by, marked in the header.
    pub fn sort(mut self, sort: Option<Sort>) -> Self {
        self.sort = sort;
        self
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Say `what a–b of N` in the bottom border when the rows don't all
    /// show (`rows`); `None`, as it is, for no count.
    pub fn indicator(mut self, what: impl Into<Option<&'a str>>) -> Self {
        self.indicator = what.into();
        self
    }

    /// Cells between two columns (1).
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    pub fn header_style(mut self, style: impl Into<Style>) -> Self {
        self.header_style = style.into();
        self
    }

    /// Patched over the selected row (reversed).
    pub fn highlight_style(mut self, style: impl Into<Style>) -> Self {
        self.highlight_style = style.into();
        self
    }

    /// Patched over the selected column's rows, the selected row's cell too
    /// (a grey background).
    pub fn column_style(mut self, style: impl Into<Style>) -> Self {
        self.column_style = style.into();
        self
    }

    /// The bottom border's notes (dim).
    pub fn note_style(mut self, style: impl Into<Style>) -> Self {
        self.note_style = style.into();
        self
    }

    /**
    The scrollbar's style, or `None` for no scrollbar. It is drawn over
    the block's right border when the rows don't all show, at the selected
    row's place in the list; a table without a block (or one with nothing
    right of its rows) has none.
    */
    pub fn scrollbar(mut self, style: impl Into<Option<Style>>) -> Self {
        self.scrollbar = style.into();
        self
    }

    /**
    How wide the table wants to be, its block included: every column at
    its width, the gaps between. Grows the state's widths to these rows,
    as the draw does, so the answer holds for the frame about to be drawn.
    */
    pub fn measure(&self, state: &mut GridState) -> u16 {
        let cells: u32 = total_width(state.grow(self.columns, &self.rows).iter().copied(), self.gap);
        let chrome: u16 = self.block.as_ref().map_or(0, |b| PROBE.width - b.inner(PROBE).width);
        u16::try_from(cells + u32::from(chrome)).unwrap_or(u16::MAX)
    }
}

impl StatefulWidget for GridTable<'_> {
    type State = GridState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut GridState) {
        let inner = self.block.as_ref().map_or(area, |b| b.inner(area));
        let body = inner.height.saturating_sub(HEADER_ROWS);
        let need: Vec<u16> = state.grow(self.columns, &self.rows).to_vec();
        let lanes = Lanes::new(self.columns, &need, inner, self.gap);

        // the rows in view, and where they are in the list
        let (len, shown): (usize, Range<usize>) = match self.len {
            Some(len) => {
                let first = state.cursor.offset();
                let last = first + self.rows.len().min(body.into());
                (len.max(first + self.rows.len()), first..last)
            }
            None => (self.rows.len(), state.cursor.window(self.rows.len(), body.into())),
        };
        let skip: usize = match self.len {
            Some(_) => 0,
            None => shown.start,
        };
        let selected: Option<usize> = state
            .cursor
            .selected()
            .and_then(|i| i.checked_sub(shown.start))
            .filter(|k| *k < shown.len());
        state.column = match self.columns.len() {
            0 => None,
            n => state.column.map(|c| c.min(n - 1)),
        };

        if let Some(block) = self.block {
            let label = self
                .indicator
                .filter(|_| len > usize::from(body))
                .map(|what| range_label(what, shown.clone(), len));
            with_notes(block, label, lanes.hidden(), self.note_style).render(area, buf);
        }
        if inner.is_empty() {
            return;
        }
        lanes.header(buf, self.sort, self.header_style);
        let in_view = self.rows.into_iter().skip(skip).take(shown.len());
        lanes.rows(buf, in_view, selected, self.highlight_style);

        // the selected column, over its rows and the lines under them
        if let Some((x, width)) = state.column.and_then(|c| lanes.span(c)) {
            let column = Rect { x, width, y: inner.y + HEADER_ROWS, height: body };
            buf.set_style(column, self.column_style);
        }

        // over the right border: nothing is drawn there otherwise
        if let Some(style) = self.scrollbar
            && len > usize::from(body)
            && inner.right() < area.right()
        {
            let at: usize = state.cursor.selected().unwrap_or(shown.start);
            Scrollbar::new(ScrollbarOrientation::VerticalRight).style(style).render(
                area,
                buf,
                &mut ScrollbarState::new(len).position(at),
            );
        }
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::KeyModifiers;
    use ratatui::text::Line;

    use crate::{buffer_rows, render_stateful_buffer};

    type Host = (&'static str, u64, &'static str);

    static HOSTS: &[Host] = &[("10.0.0.1", 12, "ok"), ("2001:db8::1", 3, "timeout")];
    /// Narrower in every column than [`HOSTS`].
    static SHORT: &[Host] = &[("10.0.0.2", 7, "ok")];

    /// mping's columns, three of them: none sortable, the address kept longest.
    fn columns() -> Vec<Column<'static>> {
        vec![
            Column::new("address", Width::CONTENT).sortable(false).priority(9),
            Column::new("sent", Width::CONTENT).sortable(false).right(),
            Column::new("status", Width::CONTENT).sortable(false),
        ]
    }

    fn rows(hosts: &[Host]) -> Vec<TableRow<'static>> {
        hosts
            .iter()
            .map(|&(addr, sent, status)| TableRow::new([addr.to_string(), sent.to_string(), status.to_string()]))
            .collect()
    }

    /// Ten hosts, `10.0.0.0` to `10.0.0.9`.
    fn many() -> Vec<TableRow<'static>> {
        (0..10).map(|i| TableRow::new([format!("10.0.0.{i}"), i.to_string(), "ok".to_string()])).collect()
    }

    fn table<'a>(columns: &'a [Column<'a>], rows: Vec<TableRow<'a>>) -> GridTable<'a> {
        GridTable::new(columns, rows).gap(2).block(Block::bordered())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_table_says_how_wide_it_is() {
        let columns = columns();
        let mut state = GridState::new();
        let grid = table(&columns, rows(HOSTS));
        // 11 + 4 + 7 cells, two gaps of 2, the borders
        assert_eq!(grid.measure(&mut state), 28);
        assert_eq!(state.widths(), [11, 4, 7]);
        let buf = render_stateful_buffer(grid, 28, 5, &mut state);
        assert_eq!(
            buffer_rows(&buf),
            [
                "┌──────────────────────────┐",
                "│address      sent  status │",
                "│10.0.0.1       12  ok     │",
                "│2001:db8::1     3  timeout│",
                "└──────────────────────────┘",
            ]
        );
        // without a block it is the cells alone
        assert_eq!(GridTable::new(&columns, rows(HOSTS)).measure(&mut GridState::new()), 11 + 1 + 4 + 1 + 7);
    }

    #[test]
    fn columns_grow_and_never_shrink() {
        let columns = columns();
        let mut state = GridState::new();
        assert_eq!(table(&columns, rows(SHORT)).measure(&mut state), 8 + 4 + 6 + 4 + 2, "the headers and one address");
        assert_eq!(table(&columns, rows(HOSTS)).measure(&mut state), 28);
        // the wide rows are gone: the columns stay
        let grid = table(&columns, rows(SHORT));
        assert_eq!(grid.measure(&mut state), 28);
        let buf = render_stateful_buffer(grid, 28, 4, &mut state);
        assert_eq!(buffer_rows(&buf)[2], "│10.0.0.2        7  ok     │");
        assert_eq!(state.widths(), [11, 4, 7], "a draw measures as measure does");

        state.reset_widths();
        assert!(state.widths().is_empty());
        assert_eq!(table(&columns, rows(SHORT)).measure(&mut state), 24);
        // a column's bounds hold over what it has been
        let capped = [Column::new("address", Width::Fit { min: 0, max: 9 }).sortable(false)];
        let mut state = GridState::new();
        let wide = GridTable::new(&capped, rows(HOSTS));
        assert_eq!(wide.measure(&mut state), 9);
        let buf = render_stateful_buffer(wide, 9, 3, &mut state);
        assert_eq!(buffer_rows(&buf), ["address  ", "10.0.0.1 ", "2001:db8…"]);
    }

    #[test]
    fn cells_are_measured_in_display_columns() {
        // mping measured bytes: 9 for the first of these and 15 for the second
        let columns = columns();
        let mut state = GridState::new();
        let wide = vec![TableRow::new(["日本語", "1", "ok"])];
        table(&columns, wide).measure(&mut state);
        assert_eq!(state.widths()[0], 7, "six columns of text: the header is wider");
        let accented = vec![TableRow::new(["ñandú.example", "1", "ok"])];
        table(&columns, accented).measure(&mut state);
        assert_eq!(state.widths()[0], 13);
    }

    #[test]
    fn a_row_and_a_column_are_selected() {
        let columns = columns();
        let mut state = GridState::new();
        state.cursor.select(Some(1));
        state.select_column(Some(1));
        let buf = render_stateful_buffer(table(&columns, rows(HOSTS)), 28, 6, &mut state);
        // the row: line 3, all of it; the column: x 14..18, under the header
        let reversed = |x: u16, y: u16| buf[(x, y)].modifier.contains(Modifier::REVERSED);
        let grey = |x: u16, y: u16| buf[(x, y)].bg == COLUMN_BG;
        assert!(reversed(1, 3) && reversed(13, 3) && reversed(26, 3), "the gaps too");
        assert!(!reversed(1, 2) && !reversed(0, 3), "not the other row, not the border");
        assert!(grey(14, 2) && grey(17, 2) && grey(14, 3), "the selected row's cell too");
        assert!(grey(14, 4), "and the empty line under the rows");
        assert!(!grey(14, 1), "not the header");
        assert!(!grey(13, 2) && !grey(18, 2), "not the gaps");

        // a column past the last is the last
        state.select_column(Some(9));
        render_stateful_buffer(table(&columns, rows(HOSTS)), 28, 6, &mut state);
        assert_eq!(state.column(), Some(2));
    }

    #[test]
    fn columns_step_and_stop_at_the_ends() {
        let mut state = GridState::new();
        state.next_column(3);
        assert_eq!(state.column(), Some(0), "from none: the first");
        state.next_column(3);
        state.next_column(3);
        state.next_column(3);
        assert_eq!(state.column(), Some(2), "no wrapping");
        state.previous_column(3);
        assert_eq!(state.column(), Some(1));
        state.select_column(None);
        state.previous_column(3);
        assert_eq!(state.column(), Some(2), "from none: the last");
        state.previous_column(0);
        assert_eq!(state.column(), None, "no columns, none selected");

        // keys: the arrows across are the column's, the rest the cursor's
        let mut state = GridState::new();
        assert_eq!(state.key(key(KeyCode::Right), 5, 3), NavOutcome::Handled);
        assert_eq!(state.key(key(KeyCode::Down), 5, 3), NavOutcome::Moved);
        assert_eq!(state.key(key(KeyCode::End), 5, 3), NavOutcome::Moved);
        assert_eq!((state.cursor.selected(), state.column()), (Some(4), Some(0)));
        let shift_right = KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT);
        assert_eq!(state.key(shift_right, 5, 3), NavOutcome::Ignored, "a chord is the app's");
        assert_eq!(state.key(key(KeyCode::Char('x')), 5, 3), NavOutcome::Ignored);
        state.deselect();
        assert_eq!((state.cursor.selected(), state.column()), (None, None));
    }

    #[test]
    fn a_scrollbar_over_the_border_when_rows_are_out_of_view() {
        let columns = columns();
        let right_border = |buf: &Buffer| -> String {
            (0..buf.area.height).map(|y| buf[(buf.area.width - 1, y)].symbol().to_string()).collect()
        };
        // ten rows in four lines
        let mut state = GridState::new();
        let buf = render_stateful_buffer(table(&columns, many()), 24, 7, &mut state);
        let bar = right_border(&buf);
        assert!(bar.starts_with('▲') && bar.ends_with('▼') && bar.contains('█'), "{bar}");
        let top = bar.find('█');
        // it follows the selected row
        state.cursor.select(Some(9));
        let buf = render_stateful_buffer(table(&columns, many()), 24, 7, &mut state);
        assert!(right_border(&buf).find('█') > top, "{}", right_border(&buf));
        assert_eq!(buffer_rows(&buf)[5], "│10.0.0.9     9  ok    █", "the rows keep their columns");

        // all in view: the border
        let buf = render_stateful_buffer(table(&columns, many()), 24, 13, &mut state);
        assert_eq!(right_border(&buf), "┐│││││││││││┘");
        // turned off, and without a border to draw it on
        let off = table(&columns, many()).scrollbar(None);
        assert_eq!(right_border(&render_stateful_buffer(off, 24, 7, &mut state)), "┐│││││┘");
        let bare = GridTable::new(&columns, many());
        let buf = render_stateful_buffer(bare, 24, 5, &mut state);
        assert!(!right_border(&buf).contains(['▲', '█', '▼']), "{}", right_border(&buf));
    }

    #[test]
    fn rows_of_the_window_only() {
        let columns = columns();
        let all = many();
        let mut state = GridState::new();
        state.cursor.select(Some(7));
        // a bordered table of five lines: three inside, the header one of them
        let window = state.window(all.len(), 3);
        assert_eq!(window, 6..8);
        let grid = table(&columns, all[window].to_vec()).windowed(all.len()).indicator("rows");
        let buf = render_stateful_buffer(grid, 24, 5, &mut state);
        let rows = buffer_rows(&buf);
        assert!(rows[2].starts_with("│10.0.0.6") && rows[3].starts_with("│10.0.0.7"), "{rows:?}");
        assert!(rows[4].contains(" rows 7–8 of 10 "), "{rows:?}");
        assert!(buf[(1, 3)].modifier.contains(Modifier::REVERSED), "row 7, the second of the window");
        assert!(!buf[(1, 2)].modifier.contains(Modifier::REVERSED));
        assert_eq!(state.cursor.offset(), 6, "the draw leaves the cursor alone");

        // the terminal shrank since the rows were formatted: those that fit
        let grid = table(&columns, all[6..8].to_vec()).windowed(all.len()).indicator("rows");
        let buf = render_stateful_buffer(grid, 24, 4, &mut state);
        let rows = buffer_rows(&buf);
        assert!(rows[2].starts_with("│10.0.0.6") && rows[3].contains(" rows 7–7 of 10 "), "{rows:?}");
        assert!(!buf[(1, 2)].modifier.contains(Modifier::REVERSED), "the selected row is out of view");
        assert_eq!(state.cursor.offset(), 6);
    }

    #[test]
    fn too_narrow_it_gives_way_as_a_column_table() {
        let columns = columns();
        let mut state = GridState::new();
        let buf = render_stateful_buffer(table(&columns, rows(HOSTS)), 20, 5, &mut state);
        assert_eq!(
            buffer_rows(&buf),
            [
                "┌──────────────────┐",
                "│address      sent │",
                "│10.0.0.1       12 │",
                "│2001:db8::1     3 │",
                "└ 1 column hidden ─┘",
            ]
        );
        assert_eq!(state.widths(), [11, 4, 7], "what it would take is remembered all the same");
    }

    #[test]
    fn a_fill_column_takes_what_a_wider_area_leaves() {
        let columns = [
            Column::new("note", Width::Fill { min: 4 }).sortable(false),
            Column::new("n", Width::CONTENT).sortable(false).right(),
        ];
        let rows = || vec![TableRow::new([Line::from("a long note"), Line::from("12")])];
        let mut state = GridState::new();
        let grid = GridTable::new(&columns, rows());
        assert_eq!(grid.measure(&mut state), 4 + 1 + 2, "a fill column asks for its least");
        let buf = render_stateful_buffer(grid, 7, 2, &mut state);
        assert_eq!(buffer_rows(&buf), ["note  n", "a l… 12"]);
        let buf = render_stateful_buffer(GridTable::new(&columns, rows()), 16, 2, &mut state);
        assert_eq!(buffer_rows(&buf), ["note           n", "a long note   12"]);
    }

    #[test]
    fn the_sort_column_is_marked() {
        let columns = [
            Column::new("address", Width::CONTENT),
            Column::new("sent", Width::CONTENT).right().descending_first(),
        ];
        let mut state = GridState::new();
        let two = || vec![TableRow::new(["10.0.0.1", "12"])];
        let grid = GridTable::new(&columns, two()).sort(Some(Sort::by(&columns, 1)));
        // a sortable column keeps room for the marker, sorted or not
        assert_eq!(grid.measure(&mut state), 9 + 1 + 6);
        let buf = render_stateful_buffer(grid, 16, 2, &mut state);
        assert_eq!(buffer_rows(&buf), ["address   sent ▼", "10.0.0.1      12"]);
    }
}
