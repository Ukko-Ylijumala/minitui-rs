// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A share of a whole on one line: `ring      ▕██████▋     ▏  57.3%  1.2 G of 2.1 G`.
[`Gauge`] lays out a label, the bar, the percentage and a text, the bar
taking what the row leaves; [`Levels`] gives it the colour of how full it
is, and is of use on its own for a number that has a warning level.

Merged from collector-status's gauge lines (a bar in eighths between
`▕ ▏`, coloured by a warn and a bad level), zfs-browser's scrub progress
(`[████░░░░] 57.3%`), the malware scanner's phase bars and squawk's
filesystem usage colours.
*/

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::hbar;

/* ===== consts and statics ================================================= */

/// The bar's ends by default: thin, so a full bar and an empty one are both a box.
const CAPS: (&str, &str) = ("▕", "▏");
/// The unfilled part by default: nothing, which lets the bar end on an eighth.
static BLANK: &str = " ";
static FULL_BLOCK: &str = "█";
/// What stands for the percentage of a share that isn't known.
static NO_SHARE: &str = "–";
/// The fewest cells a bar that takes what is left is given.
const MIN_BAR: usize = 4;
/// Columns between the bar, the percentage and the text.
const GAP: usize = 2;
/// The integer digits of a percentage: up to `100`.
const PERCENT_DIGITS: usize = 3;
const PERCENT: f64 = 100.0;

/* ===== types ============================================================== */

/**
A warning level and a bad one, and the style of a value by where it lies:
green below both, yellow from the first, red from the second.

`Levels::new(0.75, 0.95)` is for a value that is worse the higher it is (a
queue, a disk filling up). With the bad level below the warning one it is
worse the lower it is: `Levels::new(0.2, 0.05)` for what is left of a
battery.

```
use minitui::Levels;
use ratatui::style::{Color, Style};

const DISK: Levels = Levels::new(80.0, 90.0);
assert_eq!(DISK.style(42.0), Style::new().fg(Color::Green));
assert_eq!(DISK.style(85.0), Style::new().fg(Color::Yellow));
assert_eq!(DISK.style(93.5), Style::new().fg(Color::Red));
```
*/
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    warn: f64,
    bad: f64,
    ok_style: Style,
    warn_style: Style,
    bad_style: Style,
}

/**
One line of a share: a label, the bar, the percentage, a text.

```
use minitui::{Gauge, Levels};

const RING: Levels = Levels::new(0.75, 0.95);

// in 60 columns: "ring      ▕██████████████████▎             ▏  57.3%  1.2 G of 2.1 G"
let line = Gauge::new(0.573)
    .label("ring", 10)
    .percent(1)
    .text("1.2 G of 2.1 G")
    .levels(RING)
    .line(60);
assert_eq!(line.width(), 60);

// a share that isn't known yet keeps its place: an empty bar and a dash
let waiting = Gauge::new(None).label("ring", 10).percent(1).line(40);
assert!(waiting.to_string().ends_with('–'));
```

The bar is in eighths of a cell while the unfilled part is blank; with a
[`track`](Self::track) glyph it is in whole cells, as `[████░░░░]` is.
*/
#[derive(Debug, Clone)]
#[must_use]
pub struct Gauge<'a> {
    share: Option<f64>,
    label: Option<(Span<'a>, usize)>,
    text: Option<Span<'a>>,
    percent: Option<u8>,
    levels: Option<Levels>,
    style: Style,
    track: &'a str,
    track_style: Style,
    caps: (&'a str, &'a str),
    cap_style: Style,
    bar_width: Option<usize>,
}

/* ===== Levels ============================================================= */

impl Levels {
    /// From `warn` on a warning, from `bad` on bad; the other way round
    /// when `bad` is the lower of the two.
    pub const fn new(warn: f64, bad: f64) -> Self {
        Self {
            warn,
            bad,
            ok_style: Style::new().fg(Color::Green),
            warn_style: Style::new().fg(Color::Yellow),
            bad_style: Style::new().fg(Color::Red),
        }
    }

    /// The three styles, in place of green, yellow and red.
    #[must_use]
    pub const fn styles(mut self, ok: Style, warn: Style, bad: Style) -> Self {
        (self.ok_style, self.warn_style, self.bad_style) = (ok, warn, bad);
        self
    }

    /// The style of `value`; a NaN is as good as any.
    pub fn style(&self, value: f64) -> Style {
        let (warn, bad) = match self.bad >= self.warn {
            true => (value >= self.warn, value >= self.bad),
            false => (value <= self.warn, value <= self.bad),
        };
        match (bad, warn) {
            (true, _) => self.bad_style,
            (false, true) => self.warn_style,
            (false, false) => self.ok_style,
        }
    }
}

/* ===== Gauge ============================================================== */

impl<'a> Gauge<'a> {
    /// A gauge of `share`, 0 to 1 (clamped); `None` for one that isn't known.
    pub fn new(share: impl Into<Option<f64>>) -> Self {
        Self {
            share: share.into().filter(|s| !s.is_nan()).map(|s| s.clamp(0.0, 1.0)),
            label: None,
            text: None,
            percent: None,
            levels: None,
            style: Style::new().fg(Color::Cyan),
            track: BLANK,
            track_style: Style::new().fg(Color::DarkGray),
            caps: CAPS,
            cap_style: Style::new().fg(Color::DarkGray),
            bar_width: None,
        }
    }

    /// A gauge of `done` out of `total`; not known while there is no total.
    pub fn of(done: f64, total: f64) -> Self {
        Self::new((total > 0.0).then(|| done / total))
    }

    /// What the gauge is of, before the bar, in a column `width` wide so a
    /// stack of gauges lines up. A `Span` brings its own style.
    pub fn label(mut self, label: impl Into<Span<'a>>, width: usize) -> Self {
        self.label = Some((label.into(), width));
        self
    }

    /// The percentage after the bar, with so many decimals, in the bar's colour.
    pub fn percent(mut self, decimals: u8) -> Self {
        self.percent = Some(decimals);
        self
    }

    /// What follows: the numbers the share is of, an ETA. A `Span` brings its own style.
    pub fn text(mut self, text: impl Into<Span<'a>>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// The bar's and the percentage's colour by how full it is, in place of one [`style`](Self::style).
    pub fn levels(mut self, levels: Levels) -> Self {
        self.levels = Some(levels);
        self
    }

    /// The bar's and the percentage's style (cyan by default).
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// What the unfilled part is drawn with, one cell wide: `░` for
    /// `████░░░░`. The bar is then in whole cells.
    pub fn track(mut self, glyph: &'a str) -> Self {
        self.track = glyph;
        self
    }

    /// The bar's ends: `("[", "]")`, or two empty strings for none.
    pub fn caps(mut self, left: &'a str, right: &'a str) -> Self {
        self.caps = (left, right);
        self
    }

    /// The style of the bar's ends and of its unfilled part (dark gray by default).
    pub fn chrome_style(mut self, style: impl Into<Style>) -> Self {
        let style = style.into();
        (self.cap_style, self.track_style) = (style, style);
        self
    }

    /// The bar's width in cells, in place of what the line leaves it.
    pub fn bar_width(mut self, cells: usize) -> Self {
        self.bar_width = Some(cells);
        self
    }

    /// The gauge laid out for a row `width` columns wide: the bar takes what
    /// the other parts leave, and at least a few cells of it.
    pub fn line(&self, width: usize) -> Line<'a> {
        let color = match (self.levels, self.share) {
            (Some(levels), Some(share)) => levels.style(share),
            _ => self.style,
        };
        let percent = self.percent.map(|decimals| {
            let decimals = usize::from(decimals);
            // "100", and the point and the decimals, and the sign
            let cols = PERCENT_DIGITS + if decimals > 0 { decimals + 1 } else { 0 } + 1;
            match self.share {
                Some(share) => format!("{:>cols$}", format!("{:.decimals$}%", share * PERCENT)),
                None => format!("{NO_SHARE:>cols$}"),
            }
        });

        let label_cols = self.label.as_ref().map_or(0, |(_, width)| *width);
        let caps_cols = self.caps.0.width() + self.caps.1.width();
        let percent_cols = percent.as_ref().map_or(0, |p| GAP + p.width());
        let text_cols = self.text.as_ref().map_or(0, |t| GAP + t.width());
        let bar_cols = self.bar_width.unwrap_or_else(|| {
            width
                .saturating_sub(label_cols + caps_cols + percent_cols + text_cols)
                .max(MIN_BAR)
        });

        let mut spans: Vec<Span<'a>> = Vec::with_capacity(8);
        if let Some((label, cols)) = &self.label {
            let pad = cols.saturating_sub(label.width());
            spans.push(label.clone());
            spans.push(Span::raw(BLANK.repeat(pad)));
        }
        spans.push(Span::styled(self.caps.0, self.cap_style));
        let (filled, rest) = self.bar(bar_cols);
        spans.push(Span::styled(filled, color));
        spans.push(Span::styled(rest, self.track_style));
        spans.push(Span::styled(self.caps.1, self.cap_style));
        if let Some(percent) = percent {
            spans.push(Span::styled(format!("{:GAP$}{percent}", ""), color));
        }
        if let Some(text) = &self.text {
            spans.push(Span::raw(BLANK.repeat(GAP)));
            spans.push(text.clone());
        }
        spans.retain(|span| !span.content.is_empty());
        Line::from(spans)
    }

    /// The bar `cells` wide: what is filled, and what is left of it.
    fn bar(&self, cells: usize) -> (String, String) {
        let share = self.share.unwrap_or(0.0);
        let filled = match self.track == BLANK {
            // in eighths, and a share above nothing shows at least a sliver
            true => hbar(share, 1.0, cells),
            false => FULL_BLOCK.repeat((share * cells as f64).round() as usize),
        };
        let rest = self.track.repeat(cells.saturating_sub(filled.chars().count()));
        (filled, rest)
    }
}

impl Widget for Gauge<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.line(area.width.into()).render(area, buf);
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{buffer_text, render_buffer};

    fn green() -> Style {
        Style::new().fg(Color::Green)
    }
    fn yellow() -> Style {
        Style::new().fg(Color::Yellow)
    }
    fn red() -> Style {
        Style::new().fg(Color::Red)
    }

    #[test]
    fn levels_rising_and_falling() {
        let rising = Levels::new(0.75, 0.95);
        for (value, style) in [
            (0.0, green()),
            (0.749, green()),
            (0.75, yellow()),
            (0.949, yellow()),
            (0.95, red()),
            (7.0, red()),
            (f64::NAN, green()),
        ] {
            assert_eq!(rising.style(value), style, "{value}");
        }
        let falling = Levels::new(0.2, 0.05);
        for (value, style) in [
            (1.0, green()),
            (0.21, green()),
            (0.2, yellow()),
            (0.06, yellow()),
            (0.05, red()),
            (0.0, red()),
        ] {
            assert_eq!(falling.style(value), style, "{value}");
        }
        let own = rising.styles(Style::new(), yellow(), Style::new().bg(Color::Red));
        assert_eq!(own.style(0.1), Style::new());
        assert_eq!(own.style(0.99), Style::new().bg(Color::Red));
    }

    #[test]
    fn the_bar_takes_what_the_row_leaves() {
        let gauge = Gauge::new(0.5).label("ring", 6).percent(1).text("1 of 2");
        //       label  cap  bar          cap  gap+pct   gap+text
        assert_eq!(
            gauge.line(40).to_string(),
            "ring  ▕████████        ▏   50.0%  1 of 2"
        );
        assert_eq!(gauge.line(40).width(), 40);
        assert_eq!(gauge.line(60).width(), 60);
        // too narrow: the bar keeps a few cells and the line runs over
        assert_eq!(gauge.line(10).to_string(), "ring  ▕██  ▏   50.0%  1 of 2");
        // a bar of a given width, whatever the row's
        assert_eq!(
            Gauge::new(0.5).bar_width(8).percent(0).line(80).to_string(),
            "▕████    ▏   50%"
        );
    }

    #[test]
    fn eighths_and_the_ends() {
        let bar = |share: f64| Gauge::new(share).bar_width(4).line(0).to_string();
        assert_eq!(bar(0.0), "▕    ▏");
        // a little is a sliver, not nothing
        assert_eq!(bar(0.001), "▕▏   ▏");
        assert_eq!(bar(0.4), "▕█▋  ▏");
        assert_eq!(bar(1.0), "▕████▏");
        // clamped, and a NaN is a share that isn't known
        assert_eq!(bar(7.0), "▕████▏");
        assert_eq!(bar(-1.0), "▕    ▏");
        assert_eq!(bar(f64::NAN), "▕    ▏");
    }

    #[test]
    fn a_track_and_brackets() {
        let gauge = |share: f64| {
            Gauge::new(share)
                .caps("[", "]")
                .track("░")
                .bar_width(8)
                .percent(1)
                .line(0)
                .to_string()
        };
        assert_eq!(gauge(0.573), "[█████░░░]   57.3%");
        assert_eq!(gauge(0.0), "[░░░░░░░░]    0.0%");
        assert_eq!(gauge(1.0), "[████████]  100.0%");
        assert_eq!(
            Gauge::new(0.5).caps("", "").bar_width(4).line(0).to_string(),
            "██  "
        );
    }

    #[test]
    fn a_share_that_is_not_known() {
        let line = Gauge::new(None).label("ring", 6).bar_width(4).percent(1).line(0);
        assert_eq!(line.to_string(), "ring  ▕    ▏       –");
        assert_eq!(Gauge::of(5.0, 0.0).bar_width(4).percent(0).line(0).to_string(), "▕    ▏     –");
        assert_eq!(Gauge::of(1.0, 4.0).bar_width(4).percent(0).line(0).to_string(), "▕█   ▏   25%");
        assert_eq!(Gauge::of(9.0, 4.0).bar_width(4).percent(0).line(0).to_string(), "▕████▏  100%");
    }

    #[test]
    fn coloured_by_its_levels() {
        let levels = Levels::new(0.75, 0.95);
        let styles = |share: f64| {
            let line = Gauge::new(share).bar_width(8).percent(0).levels(levels).line(0);
            // the filled part, and the percentage at the end
            (line.spans[1].style, line.spans.last().map(|s| s.style))
        };
        assert_eq!(styles(0.5), (green(), Some(green())));
        assert_eq!(styles(0.8), (yellow(), Some(yellow())));
        assert_eq!(styles(0.99), (red(), Some(red())));
        // without levels, the one style
        let line = Gauge::new(0.5).bar_width(8).style(Color::Magenta).line(0);
        assert_eq!(line.spans[1].style, Style::new().fg(Color::Magenta));
        // a label keeps the style it came with
        let line = Gauge::new(0.5).label(Span::styled("ring", red()), 6).line(20);
        assert_eq!(line.spans[0].style, red());
    }

    #[test]
    fn renders_as_a_widget() {
        let buf = render_buffer(Gauge::new(0.25).label("q", 2).percent(0), 20, 1);
        assert_eq!(buffer_text(&buf).trim_end(), "q ▕██▌       ▏   25%");
    }
}
