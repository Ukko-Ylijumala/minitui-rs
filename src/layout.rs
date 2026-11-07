// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Placing areas: [`centered_rect`], the popup placement squawk, nft-browser
and zfs-browser each had a copy of, and [`Split`], zfs-browser's list /
detail split that Ctrl-←/→ moves, kept apart from its app: a share in
percent that stays within bounds, and a pane that is never drawn narrower
than a set number of cells while the area has room for both.
*/

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};

/* ===== consts and statics ================================================= */

const PERCENT: u16 = 100;
/// How far one key moves a [`Split`] unless told otherwise, in percent.
const SPLIT_STEP: u16 = 5;
/// Neither pane's share goes below this unless told otherwise, in percent.
const SPLIT_MIN_PCT: u16 = 15;

/* ===== types ============================================================== */

/**
Two panes side by side (or one above the other) with an adjustable share:
the first pane's in percent, moved a step at a time by Ctrl-←/→ (Ctrl-↑/↓
for a vertical split) within `min_pct` of either edge. Kept across frames
by the app; an app with several kinds of view keeps a `Split` for each, so
each keeps the share the user gave it (zfs-browser's list and table).

```no_run
# use crossterm::event::{KeyCode, KeyEvent};
# use ratatui::layout::Rect;
use minitui::Split;

let mut split = Split::horizontal(45).min_cells(24);
# let (key, body) = (KeyEvent::from(KeyCode::Left), Rect::ZERO);
# let mut status = String::new();
// in the key handler
if let Some(pct) = split.key(key) {
    status = format!("list pane: {pct}% of the width");
}
// drawing
let [list, detail] = split.areas(body);
```
*/
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split {
    direction: Direction,
    /// The share it was made with, for [`reset`](Split::reset).
    base: u16,
    pct: u16,
    step: u16,
    min_pct: u16,
    min_cells: u16,
}

/* ===== centered_rect ====================================================== */

/// A `width` × `height` area centered in `outer`, capped to it.
pub fn centered_rect(outer: Rect, width: u16, height: u16) -> Rect {
    outer.centered(
        Constraint::Length(width.min(outer.width)),
        Constraint::Length(height.min(outer.height)),
    )
}

/* ===== Split ============================================================== */

impl Split {
    /// Panes side by side, the left one taking `pct` percent of the width.
    pub fn horizontal(pct: u16) -> Self {
        Self::new(Direction::Horizontal, pct)
    }

    /// Panes one above the other, the top one taking `pct` percent of the height.
    pub fn vertical(pct: u16) -> Self {
        Self::new(Direction::Vertical, pct)
    }

    fn new(direction: Direction, pct: u16) -> Self {
        let pct = pct.min(PERCENT);
        Self {
            direction,
            base: pct,
            pct,
            step: SPLIT_STEP,
            min_pct: SPLIT_MIN_PCT,
            min_cells: 0,
        }
    }

    /// How far one key (or one of [`shift`](Split::shift)'s steps) moves it, in percent.
    pub fn step(mut self, pct: u16) -> Self {
        self.step = pct.min(PERCENT);
        self
    }

    /// Moving never takes either pane's share below this, in percent (at most 50).
    pub fn min_pct(mut self, pct: u16) -> Self {
        self.min_pct = pct.min(PERCENT / 2);
        self
    }

    /**
    Neither pane is drawn smaller than this many cells while the area has
    room for both; in a smaller one the share stands. Off (0) by default.
    */
    pub fn min_cells(mut self, cells: u16) -> Self {
        self.min_cells = cells;
        self
    }

    /// The first pane's share, in percent.
    pub fn pct(&self) -> u16 {
        self.pct
    }

    /// Set the first pane's share (a saved one, say), kept within the bounds.
    pub fn set_pct(&mut self, pct: u16) {
        self.pct = pct.clamp(self.min_pct, PERCENT - self.min_pct);
    }

    /**
    Move the split by `steps` steps, positive toward the end (right, down),
    within the bounds. Returns the first pane's new share, in percent. At a
    bound a step back moves at once: there is no stored overshoot.
    */
    pub fn shift(&mut self, steps: i16) -> u16 {
        let moved = i32::from(self.pct) + i32::from(steps) * i32::from(self.step);
        let lo = i32::from(self.min_pct);
        let hi = i32::from(PERCENT - self.min_pct);
        self.pct = u16::try_from(moved.clamp(lo, hi)).unwrap_or(self.base);
        self.pct
    }

    /// Back to the share it was made with.
    pub fn reset(&mut self) {
        self.pct = self.base;
    }

    /**
    Ctrl-← / Ctrl-→ (Ctrl-↑ / Ctrl-↓ for a vertical split) move it a step,
    a held key keeps moving it. Returns the first pane's new share for one
    of those keys, `None` for any other key, which the app handles itself.
    */
    pub fn key(&mut self, key: KeyEvent) -> Option<u16> {
        if key.kind == KeyEventKind::Release || !key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        let steps = match (self.direction, key.code) {
            (Direction::Horizontal, KeyCode::Left) | (Direction::Vertical, KeyCode::Up) => -1,
            (Direction::Horizontal, KeyCode::Right) | (Direction::Vertical, KeyCode::Down) => 1,
            _ => return None,
        };
        Some(self.shift(steps))
    }

    /**
    The two panes' constraints for `len` cells (the area's width, or its
    height for a vertical split): the first pane's share of `len`, held to
    `min_cells` on either side while `len` has room for both.
    */
    pub fn constraints(&self, len: u16) -> [Constraint; 2] {
        let mut first = u16::try_from(u32::from(len) * u32::from(self.pct) / u32::from(PERCENT))
            .unwrap_or(len);
        if len >= self.min_cells.saturating_mul(2) {
            first = first.clamp(self.min_cells, len - self.min_cells);
        }
        [Constraint::Length(first), Constraint::Fill(1)]
    }

    /// `area` split in two.
    pub fn areas(&self, area: Rect) -> [Rect; 2] {
        let len = match self.direction {
            Direction::Horizontal => area.width,
            Direction::Vertical => area.height,
        };
        Layout::new(self.direction, self.constraints(len)).areas(area)
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn centered_and_capped() {
        let outer = Rect::new(10, 5, 40, 20);
        assert_eq!(centered_rect(outer, 20, 10), Rect::new(20, 10, 20, 10));
        assert_eq!(centered_rect(outer, 100, 100), outer);
    }

    #[test]
    fn the_share_of_the_area() {
        let split = Split::horizontal(45);
        assert_eq!(split.constraints(200)[0], Constraint::Length(90));
        let [a, b] = split.areas(Rect::new(0, 0, 200, 10));
        assert_eq!((a.width, b.x, b.width), (90, 90, 110));
        let [top, bottom] = Split::vertical(25).areas(Rect::new(0, 0, 10, 40));
        assert_eq!((top.height, bottom.y, bottom.height), (10, 10, 30));
    }

    #[test]
    fn moves_within_bounds() {
        let mut split = Split::horizontal(45);
        assert_eq!(split.shift(1), 50);
        assert_eq!(split.shift(-2), 40);
        for _ in 0..30 {
            split.shift(1);
        }
        assert_eq!(split.pct(), 100 - SPLIT_MIN_PCT);
        // a step back from the bound moves at once
        assert_eq!(split.shift(-1), 100 - SPLIT_MIN_PCT - SPLIT_STEP);
        split.shift(-30);
        assert_eq!(split.pct(), SPLIT_MIN_PCT);
        split.reset();
        assert_eq!(split.pct(), 45);
        split.set_pct(99);
        assert_eq!(split.pct(), 100 - SPLIT_MIN_PCT);
    }

    #[test]
    fn panes_keep_a_minimum_size() {
        let split = Split::horizontal(15).min_cells(24);
        // 15% of 100 cells would be 15: held at the minimum, on both sides
        assert_eq!(split.constraints(100)[0], Constraint::Length(24));
        let split = Split::horizontal(85).min_cells(24);
        assert_eq!(split.constraints(100)[0], Constraint::Length(76));
        // too small for two minimums: the share stands
        assert_eq!(Split::horizontal(50).min_cells(24).constraints(40)[0], Constraint::Length(20));
    }

    #[test]
    fn keys_by_direction() {
        let mut split = Split::horizontal(50);
        assert_eq!(split.key(ctrl(KeyCode::Right)), Some(55));
        assert_eq!(split.key(ctrl(KeyCode::Left)), Some(50));
        assert_eq!(split.key(ctrl(KeyCode::Up)), None);
        assert_eq!(split.key(KeyEvent::from(KeyCode::Left)), None, "needs Ctrl");
        let mut release = ctrl(KeyCode::Left);
        release.kind = KeyEventKind::Release;
        assert_eq!(split.key(release), None);
        let mut split = Split::vertical(50).step(10);
        assert_eq!(split.key(ctrl(KeyCode::Down)), Some(60));
        assert_eq!(split.key(ctrl(KeyCode::Left)), None);
    }
}
