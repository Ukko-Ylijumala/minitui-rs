// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Time series in braille: [`TimeChart`] draws [`Trace`]s as lines over the
stretch of time up to now, and [`braille_spark`] packs a sparkline into a
string. Merged from the apps' charts, which all built the same ratatui
`Chart` by hand: braille line datasets, y from 0 up to the peak with some
head room (flood, zfs-browser, collector-status) or rounded up to a nice
figure (squawk), labelled at the bottom, middle and top; x as time before
now, labelled `-5m … now` (squawk, zfs-browser, mping-rs). Besides doing
that once:
- a NaN value is a gap in its trace, not a line bridging it
  (collector-status's seams), and a lone point between gaps still shows;
- the scale follows only what is in view (collector-status);
- the traces' names and latest values go in the top border (squawk's
  title), not in ratatui's legend box, which hides itself when the chart
  is small (zfs-browser overrode that chart by chart).
*/

use std::borrow::Cow;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, Widget};

use crate::logview::{SECS_PER_DAY, SECS_PER_HOUR, SECS_PER_MIN};

/* ===== consts and statics ================================================= */

/// Head room over the peak by default, so the line clears the top border.
const DEFAULT_HEADROOM: f64 = 1.1;
/// The multiples of a power of ten that [`nice_ceil`] rounds up to.
const NICE_STEPS: [f64; 4] = [1.0, 2.0, 5.0, 10.0];
/// SI prefixes of the default y labels, largest first.
static SI_STEPS: [(f64, &str); 4] = [(1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "k")];
/// Below this a label keeps one decimal.
const ONE_DECIMAL_BELOW: f64 = 10.0;
static NOW: &str = "now";

/// Braille dots filled bottom-up: the left column, then the right.
const BRAILLE_LEFT: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
const BRAILLE_RIGHT: [u32; 4] = [0x80, 0x20, 0x10, 0x08];
const BRAILLE_BASE: u32 = 0x2800;
/// A sparkline cell holds two values, each a bar of up to four dots.
const VALUES_PER_CELL: usize = 2;
const DOTS_PER_VALUE: f64 = 4.0;

/* ===== types ============================================================== */

/**
One line of a [`TimeChart`]: points `(x, y)` with x the seconds before now
(≤ 0, rising), a NaN y a gap. Named, it is listed in the chart's top
border with its latest value.
*/
#[derive(Debug, Clone)]
pub struct Trace<'a> {
    name: Cow<'a, str>,
    style: Style,
    segments: Vec<Cow<'a, [(f64, f64)]>>,
}

/**
[`Trace`]s as braille lines from `-span` to now. The y axis runs from 0
(or a set floor, or the lowest value in view) to a set top or the peak in
view with [head room](Self::headroom), and is labelled at the bottom,
middle and top; the x axis is labelled `-5m`, `-2m30s`, `now`. Both label
formats can be the app's own. With a [`block`](Self::block), the named
traces and their latest values are listed in its top border, right.
*/
pub struct TimeChart<'a> {
    traces: Vec<Trace<'a>>,
    span: Option<Duration>,
    y_min: Option<f64>,
    y_max: Option<f64>,
    headroom: f64,
    nice: bool,
    y_label: Option<&'a dyn Fn(f64) -> String>,
    x_label: Option<&'a dyn Fn(Duration) -> String>,
    x_labels: bool,
    legend: bool,
    block: Option<Block<'a>>,
    axis_style: Style,
}

/* ===== Trace ============================================================== */

impl<'a> Trace<'a> {
    /**
    Points `(seconds before now, value)`, oldest first; borrowed points
    stay borrowed. A NaN value breaks the line there.
    */
    pub fn new(name: impl Into<Cow<'a, str>>, points: impl Into<Cow<'a, [(f64, f64)]>>) -> Self {
        let is_gap = |p: &(f64, f64)| p.1.is_nan();
        let segments = match points.into() {
            Cow::Borrowed(points) => points
                .split(is_gap)
                .filter(|s| !s.is_empty())
                .map(Cow::Borrowed)
                .collect(),
            Cow::Owned(points) if !points.iter().any(is_gap) => match points.is_empty() {
                true => Vec::new(),
                false => vec![Cow::Owned(points)],
            },
            Cow::Owned(points) => points
                .split(is_gap)
                .filter(|s| !s.is_empty())
                .map(|s| Cow::Owned(s.to_vec()))
                .collect(),
        };
        Self {
            name: name.into(),
            style: Style::new(),
            segments,
        }
    }

    /// `values` sampled `every` apart, oldest first, the last one now.
    pub fn samples(
        name: impl Into<Cow<'a, str>>,
        values: impl IntoIterator<Item = f64>,
        every: Duration,
    ) -> Self {
        let values: Vec<f64> = values.into_iter().collect();
        let step = every.as_secs_f64();
        let last = values.len().saturating_sub(1);
        let points: Vec<(f64, f64)> = values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (-((last - i) as f64) * step, v))
            .collect();
        Self::new(name, points)
    }

    /// Its colour, mostly.
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// The newest value that isn't a gap.
    pub fn latest(&self) -> Option<f64> {
        self.segments.last()?.last().map(|p| p.1)
    }

    /// Its oldest point's x.
    fn oldest(&self) -> Option<f64> {
        self.segments.first()?.first().map(|p| p.0)
    }

    /// The values from `from` seconds before now on.
    fn values_since(&self, from: f64) -> impl Iterator<Item = f64> + '_ {
        self.segments
            .iter()
            .flat_map(|s| s.iter())
            .filter(move |p| p.0 >= from)
            .map(|p| p.1)
    }
}

/* ===== TimeChart ========================================================== */

impl<'a> TimeChart<'a> {
    pub fn new(traces: impl IntoIterator<Item = Trace<'a>>) -> Self {
        Self {
            traces: traces.into_iter().collect(),
            span: None,
            y_min: Some(0.0),
            y_max: None,
            headroom: DEFAULT_HEADROOM,
            nice: false,
            y_label: None,
            x_label: None,
            x_labels: true,
            legend: true,
            block: None,
            axis_style: Style::new().fg(Color::DarkGray),
        }
    }

    /// The x axis runs from `-span` to now; by default from the oldest point.
    pub fn span(mut self, span: Duration) -> Self {
        self.span = Some(span);
        self
    }

    /// The bottom of the y axis: 0 by default; `None` the lowest value in view.
    pub fn y_min(mut self, min: impl Into<Option<f64>>) -> Self {
        self.y_min = min.into();
        self
    }

    /// A fixed top (100 for a percentage); `None` (the default) follows the peak.
    pub fn y_max(mut self, max: impl Into<Option<f64>>) -> Self {
        self.y_max = max.into();
        self
    }

    /// The top over the peak, as a factor of the range: 1.1 by default.
    pub fn headroom(mut self, factor: f64) -> Self {
        self.headroom = factor;
        self
    }

    /// Round a top that follows the peak up to 1, 2 or 5 times a power of ten.
    pub fn nice(mut self, nice: bool) -> Self {
        self.nice = nice;
        self
    }

    /// The y labels' format, also of the latest values: `12k`, `3.4M` by default.
    pub fn y_label(mut self, format: &'a dyn Fn(f64) -> String) -> Self {
        self.y_label = Some(format);
        self
    }

    /// The x labels' format, given the time before now: `90s` → `1m30s` by default.
    pub fn x_label(mut self, format: &'a dyn Fn(Duration) -> String) -> Self {
        self.x_label = Some(format);
        self
    }

    /// Whether the x axis is labelled (flood and collector-status left it bare).
    pub fn x_labels(mut self, show: bool) -> Self {
        self.x_labels = show;
        self
    }

    /// Whether the named traces are listed in the block's top border.
    pub fn legend(mut self, show: bool) -> Self {
        self.legend = show;
        self
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// The axes and their labels: dark grey by default.
    pub fn axis_style(mut self, style: impl Into<Style>) -> Self {
        self.axis_style = style.into();
        self
    }

    /// Where the x axis starts, in seconds before now (< 0).
    pub fn x_from(&self) -> f64 {
        let from = match self.span {
            Some(span) => -span.as_secs_f64(),
            None => self
                .traces
                .iter()
                .filter_map(Trace::oldest)
                .fold(0.0, f64::min),
        };
        if from < 0.0 { from } else { -1.0 }
    }

    /// The y axis's bottom and top, as drawn.
    pub fn y_bounds(&self) -> (f64, f64) {
        let from = self.x_from();
        let (lo, hi) = self
            .traces
            .iter()
            .flat_map(|t| t.values_since(from))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            });
        let bottom = self.y_min.unwrap_or(if lo.is_finite() { lo } else { 0.0 });
        let top = match self.y_max {
            Some(top) => top,
            None if hi.is_finite() => {
                let top = bottom + (hi - bottom) * self.headroom;
                if self.nice { nice_ceil(top) } else { top }
            }
            None => bottom,
        };
        if top > bottom {
            (bottom, top)
        } else {
            (bottom, bottom + 1.0)
        }
    }

    /// ` name latest ` for each named trace, in its style.
    fn legend_line(&self, format: &dyn Fn(f64) -> String) -> Option<Line<'static>> {
        let mut spans = Vec::new();
        for t in self.traces.iter().filter(|t| !t.name.is_empty()) {
            spans.push(Span::styled(format!(" {} ", t.name), t.style));
            spans.push(Span::raw(t.latest().map_or_else(|| "–".into(), format)));
        }
        if spans.is_empty() {
            return None;
        }
        spans.push(Span::raw(" "));
        Some(Line::from(spans))
    }
}

impl Widget for TimeChart<'_> {
    fn render(mut self, area: Rect, buf: &mut Buffer) {
        let si: &dyn Fn(f64) -> String = &si_label;
        let y_fmt = self.y_label.unwrap_or(si);
        let from = self.x_from();
        let (bottom, top) = self.y_bounds();
        let legend = match self.legend {
            true => self.legend_line(y_fmt),
            false => None,
        };
        let label = |text: String| Span::styled(text, self.axis_style);

        let y_axis = Axis::default()
            .bounds([bottom, top])
            .style(self.axis_style)
            .labels([bottom, (bottom + top) / 2.0, top].map(|v| label(y_fmt(v))));
        let mut x_axis = Axis::default().bounds([from, 0.0]).style(self.axis_style);
        if self.x_labels {
            let dur: &dyn Fn(Duration) -> String = &span_label;
            let x_fmt = self.x_label.unwrap_or(dur);
            let span = Duration::from_secs_f64(-from);
            x_axis = x_axis.labels([
                label(format!("-{}", x_fmt(span))),
                label(format!("-{}", x_fmt(span / 2))),
                label(NOW.into()),
            ]);
        }

        // unnamed datasets: ratatui draws no legend box of its own
        let datasets: Vec<Dataset> = self
            .traces
            .iter()
            .flat_map(|t| {
                t.segments.iter().map(|seg| {
                    let kind = match seg.len() {
                        1 => GraphType::Scatter,
                        _ => GraphType::Line,
                    };
                    Dataset::default()
                        .marker(Marker::Braille)
                        .graph_type(kind)
                        .style(t.style)
                        .data(seg)
                })
            })
            .collect();
        let mut chart = Chart::new(datasets).x_axis(x_axis).y_axis(y_axis);
        if let Some(mut block) = self.block.take() {
            if let Some(legend) = legend {
                block = block.title_top(legend.right_aligned());
            }
            chart = chart.block(block);
        }
        chart.render(area, buf);
    }
}

/* ===== free functions ===================================================== */

/**
`values` as a sparkline `cells` wide in braille: two values a cell, each a
bar of up to four dots, scaled to `max` (the largest value shown when
`None`). The newest value is the rightmost; with fewer values than fit,
the left stays blank. A value above zero always shows a dot, so a trickle
never looks dead. From collector-status.
*/
pub fn braille_spark(values: &[f64], cells: usize, max: Option<f64>) -> String {
    let take = values.len().min(cells * VALUES_PER_CELL);
    let shown = &values[values.len() - take..];
    let top = max.unwrap_or_else(|| {
        shown
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(0.0, f64::max)
    });
    let dots = |v: f64| -> usize {
        match v.is_finite() && v > 0.0 && top > 0.0 {
            true => ((v / top * DOTS_PER_VALUE).ceil() as usize).clamp(1, BRAILLE_LEFT.len()),
            false => 0,
        }
    };
    let pad = cells * VALUES_PER_CELL - take;
    let at = |i: usize| {
        i.checked_sub(pad)
            .and_then(|i| shown.get(i))
            .map_or(0, |&v| dots(v))
    };
    (0..cells)
        .map(|c| {
            let i = c * VALUES_PER_CELL;
            let bits: u32 = BRAILLE_LEFT[..at(i)].iter().sum::<u32>()
                + BRAILLE_RIGHT[..at(i + 1)].iter().sum::<u32>();
            char::from_u32(BRAILLE_BASE + bits).unwrap_or(' ')
        })
        .collect()
}

/// `v` rounded up to 1, 2 or 5 times a power of ten (squawk's); 1 for none.
pub fn nice_ceil(v: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(v.log10().floor());
    NICE_STEPS
        .into_iter()
        .map(|m| m * magnitude)
        .find(|n| *n >= v)
        .unwrap_or(NICE_STEPS[NICE_STEPS.len() - 1] * magnitude)
}

/// `950`, `9.5`, `12k`, `3.4M`: the default y label.
fn si_label(v: f64) -> String {
    let (x, unit) = SI_STEPS
        .iter()
        .find(|(scale, _)| v.abs() >= *scale)
        .map_or((v, ""), |&(scale, unit)| (v / scale, unit));
    match x.abs() < ONE_DECIMAL_BELOW && x.fract() != 0.0 {
        true => format!("{x:.1}{unit}"),
        false => format!("{x:.0}{unit}"),
    }
}

/// `45s`, `5m`, `1m30s`, `2h`, `1d6h`: the default x label, to the second.
fn span_label(d: Duration) -> String {
    let s = d.as_secs();
    let two = |big: u64, a: &str, small: u64, b: &str| match small {
        0 => format!("{big}{a}"),
        _ => format!("{big}{a}{small}{b}"),
    };
    match s {
        0..SECS_PER_MIN => format!("{s}s"),
        SECS_PER_MIN..SECS_PER_HOUR => two(s / SECS_PER_MIN, "m", s % SECS_PER_MIN, "s"),
        SECS_PER_HOUR..SECS_PER_DAY => two(
            s / SECS_PER_HOUR,
            "h",
            s % SECS_PER_HOUR / SECS_PER_MIN,
            "m",
        ),
        _ => two(s / SECS_PER_DAY, "d", s % SECS_PER_DAY / SECS_PER_HOUR, "h"),
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn screen(chart: TimeChart, w: u16, h: u16) -> Vec<String> {
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        chart.render(buf.area, &mut buf);
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn braille_cells(rows: &[String]) -> usize {
        rows.iter()
            .flat_map(|r| r.chars())
            .filter(|c| ('\u{2801}'..='\u{28FF}').contains(c))
            .count()
    }

    #[test]
    fn sparkline_packs_two_values_a_cell() {
        assert_eq!(braille_spark(&[], 2, None), "\u{2800}\u{2800}");
        assert_eq!(braille_spark(&[4.0, 4.0], 1, None), "⣿");
        assert_eq!(braille_spark(&[0.01, 4.0], 1, Some(4.0)), "⣸");
        assert_eq!(braille_spark(&[4.0], 2, None), "\u{2800}⢸");
        // only the newest that fit; a NaN is blank
        assert_eq!(braille_spark(&[9.0, f64::NAN, 4.0], 1, Some(4.0)), "⢸");
    }

    #[test]
    fn nice_ceilings() {
        assert_eq!(nice_ceil(0.0), 1.0);
        assert_eq!(nice_ceil(1.0), 1.0);
        assert_eq!(nice_ceil(1.2), 2.0);
        assert_eq!(nice_ceil(3.0), 5.0);
        assert_eq!(nice_ceil(730.0), 1000.0);
        assert_eq!(nice_ceil(0.03), 0.05);
    }

    #[test]
    fn labels() {
        assert_eq!(si_label(0.0), "0");
        assert_eq!(si_label(9.5), "9.5");
        assert_eq!(si_label(950.0), "950");
        assert_eq!(si_label(12_345.0), "12k");
        assert_eq!(si_label(3_400_000.0), "3.4M");
        assert_eq!(span_label(SECOND * 45), "45s");
        assert_eq!(span_label(SECOND * 300), "5m");
        assert_eq!(span_label(SECOND * 90), "1m30s");
        assert_eq!(span_label(SECOND * 7200), "2h");
        assert_eq!(span_label(SECOND * 30 * 3600), "1d6h");
    }

    #[test]
    fn a_nan_breaks_the_line() {
        let points = [(-3.0, 1.0), (-2.0, f64::NAN), (-1.0, 2.0), (0.0, 3.0)];
        let borrowed = Trace::new("x", &points[..]);
        assert_eq!(borrowed.segments.len(), 2);
        assert!(
            borrowed
                .segments
                .iter()
                .all(|s| matches!(s, Cow::Borrowed(_)))
        );
        let owned = Trace::new("x", points.to_vec());
        assert_eq!(owned.segments.len(), 2);
        assert_eq!(owned.latest(), Some(3.0));
        let ends_in_gap = Trace::new("x", vec![(-1.0, 1.0), (0.0, f64::NAN)]);
        assert_eq!(ends_in_gap.latest(), Some(1.0));
        assert!(Trace::new("x", Vec::new()).segments.is_empty());
    }

    #[test]
    fn samples_end_now() {
        let t = Trace::samples("x", [1.0, 2.0, 3.0], SECOND * 2);
        assert_eq!(&*t.segments[0], &[(-4.0, 1.0), (-2.0, 2.0), (0.0, 3.0)]);
        assert_eq!(TimeChart::new([t]).x_from(), -4.0);
        assert_eq!(TimeChart::new([]).x_from(), -1.0);
    }

    #[test]
    fn the_scale_follows_what_is_in_view() {
        let points = [(-100.0, 1000.0), (-5.0, 10.0), (0.0, 20.0)];
        let trace = || Trace::new("x", &points[..]);
        assert_eq!(TimeChart::new([trace()]).y_bounds(), (0.0, 1100.0));
        let (_, top) = TimeChart::new([trace()]).span(SECOND * 10).y_bounds();
        assert!(
            (top - 22.0).abs() < 1e-9,
            "the old peak is out of view: {top}"
        );
        let nice = TimeChart::new([trace()]).span(SECOND * 10).nice(true);
        assert_eq!(nice.y_bounds(), (0.0, 50.0));
        let floor = TimeChart::new([trace()])
            .span(SECOND * 10)
            .y_min(None)
            .headroom(1.0);
        assert_eq!(floor.y_bounds(), (10.0, 20.0));
        assert_eq!(
            TimeChart::new([trace()]).y_max(100.0).y_bounds(),
            (0.0, 100.0)
        );
        // nothing, or a flat zero: still a range to draw in
        assert_eq!(TimeChart::new([]).y_bounds(), (0.0, 1.0));
        let flat = Trace::samples("x", [0.0, 0.0], SECOND);
        assert_eq!(TimeChart::new([flat]).y_bounds(), (0.0, 1.0));
    }

    #[test]
    fn draws_braille_with_labels_and_legend() {
        let rising: Vec<f64> = (0..60).map(f64::from).collect();
        let fmt = |v: f64| format!("{v:.0}%");
        let chart = TimeChart::new([
            Trace::samples("rise", rising, SECOND).style(Color::Cyan),
            Trace::new("", vec![(-30.0, 10.0)]), // a lone point
        ])
        .y_label(&fmt)
        .block(Block::bordered().title("load"));
        let rows = screen(chart, 50, 12);
        assert!(braille_cells(&rows) > 20, "{rows:#?}");
        assert!(
            rows[0].starts_with("┌load") && rows[0].ends_with(" rise 59% ┐"),
            "{}",
            rows[0]
        );
        let text = rows.join("\n");
        for label in ["-59s", "-29s", "now", "0%", "65%"] {
            assert!(text.contains(label), "no {label}: {rows:#?}");
        }
    }

    #[test]
    fn a_lone_point_shows() {
        let points = vec![(-3.0, 1.0), (-2.0, f64::NAN), (-1.0, 5.0), (0.0, f64::NAN)];
        let rows = screen(TimeChart::new([Trace::new("", points)]).x_labels(false), 20, 6);
        assert_eq!(braille_cells(&rows), 2, "{rows:#?}");
    }

    #[test]
    fn no_legend_without_names_or_when_off() {
        let named = || Trace::samples("rise", [1.0, 2.0], SECOND);
        let top = |chart: TimeChart| screen(chart.block(Block::bordered()), 30, 6).remove(0);
        assert!(!top(TimeChart::new([named()]).legend(false)).contains("rise"));
        assert!(!top(TimeChart::new([Trace::samples("", [1.0, 2.0], SECOND)])).contains('2'));
        assert!(top(TimeChart::new([named()])).contains(" rise 2 "));
    }
}
