// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A log pane: [`LogBuffer`], a bounded ring of recent messages that any thread
can push to, and [`LogView`], which shows its newest lines and follows new
ones. Merged from edge-xdp-flood's EVENTS pane and mping-rs's log popup:
- both kept the last N messages behind a lock, each with its level and
  time, and styled a message by its level;
- both echoed a message to the shell while the TUI wasn't up, so what
  happened during setup stays visible; flood echoed on stdout, the stream
  the TUI writes to itself (see [`Echo::Stdout`]);
- flood fed its ring from the `log` crate ([`LogSink`], behind the `log`
  feature here);
- mping counted every message ever pushed (for its line cache). Here the
  count keeps a scrolled-up view on the lines it shows while new ones
  arrive and old ones drop out; flood's view, counted back from the
  newest, slid under the reader with every new line.
*/

use std::collections::VecDeque;
use std::fmt;
use std::io::{Write, stderr, stdout};
use std::ops::Range;
#[cfg(feature = "log")]
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyEvent, KeyModifiers};
#[cfg(feature = "log")]
use log::{LevelFilter, Log, Metadata, Record, SetLoggerError};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, StatefulWidget, Widget};

use crate::scroll::Chrome;
use crate::terminal::lock;
use crate::{NavOutcome, Scroll, tui_active};

/* ===== consts, statics and macros ========================================= */

pub(crate) const SECS_PER_DAY: u64 = 86_400;
pub(crate) const SECS_PER_HOUR: u64 = 3_600;
pub(crate) const SECS_PER_MIN: u64 = 60;

/**
`LogBuffer::error(msg)` and friends: `msg` at that level, through the
method named first (`push` there, `show` on a [`Toast`](crate::Toast)).
*/
macro_rules! level_fns {
    (&self $method:ident; $($name:ident => $level:ident),+ $(,)?) => {
        $(
            #[doc = concat!(
                "[`Self::", stringify!($method), "`] `msg` at ",
                "[`LogLevel::", stringify!($level), "`]."
            )]
            pub fn $name(&self, msg: impl Into<String>) {
                self.$method(LogLevel::$level, msg);
            }
        )+
    };
    (&mut self $method:ident; $($name:ident => $level:ident),+ $(,)?) => {
        $(
            #[doc = concat!(
                "[`Self::", stringify!($method), "`] `msg` at ",
                "[`LogLevel::", stringify!($level), "`]."
            )]
            pub fn $name(&mut self, msg: impl Into<String>) {
                self.$method(LogLevel::$level, msg);
            }
        )+
    };
}
pub(crate) use level_fns;

/* ===== types ============================================================== */

/**
How much a message matters, most severe first (so `level <= LogLevel::Warn`
is "a warning or worse"). The syslog levels apps use, plus trace; the `log`
crate's five map onto them.
*/
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Critical,
    Error,
    Warn,
    Notice,
    #[default]
    Info,
    Debug,
    Trace,
}

/// One message in a [`LogBuffer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub when: SystemTime,
    pub level: LogLevel,
    pub msg: String,
}

/// Where a [`LogBuffer`] echoes a message while the TUI isn't up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Echo {
    #[default]
    Off,
    /**
    On stdout, flushed line by line: the stream the TUI draws on, so an
    echo can't land on the alternate screen. On stderr it can, where the
    two streams don't reach the terminal in program order: piped through
    separate buffers, or as separate channels over SSH (flood's finding).
    */
    Stdout,
    /// On stderr: for an app whose stdout is redirected, or whose shell
    /// output is all on stderr.
    Stderr,
}

/// How a [`LogView`] shows a message's time, in UTC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stamp {
    None,
    /// `12:34:56`
    Time,
    /// `2026-11-06 12:34:56`: unambiguous over a run of several days.
    #[default]
    DateTime,
}

/**
The last `capacity` messages, oldest first, for a [`LogView`]: shared
between threads (behind an `Arc`, or a `static` built in a `LazyLock`),
pushed to from anywhere. Optionally echoes each message to the shell while
the TUI isn't up, see [`LogBuffer::echo`].

```
use minitui::{Echo, LogBuffer, LogLevel};

let log = LogBuffer::new(1000).echo(Echo::Stdout, LogLevel::Notice);
log.info("listening on :8080");
log.push(LogLevel::Warn, format!("{} retries", 3));
assert_eq!(log.len(), 2);
```
*/
#[derive(Debug)]
pub struct LogBuffer {
    ring: Mutex<Ring>,
    echo: Echo,
    /// Echo messages at this level or more severe.
    echo_level: LogLevel,
}

#[derive(Debug)]
struct Ring {
    entries: VecDeque<LogEntry>,
    capacity: usize,
    /// Messages ever pushed (and not cleared): the sequence number of the next.
    total: u64,
}

/**
A [`LogView`]'s scroll position: following the newest line until moved up,
then staying on the lines it shows while new ones arrive (and older ones
drop out of the buffer). End, or ↓ back to the bottom, follows again.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogScroll {
    scroll: Scroll,
    follow: bool,
    /// The sequence number of the buffer's oldest message on the last draw.
    first: u64,
}

/**
The tail of a [`LogBuffer`], rendered through a [`LogScroll`] (as a
`StatefulWidget`): one line per message (`time LEVEL: message`, styled by
level), cut at the right edge. Only the lines in view are formatted. While
scrolled up, the bottom border says which lines are in view.

```no_run
# use ratatui::Frame;
# use ratatui::widgets::Block;
# use crossterm::event::KeyEvent;
use minitui::{LogBuffer, LogScroll, LogView, Stamp};

# fn demo(frame: &mut Frame, key: KeyEvent, log: &LogBuffer, scroll: &mut LogScroll) {
scroll.key(key); // ↑ ↓ PgUp PgDn Home End; End follows again
let view = LogView::new(log).stamp(Stamp::Time).block(Block::bordered().title(" log "));
frame.render_stateful_widget(view, frame.area(), scroll);
# }
```
*/
#[must_use]
pub struct LogView<'a> {
    log: &'a LogBuffer,
    stamp: Stamp,
    levels: bool,
    format: Option<&'a dyn Fn(&LogEntry) -> Line<'static>>,
    chrome: Chrome<'a>,
}

/**
A `log::Log` backend feeding a [`LogBuffer`] (the `log` feature): records at
`level` or more severe, their text only (no target).

```no_run
# #[cfg(feature = "log")] {
use std::sync::Arc;
use log::LevelFilter;
use minitui::{Echo, LogBuffer, LogLevel, LogSink};

let events = Arc::new(LogBuffer::new(2000).echo(Echo::Stdout, LogLevel::Info));
LogSink::new(events.clone(), LevelFilter::Info).install().expect("the only logger");
log::warn!("policy map 90% full");
# }
```
*/
#[cfg(feature = "log")]
#[derive(Debug)]
pub struct LogSink {
    buffer: Arc<LogBuffer>,
    level: LevelFilter,
}

/* ===== LogLevel =========================================================== */

impl LogLevel {
    /// `CRIT`, `ERROR`, `WARN`, `NOTE`, `INFO`, `DEBUG`, `TRACE`.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Critical => "CRIT",
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Notice => "NOTE",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// The style of a message at this level: red for errors, yellow for
    /// warnings, cyan for notices, plain for info, dim below.
    pub fn style(self) -> Style {
        match self {
            Self::Critical => Style::new().bold().on_light_red(),
            Self::Error => Style::new().bold().light_red(),
            Self::Warn => Style::new().light_yellow(),
            Self::Notice => Style::new().cyan(),
            Self::Info => Style::new(),
            Self::Debug => Style::new().dim(),
            Self::Trace => Style::new().dim().italic(),
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

#[cfg(feature = "log")]
impl From<log::Level> for LogLevel {
    fn from(level: log::Level) -> Self {
        match level {
            log::Level::Error => Self::Error,
            log::Level::Warn => Self::Warn,
            log::Level::Info => Self::Info,
            log::Level::Debug => Self::Debug,
            log::Level::Trace => Self::Trace,
        }
    }
}

/* ===== LogEntry =========================================================== */

impl LogEntry {
    /// A message from now.
    pub fn new(level: LogLevel, msg: impl Into<String>) -> Self {
        Self {
            when: SystemTime::now(),
            level,
            msg: msg.into(),
        }
    }

    /// `time LEVEL: message`: the time dim, the message in its level's style;
    /// without the `LEVEL: ` unless `levels`.
    pub fn line(&self, stamp: Stamp, levels: bool) -> Line<'static> {
        let mut spans = Vec::with_capacity(3);
        if let Some(time) = stamp.format(self.when) {
            spans.push(Span::styled(format!("{time} "), Style::new().dim()));
        }
        if levels {
            spans.push(Span::raw(format!("{}: ", self.level)));
        }
        spans.push(Span::styled(self.msg.clone(), self.level.style()));
        Line::from(spans)
    }
}

/// `2026-11-06 12:34:56 WARN: message`, as echoed.
impl fmt::Display for LogEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let time = Stamp::DateTime.format(self.when).unwrap_or_default();
        write!(f, "{time} {}: {}", self.level, self.msg)
    }
}

/* ===== Stamp ============================================================== */

impl Stamp {
    /// `when` in this form, in UTC; `None` for [`Stamp::None`].
    pub fn format(self, when: SystemTime) -> Option<String> {
        let secs = when.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let (h, m, s) = (
            secs % SECS_PER_DAY / SECS_PER_HOUR,
            secs % SECS_PER_HOUR / SECS_PER_MIN,
            secs % SECS_PER_MIN,
        );
        match self {
            Self::None => None,
            Self::Time => Some(format!("{h:02}:{m:02}:{s:02}")),
            Self::DateTime => {
                let (year, month, day) = civil_from_days(secs / SECS_PER_DAY);
                Some(format!(
                    "{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}"
                ))
            }
        }
    }
}

/**
Days since the Unix epoch to a (year, month, day) in the proleptic Gregorian
calendar: Howard Hinnant's `civil_from_days` (from flood), for days on or
after the epoch. Exact, and no date crate for one timestamp format.
*/
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], from March
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + u64::from(month <= 2), month, day)
}

/* ===== LogBuffer ========================================================== */

impl LogBuffer {
    /// Room for the last `capacity` messages (at least one).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            ring: Mutex::new(Ring {
                entries: VecDeque::with_capacity(capacity),
                capacity,
                total: 0,
            }),
            echo: Echo::Off,
            echo_level: LogLevel::Info,
        }
    }

    /**
    Also write each message at `level` or more severe to the shell, as
    `2026-11-06 12:34:56 WARN: message`, while the TUI isn't up: during
    setup, and after the guard has restored the terminal.
    */
    pub fn echo(mut self, to: Echo, level: LogLevel) -> Self {
        (self.echo, self.echo_level) = (to, level);
        self
    }

    /// Add a message; the oldest drops out when the buffer is full.
    pub fn push(&self, level: LogLevel, msg: impl Into<String>) {
        let entry = LogEntry::new(level, msg);
        // outside the lock: a blocked stream mustn't stall the other threads
        if level <= self.echo_level && !tui_active() {
            self.echo_entry(&entry);
        }
        let mut ring = lock(&self.ring);
        if ring.entries.len() == ring.capacity {
            ring.entries.pop_front();
        }
        ring.entries.push_back(entry);
        ring.total += 1;
    }

    level_fns!(&self push;
        critical => Critical,
        error => Error,
        warn => Warn,
        notice => Notice,
        info => Info,
        debug => Debug,
        trace => Trace,
    );

    /// Messages held now.
    pub fn len(&self) -> usize {
        lock(&self.ring).entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        lock(&self.ring).capacity
    }

    /// Messages ever pushed, those dropped out included: changes when one arrives.
    pub fn total(&self) -> u64 {
        lock(&self.ring).total
    }

    /// Drop every message.
    pub fn clear(&self) {
        lock(&self.ring).entries.clear();
    }

    /// Read the messages, oldest first, under the lock: keep `f` short.
    pub fn with<R>(&self, f: impl FnOnce(&VecDeque<LogEntry>) -> R) -> R {
        f(&lock(&self.ring).entries)
    }

    fn echo_entry(&self, entry: &LogEntry) {
        // a write and a flush per line: the TUI may claim the terminal next
        let _ = match self.echo {
            Echo::Off => Ok(()),
            Echo::Stdout => {
                let mut out = stdout().lock();
                writeln!(out, "{entry}").and_then(|()| out.flush())
            }
            Echo::Stderr => writeln!(stderr().lock(), "{entry}"),
        };
    }
}

/* ===== LogScroll ========================================================== */

impl Default for LogScroll {
    fn default() -> Self {
        Self {
            scroll: Scroll::new(),
            follow: true,
            first: 0,
        }
    }
}

impl LogScroll {
    /// Following the newest line.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the view follows the newest line (else it stays where it was moved).
    pub fn following(&self) -> bool {
        self.follow
    }

    /// Back to the newest line, following it.
    pub fn follow(&mut self) {
        self.follow = true;
    }

    /**
    One key: ↑ ↓ Home End PgUp PgDn, without modifiers, as
    [`Scroll::key`]. A move up stops following; one that reaches the
    bottom (End, or ↓ / PgDn onto it) follows again.
    */
    pub fn key(&mut self, key: KeyEvent) -> NavOutcome {
        self.key_with(key, KeyModifiers::NONE)
    }

    /// As [`LogScroll::key`], with exactly `modifiers` held (see [`Scroll::key_with`]).
    pub fn key_with(&mut self, key: KeyEvent, modifiers: KeyModifiers) -> NavOutcome {
        let outcome = self.scroll.key_with(key, modifiers);
        if outcome != NavOutcome::Ignored {
            self.follow = self.scroll.at_bottom();
        }
        outcome
    }

    /**
    The messages of a buffer holding `len` from sequence number `first`
    on, to show in `rows`: the newest when following, else the same ones
    as last time, as far as they are still held.
    */
    fn window(&mut self, first: u64, len: usize, rows: usize) -> Range<usize> {
        let offset = match self.follow {
            true => usize::MAX,
            false => {
                let dropped = first.saturating_sub(self.first);
                let dropped = usize::try_from(dropped).unwrap_or(usize::MAX);
                self.scroll.offset().saturating_sub(dropped)
            }
        };
        self.scroll.set_offset(offset);
        self.first = first;
        self.scroll.window(len, rows)
    }
}

/* ===== LogView ============================================================ */

impl<'a> LogView<'a> {
    pub fn new(log: &'a LogBuffer) -> Self {
        Self {
            log,
            stamp: Stamp::default(),
            levels: true,
            format: None,
            chrome: Chrome::default(),
        }
    }

    /// How the time shows ([`Stamp::DateTime`] by default).
    pub fn stamp(mut self, stamp: Stamp) -> Self {
        self.stamp = stamp;
        self
    }

    /// Whether the `LEVEL: ` tag shows (it does by default; the color tells
    /// the level either way).
    pub fn levels(mut self, levels: bool) -> Self {
        self.levels = levels;
        self
    }

    /// Format each message with `format` instead of [`LogEntry::line`].
    pub fn format(mut self, format: &'a dyn Fn(&LogEntry) -> Line<'static>) -> Self {
        self.format = Some(format);
        self
    }

    /// The block around the log; the position indicator goes in its bottom border.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.chrome.block = Some(block);
        self
    }

    /// What the position indicator counts (`lines`, the default); `None` for no
    /// indicator. It shows only while scrolled up from the newest.
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

impl StatefulWidget for LogView<'_> {
    type State = LogScroll;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut LogScroll) {
        let Self {
            log,
            stamp,
            levels,
            format,
            mut chrome,
        } = self;
        let inner = chrome.inner(area);
        let lines: Vec<Line> = {
            let ring = lock(&log.ring);
            let len = ring.entries.len();
            let first = ring.total.saturating_sub(len as u64);
            let shown = state.window(first, len, inner.height.into());
            ring.entries
                .range(shown)
                .map(|e| format.map_or_else(|| e.line(stamp, levels), |f| f(e)))
                .collect()
        };
        if state.follow {
            chrome.what = None;
        }
        chrome.render(area, buf, &state.scroll);
        Paragraph::new(lines).render(inner, buf);
    }
}

/* ===== LogSink ============================================================ */

#[cfg(feature = "log")]
impl LogSink {
    pub fn new(buffer: Arc<LogBuffer>, level: LevelFilter) -> Self {
        Self { buffer, level }
    }

    /// Make this the process's `log` backend, at its level.
    pub fn install(self) -> Result<(), SetLoggerError> {
        let level = self.level;
        log::set_boxed_logger(Box::new(self))?;
        log::set_max_level(level);
        Ok(())
    }
}

#[cfg(feature = "log")]
impl Log for LogSink {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.buffer
                .push(record.level().into(), record.args().to_string());
        }
    }

    // each echo is flushed as written; the buffer itself is memory
    fn flush(&self) {}
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use std::time::Duration;

    fn numbered(capacity: usize, n: usize) -> LogBuffer {
        let log = LogBuffer::new(capacity);
        for i in 0..n {
            log.info(format!("m{i}"));
        }
        log
    }

    /// The messages shown in a borderless pane `rows` tall, without time or level.
    fn shown(log: &LogBuffer, scroll: &mut LogScroll, rows: u16) -> Vec<String> {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, rows));
        LogView::new(log)
            .stamp(Stamp::None)
            .levels(false)
            .render(buf.area, &mut buf, scroll);
        (0..rows)
            .map(|y| {
                (0..8)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn the_oldest_drops_out() {
        let log = numbered(3, 5);
        assert_eq!(log.len(), 3);
        assert_eq!(log.total(), 5);
        let msgs = log.with(|e| e.iter().map(|e| e.msg.clone()).collect::<Vec<_>>());
        assert_eq!(msgs, ["m2", "m3", "m4"]);
        log.clear();
        assert!(log.is_empty());
        assert_eq!(LogBuffer::new(0).capacity(), 1);
    }

    #[test]
    fn levels_order_by_severity() {
        assert!(LogLevel::Critical < LogLevel::Error);
        assert!(LogLevel::Warn <= LogLevel::Warn);
        assert!(LogLevel::Debug > LogLevel::Info);
        assert_eq!(LogLevel::Notice.to_string(), "NOTE");
    }

    #[test]
    fn stamps_in_utc() {
        assert_eq!(
            Stamp::DateTime.format(at(0)).as_deref(),
            Some("1970-01-01 00:00:00")
        );
        // 2000-02-29 (a leap day) 13:14:15
        let leap = 11_016 * SECS_PER_DAY + 13 * SECS_PER_HOUR + 14 * SECS_PER_MIN + 15;
        assert_eq!(
            Stamp::DateTime.format(at(leap)).as_deref(),
            Some("2000-02-29 13:14:15")
        );
        assert_eq!(Stamp::Time.format(at(leap)).as_deref(), Some("13:14:15"));
        assert_eq!(Stamp::None.format(at(leap)), None);
    }

    #[test]
    fn civil_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_818), (2026, 12, 31));
        assert_eq!(civil_from_days(20_819), (2027, 1, 1));
    }

    #[test]
    fn a_line_has_time_level_and_message() {
        let entry = LogEntry {
            when: at(0),
            level: LogLevel::Warn,
            msg: "full".into(),
        };
        assert_eq!(
            entry.line(Stamp::Time, true).to_string(),
            "00:00:00 WARN: full"
        );
        assert_eq!(entry.line(Stamp::None, false).to_string(), "full");
        let line = entry.line(Stamp::None, false);
        assert_eq!(line.spans[0].style, LogLevel::Warn.style());
        assert_eq!(entry.to_string(), "1970-01-01 00:00:00 WARN: full");
    }

    #[test]
    fn follows_the_newest() {
        let log = numbered(100, 10);
        let mut scroll = LogScroll::new();
        assert_eq!(shown(&log, &mut scroll, 3), ["m7", "m8", "m9"]);
        log.info("m10");
        assert_eq!(shown(&log, &mut scroll, 3), ["m8", "m9", "m10"]);
    }

    #[test]
    fn scrolled_up_it_stays_on_its_lines() {
        let log = numbered(100, 10);
        let mut scroll = LogScroll::new();
        shown(&log, &mut scroll, 3);
        assert_eq!(scroll.key(KeyEvent::from(KeyCode::Up)), NavOutcome::Moved);
        assert!(!scroll.following());
        log.info("m10");
        assert_eq!(shown(&log, &mut scroll, 3), ["m6", "m7", "m8"]);
        // ↓ ↓ ↓ back onto the bottom follows again
        for _ in 0..3 {
            scroll.key(KeyEvent::from(KeyCode::Down));
        }
        assert!(scroll.following());
        log.info("m11");
        assert_eq!(shown(&log, &mut scroll, 3), ["m9", "m10", "m11"]);
    }

    #[test]
    fn scrolled_up_it_stays_while_old_lines_drop_out() {
        let log = numbered(10, 10);
        let mut scroll = LogScroll::new();
        shown(&log, &mut scroll, 3);
        scroll.key(KeyEvent::from(KeyCode::PageUp));
        assert_eq!(shown(&log, &mut scroll, 3), ["m5", "m6", "m7"]);
        for i in 10..13 {
            log.info(format!("m{i}"));
        }
        assert_eq!(shown(&log, &mut scroll, 3), ["m5", "m6", "m7"]);
        // once its lines are gone, it is at the oldest
        for i in 13..20 {
            log.info(format!("m{i}"));
        }
        assert_eq!(shown(&log, &mut scroll, 3), ["m10", "m11", "m12"]);
        scroll.key(KeyEvent::from(KeyCode::End));
        assert!(scroll.following());
    }

    #[test]
    fn the_indicator_only_while_scrolled_up() {
        let log = numbered(100, 20);
        let mut scroll = LogScroll::new();
        let area = Rect::new(0, 0, 30, 5);
        let bottom = |scroll: &mut LogScroll| {
            let mut buf = Buffer::empty(area);
            LogView::new(&log)
                .block(Block::bordered())
                .render(area, &mut buf, scroll);
            (0..30).map(|x| buf[(x, 4)].symbol()).collect::<String>()
        };
        assert!(!bottom(&mut scroll).contains("lines"));
        scroll.key(KeyEvent::from(KeyCode::Home));
        assert!(bottom(&mut scroll).contains(" lines 1–3 of 20 "));
    }

    #[cfg(feature = "log")]
    #[test]
    fn the_log_sink_pushes_at_its_level() {
        let buffer = Arc::new(LogBuffer::new(10));
        let sink = LogSink::new(buffer.clone(), LevelFilter::Info);
        sink.log(
            &Record::builder()
                .level(log::Level::Warn)
                .args(format_args!("hot"))
                .build(),
        );
        sink.log(
            &Record::builder()
                .level(log::Level::Debug)
                .args(format_args!("noise"))
                .build(),
        );
        let held = buffer.with(|e| {
            e.iter()
                .map(|e| (e.level, e.msg.clone()))
                .collect::<Vec<_>>()
        });
        assert_eq!(held, [(LogLevel::Warn, "hot".to_string())]);
    }
}
