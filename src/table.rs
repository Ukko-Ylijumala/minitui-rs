// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A table whose columns fit the width: [`ColumnTable`] lays out its
[`Column`]s for the area, drops the least important when they don't all
fit, cuts each cell to its column with an `…`, and marks the [`Sort`]
column in the header. Rows are formatted only for the rows in view, from
a [`ListCursor`]. Merged from the apps' tables:
- every one had fixed-width numbers and one flexible name column; numbers
  were right-aligned by padding strings (zfs-browser, flood, ztop) or a
  helper cell (squawk);
- squawk left out the columns that didn't fit and said so in the border,
  because a right-aligned number squeezed by ratatui loses its leading
  digits; zfs-browser paged its columns instead;
- zfs-browser counted the sort marker into the header's width, so the
  columns don't move when the sort does; it, squawk and mping-rs sorted
  the rows without a value last both ways, numbers largest first;
- the sort keys differed (`+`/`-`, `s`, `v`, `r`, ⇧↑↓), so they stay with
  the app: [`Sort`] only steps, reverses and compares;
- nft-browser measured its key column in bytes, which wide text overflows.
*/

use std::cmp::Ordering;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, StatefulWidget, Widget};
use unicode_width::UnicodeWidthStr;

use crate::text::{Cut, fit_line};
use crate::{ListCursor, range_label};

/* ===== consts and statics ================================================= */

static ASCENDING: &str = "▲";
static DESCENDING: &str = "▼";
/// What a sortable column's header keeps room for: a space and the marker.
const SORT_MARK_W: usize = 2;
static DEFAULT_INDICATOR: &str = "rows";
const DEFAULT_GAP: u16 = 1;
const HEADER_ROWS: u16 = 1;

/* ===== types ============================================================== */

/// How wide a column is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// Always this many columns.
    Fixed(u16),
    /// As wide as its header and the rows in view need, within `min..=max`.
    Fit { min: u16, max: u16 },
    /// What the others leave, shared with the other fill columns; at least `min`.
    Fill { min: u16 },
}

/**
One column: its header, its [`Width`], and what gives way first. A
column of lower `priority` is left out before one of higher when they
don't all fit, the rightmost first among equals; the last one left is
never dropped.
*/
#[derive(Debug, Clone)]
pub struct Column<'a> {
    title: &'a str,
    width: Width,
    priority: u8,
    alignment: Alignment,
    cut: Cut,
    sortable: bool,
    descending_first: bool,
}

/**
Which column the rows are sorted by, and which way. The sorting is the
app's; this says what to show and helps compare. Wrap it in an `Option`
for an unsorted state.

```no_run
# use std::cmp::Ordering;
# use crossterm::event::{KeyCode, KeyEvent};
use minitui::{Column, Sort, Width};

# fn demo(key: KeyEvent, columns: &[Column], rows: &mut [(String, Option<u64>)], sort: &mut Sort) {
match key.code {
    KeyCode::Char('s') => sort.step(columns, 1),
    KeyCode::Char('S') => sort.reverse(),
    _ => {}
}
rows.sort_by(|a, b| match sort.column {
    0 => sort.apply(a.0.cmp(&b.0)),
    _ => sort.cmp_opt(a.1, b.1),
});
# }
```
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub descending: bool,
}

/// One row of a [`ColumnTable`]: a line per column, or a heading across them all.
#[derive(Debug, Clone, Default)]
pub struct TableRow<'a> {
    cells: Vec<Line<'a>>,
    heading: Option<Line<'a>>,
    style: Style,
}

/**
The table: `columns`, `len` rows of which [`row`](ColumnTable::new) formats
those in view, a header naming the columns (the [`Sort`] column marked
▲ / ▼), the [`ListCursor`]'s row highlighted. With a block, its bottom
border says `rows a–b of N` when they don't all show, and how many
columns were left out.

```no_run
# use ratatui::Frame;
# use ratatui::layout::Rect;
# use ratatui::text::Line;
# use ratatui::widgets::Block;
use minitui::{Column, ColumnTable, ListCursor, Sort, TableRow, Width};

# fn draw(frame: &mut Frame, area: Rect, names: &[(String, u64)], cursor: &mut ListCursor, sort: Sort) {
let columns = [
    Column::new("name", Width::Fill { min: 10 }).priority(9),
    Column::new("size", Width::Fixed(8)).right().descending_first(),
];
let table = ColumnTable::new(&columns, names.len(), |i| {
    TableRow::new([Line::from(names[i].0.as_str()), Line::from(names[i].1.to_string()).right_aligned()])
})
.sort(Some(sort))
.block(Block::bordered().title(" files "));
frame.render_stateful_widget(table, area, cursor);
# }
```
*/
pub struct ColumnTable<'a, F> {
    columns: &'a [Column<'a>],
    len: usize,
    row: F,
    sort: Option<Sort>,
    block: Option<Block<'a>>,
    indicator: Option<&'a str>,
    gap: u16,
    header_style: Style,
    highlight_style: Style,
    note_style: Style,
}

/* ===== Column ============================================================= */

impl<'a> Column<'a> {
    /// Left-aligned, cut at the end, sortable ascending first, priority 0.
    pub fn new(title: &'a str, width: Width) -> Self {
        Self {
            title,
            width,
            priority: 0,
            alignment: Alignment::Left,
            cut: Cut::End,
            sortable: true,
            descending_first: false,
        }
    }

    /// Kept over the columns of lower priority when they don't all fit.
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Right-aligned, header too: numbers.
    pub fn right(self) -> Self {
        self.align(Alignment::Right)
    }

    pub fn align(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Which end of a cell gives way: [`Cut::Start`] for paths.
    pub fn cut(mut self, cut: Cut) -> Self {
        self.cut = cut;
        self
    }

    /// Whether [`Sort::step`] stops here (and the header keeps room for the marker).
    pub fn sortable(mut self, sortable: bool) -> Self {
        self.sortable = sortable;
        self
    }

    /// Sorted largest first when [`Sort::step`] arrives: counts, sizes, rates.
    pub fn descending_first(mut self) -> Self {
        self.descending_first = true;
        self
    }

    pub fn title(&self) -> &'a str {
        self.title
    }

    /// The columns the header needs: the title, and the sort marker's room.
    fn header_width(&self) -> usize {
        self.title.width() + if self.sortable { SORT_MARK_W } else { 0 }
    }
}

/* ===== Sort =============================================================== */

impl Sort {
    pub fn new(column: usize, descending: bool) -> Self {
        Self { column, descending }
    }

    /// Sorted by `column` in its first direction.
    pub fn by(columns: &[Column], column: usize) -> Self {
        let descending = columns.get(column).is_some_and(|c| c.descending_first);
        Self { column, descending }
    }

    /// Sorted by the first sortable column (the first column when none is), in its first direction.
    pub fn first(columns: &[Column]) -> Self {
        Self::by(columns, columns.iter().position(|c| c.sortable).unwrap_or(0))
    }

    /// To the next (`delta` > 0) or previous sortable column, wrapping, in its first direction.
    pub fn step(&mut self, columns: &[Column], delta: isize) {
        let n = columns.len() as isize;
        if n == 0 || delta == 0 {
            return;
        }
        let dir = delta.signum();
        let mut at = self.column as isize;
        let mut left = delta.abs();
        for _ in 0..n * delta.abs() {
            at = (at + dir).rem_euclid(n);
            if columns[at as usize].sortable {
                left -= 1;
                if left == 0 {
                    *self = Self::by(columns, at as usize);
                    return;
                }
            }
        }
    }

    pub fn reverse(&mut self) {
        self.descending = !self.descending;
    }

    /// An ascending comparison turned the way the sort goes.
    pub fn apply(&self, ord: Ordering) -> Ordering {
        match self.descending {
            true => ord.reverse(),
            false => ord,
        }
    }

    /// As [`apply`](Sort::apply), with the rows that have no value last either way.
    pub fn cmp_opt<T: Ord>(&self, a: Option<T>, b: Option<T>) -> Ordering {
        match (a, b) {
            (Some(a), Some(b)) => self.apply(a.cmp(&b)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }

    /// The marker the header shows: ▲ ascending, ▼ descending.
    pub fn marker(&self) -> &'static str {
        match self.descending {
            true => DESCENDING,
            false => ASCENDING,
        }
    }
}

/* ===== TableRow =========================================================== */

impl<'a> TableRow<'a> {
    /// A line per column, in the columns' order; missing ones are blank, extra ones ignored.
    pub fn new<L: Into<Line<'a>>>(cells: impl IntoIterator<Item = L>) -> Self {
        Self {
            cells: cells.into_iter().map(Into::into).collect(),
            ..Default::default()
        }
    }

    /// A line across all the columns: a group's heading, a separator.
    pub fn heading(line: impl Into<Line<'a>>) -> Self {
        Self {
            heading: Some(line.into()),
            ..Default::default()
        }
    }

    /// The style under the row's cells: dim for something inactive, red for a fault.
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }
}

/* ===== ColumnTable ======================================================== */

impl<'a, F: Fn(usize) -> TableRow<'a>> ColumnTable<'a, F> {
    /// `len` rows of `columns`, `row(i)` formatting row `i` when it is in view.
    pub fn new(columns: &'a [Column<'a>], len: usize, row: F) -> Self {
        Self {
            columns,
            len,
            row,
            sort: None,
            block: None,
            indicator: Some(DEFAULT_INDICATOR),
            gap: DEFAULT_GAP,
            header_style: Style::new().add_modifier(Modifier::BOLD),
            highlight_style: Style::new().add_modifier(Modifier::REVERSED),
            note_style: Style::new().add_modifier(Modifier::DIM),
        }
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

    /// What the bottom border counts (`rows`); `None` for no count.
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

    /// The bottom border's notes (dim).
    pub fn note_style(mut self, style: impl Into<Style>) -> Self {
        self.note_style = style.into();
        self
    }
}

/**
Each column's width in `width` cells, `None` for one left out, given the
rows in view: the narrowest each may be, columns dropped by priority
until those fit, then what is left shared by the fill columns.
*/
fn layout(columns: &[Column], rows: &[TableRow], width: u16, gap: u16) -> Vec<Option<u16>> {
    let content = |i: usize| {
        rows.iter()
            .filter_map(|r| r.cells.get(i))
            .map(Line::width)
            .max()
            .unwrap_or(0)
    };
    let need: Vec<u16> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| match c.width {
            Width::Fixed(n) => n,
            Width::Fit { min, max } => {
                let want = content(i).max(c.header_width());
                u16::try_from(want).unwrap_or(u16::MAX).clamp(min, max.max(min))
            }
            Width::Fill { min } => min,
        })
        .collect();
    let mut kept: Vec<bool> = vec![true; columns.len()];
    let total = |kept: &[bool]| -> u32 {
        let n = kept.iter().filter(|&&k| k).count() as u32;
        let cells: u32 = need.iter().zip(kept).filter(|(_, k)| **k).map(|(&w, _)| u32::from(w)).sum();
        cells + u32::from(gap) * n.saturating_sub(1)
    };
    while total(&kept) > u32::from(width) && kept.iter().filter(|&&k| k).count() > 1 {
        // the lowest priority, the rightmost among equals
        let drop = (0..columns.len())
            .rev()
            .filter(|&i| kept[i])
            .min_by_key(|&i| columns[i].priority)
            .expect("more than one column kept");
        kept[drop] = false;
    }
    let mut widths: Vec<Option<u16>> = need.iter().zip(&kept).map(|(&w, &k)| k.then_some(w)).collect();
    let used: u32 = total(&kept);
    let fills: Vec<usize> = (0..columns.len())
        .filter(|&i| kept[i] && matches!(columns[i].width, Width::Fill { .. }))
        .collect();
    if let Some(extra) = u32::from(width).checked_sub(used).filter(|&e| e > 0)
        && !fills.is_empty()
    {
        let each = extra / fills.len() as u32;
        let first_more = extra % fills.len() as u32;
        for (n, &i) in fills.iter().enumerate() {
            let add = each + if n == 0 { first_more } else { 0 };
            widths[i] = widths[i].map(|w| w.saturating_add(add as u16));
        }
    } else if used > u32::from(width) {
        // one column left, wider than the area
        widths = widths.into_iter().map(|w| w.map(|w| w.min(width))).collect();
    }
    widths
}

/// A line in `width` cells at `x`, `y`: cut to fit, aligned in the cells.
fn put(buf: &mut Buffer, x: u16, y: u16, width: u16, line: Line, cut: Cut, alignment: Alignment) {
    let fitted = fit_line(line, width.into(), cut);
    let w = (fitted.width() as u16).min(width);
    let offset = match fitted.alignment.unwrap_or(alignment) {
        Alignment::Left => 0,
        Alignment::Center => (width - w) / 2,
        Alignment::Right => width - w,
    };
    buf.set_line(x + offset, y, &fitted, w);
}

impl<'a, F: Fn(usize) -> TableRow<'a>> StatefulWidget for ColumnTable<'a, F> {
    type State = ListCursor;

    fn render(self, area: Rect, buf: &mut Buffer, cursor: &mut ListCursor) {
        let inner = self.block.as_ref().map_or(area, |b| b.inner(area));
        let body = inner.height.saturating_sub(HEADER_ROWS);
        let shown = cursor.window(self.len, body.into());
        let rows: Vec<TableRow> = shown.clone().map(&self.row).collect();
        let widths: Vec<Option<u16>> = layout(self.columns, &rows, inner.width, self.gap);

        if let Some(mut block) = self.block {
            if let Some(what) = self.indicator
                && self.len > usize::from(body)
            {
                let label = range_label(what, shown.clone(), self.len);
                block = block.title_bottom(Line::from(format!(" {label} ")).style(self.note_style).right_aligned());
            }
            let hidden = widths.iter().filter(|w| w.is_none()).count();
            if hidden > 0 {
                let note = match hidden {
                    1 => " 1 column hidden ".to_string(),
                    n => format!(" {n} columns hidden "),
                };
                block = block.title_bottom(Line::from(note).style(self.note_style).left_aligned());
            }
            block.render(area, buf);
        }
        if inner.is_empty() {
            return;
        }

        // the header, the sort column marked
        let mut x = inner.x;
        for (i, (column, width)) in self.columns.iter().zip(&widths).enumerate() {
            let Some(width) = *width else { continue };
            let sorted = self.sort.filter(|s| s.column == i);
            let mut spans = vec![Span::raw(column.title)];
            if let Some(sort) = sorted {
                spans.push(Span::raw(format!(" {}", sort.marker())));
            }
            // the marker stays when the title has to give way
            let line = match sorted {
                Some(_) if Line::from(spans.clone()).width() > usize::from(width) => {
                    let room = usize::from(width).saturating_sub(SORT_MARK_W);
                    let title = fit_line(Line::from(column.title), room, Cut::End);
                    Line::from([title.spans, vec![spans[1].clone()]].concat())
                }
                _ => Line::from(spans),
            };
            put(buf, x, inner.y, width, line.style(self.header_style), Cut::End, column.alignment);
            x = x.saturating_add(width + self.gap);
        }

        // the rows
        let selected: Option<usize> = cursor.selected_in_window();
        for (k, row) in rows.into_iter().enumerate() {
            let y = inner.y + HEADER_ROWS + k as u16;
            if y >= inner.bottom() {
                break;
            }
            let line_area = Rect { y, height: 1, ..inner };
            buf.set_style(line_area, row.style);
            match row.heading {
                Some(line) => put(buf, inner.x, y, inner.width, line, Cut::End, Alignment::Left),
                None => {
                    let mut x = inner.x;
                    let mut cells = row.cells.into_iter();
                    for (column, width) in self.columns.iter().zip(&widths) {
                        let cell: Line = cells.next().unwrap_or_default();
                        let Some(width) = *width else { continue };
                        put(buf, x, y, width, cell, column.cut, column.alignment);
                        x = x.saturating_add(width + self.gap);
                    }
                }
            }
            if selected == Some(k) {
                buf.set_style(line_area, self.highlight_style);
            }
        }
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::style::Color;

    use crate::buffer_rows;

    /// The columns of a file list: a path, a size, a date.
    fn columns() -> Vec<Column<'static>> {
        vec![
            Column::new("path", Width::Fill { min: 8 }).cut(Cut::Start).priority(9),
            Column::new("size", Width::Fit { min: 4, max: 10 }).right().descending_first(),
            Column::new("date", Width::Fixed(10)),
        ]
    }

    static ROWS: &[(&str, u64, &str)] = &[
        ("/srv/www/site/wp-config.php", 3_100, "2024-01-02"),
        ("/srv/www/site/index.php", 405, "2023-12-30"),
        ("/srv/www/site/wp-content/plugins/akismet/akismet.php", 2_222_222, "2024-02-10"),
    ];

    fn screen(columns: &[Column], sort: Option<Sort>, cursor: &mut ListCursor, w: u16, h: u16) -> Vec<String> {
        let table = ColumnTable::new(columns, ROWS.len(), |i| {
            let (path, size, date) = ROWS[i];
            TableRow::new([Line::from(path), Line::from(size.to_string()), Line::from(date)])
        })
        .sort(sort)
        .block(Block::bordered());
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        table.render(buf.area, &mut buf, cursor);
        buffer_rows(&buf)
    }

    #[test]
    fn columns_fit_and_give_way() {
        let columns = columns();
        let mut cursor = ListCursor::with_selected(Some(0));
        let sort = Some(Sort::by(&columns, 1));
        assert_eq!(
            screen(&columns, sort, &mut cursor, 50, 6),
            [
                "┌────────────────────────────────────────────────┐",
                "│path                           size ▼ date      │",
                "│/srv/www/site/wp-config.php      3100 2024-01-02│",
                "│/srv/www/site/index.php           405 2023-12-30│",
                "│…/plugins/akismet/akismet.php 2222222 2024-02-10│",
                "└────────────────────────────────────────────────┘",
            ]
        );
        // too narrow for all three: the date goes (priority 0, the rightmost), and says so
        assert_eq!(
            screen(&columns, sort, &mut cursor, 22, 6),
            [
                "┌────────────────────┐",
                "│path          size ▼│",
                "│…-config.php    3100│",
                "│…e/index.php     405│",
                "│…akismet.php 2222222│",
                "└ 1 column hidden ───┘",
            ]
        );
        // narrower still: the path alone, kept over the size by its priority
        let rows = screen(&columns, sort, &mut cursor, 8, 6);
        assert_eq!(rows[1..5], ["│path  │", "│…g.php│", "│…x.php│", "│…t.php│"]);
    }

    #[test]
    fn rows_in_view_and_their_styles() {
        let columns = columns();
        let mut cursor = ListCursor::with_selected(Some(2));
        let rows = screen(&columns, None, &mut cursor, 50, 4);
        // one row in view: the selected one, and the count in the border
        assert!(rows[2].contains("akismet.php"), "{rows:?}");
        assert!(rows[3].ends_with(" rows 3–3 of 3 ┘"), "{rows:?}");

        let table = ColumnTable::new(&columns, 2, |i| match i {
            0 => TableRow::heading("── web01 ──").style(Style::new().fg(Color::Cyan)),
            _ => TableRow::new(["/a", "1"]).style(Style::new().fg(Color::DarkGray)),
        });
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        let mut cursor = ListCursor::with_selected(Some(1));
        table.render(buf.area, &mut buf, &mut cursor);
        let rows = buffer_rows(&buf);
        assert_eq!(rows[1], "── web01 ──                   ");
        assert!(rows[2].starts_with("/a  ") && rows[2].contains("   1"), "{rows:?}");
        assert_eq!(buf[(0, 1)].fg, Color::Cyan);
        assert_eq!(buf[(0, 2)].fg, Color::DarkGray);
        assert!(buf[(0, 2)].modifier.contains(Modifier::REVERSED), "the selected row");
        assert!(!buf[(0, 1)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn sort_steps_reverses_and_compares() {
        let mut columns = columns();
        columns[2] = columns[2].clone().sortable(false);
        let mut sort = Sort::by(&columns, 0);
        assert_eq!(sort, Sort::new(0, false));
        let marked = [Column::new("", Width::Fixed(1)).sortable(false), Column::new("n", Width::Fixed(3)).descending_first()];
        assert_eq!(Sort::first(&marked), Sort::new(1, true), "past a column that isn't sortable");
        sort.step(&columns, 1);
        assert_eq!(sort, Sort::new(1, true), "a count starts largest first");
        sort.step(&columns, 1);
        assert_eq!(sort, Sort::new(0, false), "the date is skipped, and it wraps");
        sort.step(&columns, -1);
        assert_eq!(sort.column, 1);
        sort.reverse();
        assert_eq!(sort.marker(), ASCENDING);

        let mut values = vec![Some(3), None, Some(1), Some(2)];
        let down = Sort::new(1, true);
        values.sort_by(|a, b| down.cmp_opt(*a, *b));
        assert_eq!(values, [Some(3), Some(2), Some(1), None]);
        let up = Sort::new(1, false);
        values.sort_by(|a, b| up.cmp_opt(*a, *b));
        assert_eq!(values, [Some(1), Some(2), Some(3), None], "no value last either way");
    }
}
