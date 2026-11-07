// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
The bottom line of a TUI: [`StatusBar`] puts a status on the left, one on
the right and [`KeyHints`] in between; a [`Toast`] is a one-shot message
for it, and a [`Prompt`] an inline y/N question that takes the whole line
over. Merged from the apps' footers:
- flood's legend of bold keys, and its yellow confirm bar that any key but
  `y` cancels and that lapses on its own;
- squawk's hints or prompt on the left and error, notice or refresh age on
  the right, sized to its text;
- collector-status's hints on the right, zfs-browser's and nft-browser's
  busy pill, status and hints in one line, wiremeup's one-shot status.

None of them fitted the hints to the width: a narrow terminal cut the
legend mid-word, or clipped what came after it (flood moved PAUSED out of
its footer for that). Here hints drop out whole from the end with an `…`
in their place, the left status before them, and the right one last.
*/

use std::fmt::Display;
use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::logview::level_fns;
use crate::{Answer, LogLevel, confirm_answer};

/* ===== consts and statics ================================================= */

/**
How long a [`Prompt`] waits for its answer unless told otherwise (flood's
`CONFIRM_TTL`): a question left up must not catch a `y` typed minutes
later for something else.
*/
pub const PROMPT_TTL: Duration = Duration::from_secs(5);

static DEFAULT_SEPARATOR: &str = "  ";
static DEFAULT_PROMPT_HINT: &str = "y/N";
/// In place of the hints that didn't fit.
static MORE: &str = "…";
/// Columns kept clear on either side of the hints.
const HINTS_MARGIN: u16 = 1;

/* ===== types ============================================================== */

/**
A legend of `(key, what it does)` pairs, e.g. `("↑↓", "select")`, shown as
`↑↓ select  ⏎ drop  q quit`. Only whole hints are shown: those that don't
fit drop out from the end, an `…` in their place, so put the ones that
matter most (help, quit) first. A pair with an empty key is shown as its
text alone.
*/
#[derive(Debug, Clone)]
pub struct KeyHints<'a> {
    hints: &'a [(&'a str, &'a str)],
    key_style: Style,
    label_style: Style,
    separator: &'a str,
    alignment: Alignment,
}

/**
One line: `left`, then the [`KeyHints`] in what is left between it and
`right`, which is sized to its text. When the line is too narrow the hints
go first, then the end of `left`; `right` (an error, a refresh age) is cut
only when it alone is wider than the line. A [`prompt`](Self::prompt)
takes the whole line instead, so it can't be missed.
*/
#[derive(Debug, Clone, Default)]
pub struct StatusBar<'a> {
    left: Line<'a>,
    right: Line<'a>,
    hints: Option<KeyHints<'a>>,
    prompt: Option<Line<'a>>,
    style: Style,
}

/**
A one-shot message for the status line, styled by its [`LogLevel`]: the
result of the last action, or what went wrong. It stays until replaced or
[cleared](Self::clear) (wiremeup cleared it on the next key), or lapses
after its [`ttl`](Self::ttl).
*/
#[derive(Debug, Clone, Default)]
pub struct Toast {
    msg: Option<ToastMsg>,
    ttl: Option<Duration>,
}

#[derive(Debug, Clone)]
struct ToastMsg {
    level: LogLevel,
    text: String,
    since: Instant,
}

/**
An inline y/N question about `T` (flood's footer confirm, squawk's
`Pending`): [`ask`](Self::ask) puts it up, and the next key
[answers](Self::key) it. `y` goes ahead; any other key cancels, so the
key that asked (`q`, Enter) can't answer it by a double tap or a held-down
repeat, and neither can Enter on its way from a dialog before (squawk took
Enter as yes). Unanswered, it lapses after its [`ttl`](Self::ttl),
[`PROMPT_TTL`] by default.
*/
#[derive(Debug, Clone)]
pub struct Prompt<T> {
    asked: Option<Asked<T>>,
    ttl: Option<Duration>,
    hint: String,
    style: Style,
}

#[derive(Debug, Clone)]
struct Asked<T> {
    what: T,
    question: String,
    since: Instant,
}

/// What a key did to a [`Prompt`].
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply<T> {
    /// No question was up (or it had lapsed): the key is the app's.
    NotAsked,
    /// A key release or a repeat: the question stays up.
    Wait,
    /// `y`: go ahead.
    Yes(T),
    /// Any other key: the question is gone.
    No(T),
}

/* ===== KeyHints =========================================================== */

impl<'a> KeyHints<'a> {
    /// Keys in bold, what they do in the default style, two spaces apart.
    pub fn new(hints: &'a [(&'a str, &'a str)]) -> Self {
        Self {
            hints,
            key_style: Style::new().bold(),
            label_style: Style::new(),
            separator: DEFAULT_SEPARATOR,
            alignment: Alignment::Left,
        }
    }

    pub fn key_style(mut self, style: impl Into<Style>) -> Self {
        self.key_style = style.into();
        self
    }

    /// The style of what a key does, of the separators and of the `…`.
    pub fn label_style(mut self, style: impl Into<Style>) -> Self {
        self.label_style = style.into();
        self
    }

    /// Between two hints, e.g. `" · "`.
    pub fn separator(mut self, separator: &'a str) -> Self {
        self.separator = separator;
        self
    }

    /// Where the hints sit in their area (collector-status put them right).
    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Columns all the hints take.
    pub fn width(&self) -> usize {
        let sep = self.separator.width() * self.hints.len().saturating_sub(1);
        self.hints.iter().map(hint_width).sum::<usize>() + sep
    }

    /// The first hints that fit in `width` columns, and an `…` if some didn't.
    pub fn line(&self, width: usize) -> Line<'a> {
        let shown = self.fitting(width);
        let mut spans = Vec::with_capacity(shown * 4 + 2);
        for (i, &(key, label)) in self.hints[..shown].iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(self.separator, self.label_style));
            }
            if !key.is_empty() {
                spans.push(Span::styled(key, self.key_style));
            }
            if !key.is_empty() && !label.is_empty() {
                spans.push(Span::styled(" ", self.label_style));
            }
            if !label.is_empty() {
                spans.push(Span::styled(label, self.label_style));
            }
        }
        if shown < self.hints.len() {
            if shown > 0 {
                spans.push(Span::styled(" ", self.label_style));
            }
            spans.push(Span::styled(MORE, self.label_style));
        }
        Line::from(spans)
    }

    /// How many hints fit in `width`, leaving room for the `…` if not all.
    fn fitting(&self, width: usize) -> usize {
        let sep = self.separator.width();
        let mut used = 0;
        let mut fit = 0;
        for (i, hint) in self.hints.iter().enumerate() {
            used += hint_width(hint) + if i > 0 { sep } else { 0 };
            let last = i + 1 == self.hints.len();
            // " …" after any but the last hint
            let more = if last { 0 } else { 1 + MORE.width() };
            if used + more > width {
                break;
            }
            fit = i + 1;
        }
        fit
    }
}

impl Widget for KeyHints<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.line(area.width.into())
            .alignment(self.alignment)
            .render(area, buf);
    }
}

fn hint_width(&(key, label): &(&str, &str)) -> usize {
    let gap = usize::from(!key.is_empty() && !label.is_empty());
    key.width() + gap + label.width()
}

/* ===== StatusBar ========================================================== */

impl<'a> StatusBar<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// At the left edge: what the app is doing, a [`Toast`], a [`pill`].
    pub fn left(mut self, left: impl Into<Line<'a>>) -> Self {
        self.left = left.into();
        self
    }

    /// At the right edge, as wide as its text.
    pub fn right(mut self, right: impl Into<Line<'a>>) -> Self {
        self.right = right.into();
        self
    }

    /// In the room between the two, a column clear on either side.
    pub fn hints(mut self, hints: KeyHints<'a>) -> Self {
        self.hints = Some(hints);
        self
    }

    /// Shown instead of everything else, its style across the whole line.
    pub fn prompt(mut self, prompt: Option<Line<'a>>) -> Self {
        self.prompt = prompt;
        self
    }

    /// The whole line's style, under the parts' own (wiremeup's reversed bar).
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Rect {
            height: area.height.min(1),
            ..area
        };
        if area.is_empty() {
            return;
        }
        if let Some(prompt) = self.prompt {
            buf.set_style(area, prompt.style);
            prompt.render(area, buf);
            return;
        }
        buf.set_style(area, self.style);
        let right_w = clamp_width(self.right.width(), area.width);
        let rest = Rect {
            width: area.width - right_w,
            ..area
        };
        let right = Rect {
            x: rest.right(),
            width: right_w,
            ..area
        };
        self.right.render(right, buf);
        let left_w = clamp_width(self.left.width(), rest.width);
        self.left.render(
            Rect {
                width: left_w,
                ..rest
            },
            buf,
        );
        if let Some(hints) = self.hints {
            let room = rest.width - left_w;
            let hints_area = Rect {
                x: rest.x + left_w + HINTS_MARGIN.min(room),
                width: room.saturating_sub(2 * HINTS_MARGIN),
                ..rest
            };
            if !hints_area.is_empty() {
                hints.render(hints_area, buf);
            }
        }
    }
}

fn clamp_width(width: usize, max: u16) -> u16 {
    u16::try_from(width).unwrap_or(u16::MAX).min(max)
}

/**
`text` with a space either side, in `style`: a marker that stands out in
a status line, like zfs-browser's ` ⏳ working… ` or flood's ` FROZEN `.
*/
pub fn pill(text: impl Display, style: impl Into<Style>) -> Span<'static> {
    Span::styled(format!(" {text} "), style.into())
}

/* ===== Toast ============================================================== */

impl Toast {
    /// Messages stay until replaced or cleared.
    pub fn new() -> Self {
        Self::default()
    }

    /// Messages lapse `ttl` after they are shown; `None` keeps them.
    pub fn ttl(mut self, ttl: impl Into<Option<Duration>>) -> Self {
        self.ttl = ttl.into();
        self
    }

    /// Show `msg`, replacing the one before.
    pub fn show(&mut self, level: LogLevel, msg: impl Into<String>) {
        self.msg = Some(ToastMsg {
            level,
            text: msg.into(),
            since: Instant::now(),
        });
    }

    level_fns!(&mut self show;
        error => Error,
        warn => Warn,
        notice => Notice,
        info => Info,
    );

    pub fn clear(&mut self) {
        self.msg = None;
    }

    /// The message shown, unless it has lapsed.
    pub fn get(&self) -> Option<(LogLevel, &str)> {
        let msg = self.msg.as_ref().filter(|m| alive(m.since, self.ttl))?;
        Some((msg.level, &msg.text))
    }

    /// Until the message lapses, for the event loop's poll timeout.
    pub fn remaining(&self) -> Option<Duration> {
        remaining(self.msg.as_ref()?.since, self.ttl)
    }

    /// The message with a space either side, in its level's style.
    pub fn line(&self) -> Option<Line<'_>> {
        let (level, text) = self.get()?;
        Some(Line::from(pill(text, level.style())))
    }
}

/* ===== Prompt ============================================================= */

impl<T> Default for Prompt<T> {
    fn default() -> Self {
        Self {
            asked: None,
            ttl: Some(PROMPT_TTL),
            hint: DEFAULT_PROMPT_HINT.into(),
            style: Style::new().fg(Color::Black).bg(Color::Yellow).bold(),
        }
    }
}

impl<T> Prompt<T> {
    /// Black on yellow, `y/N` after the question, lapsing after [`PROMPT_TTL`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Questions lapse `ttl` after they are asked; `None` keeps them up.
    pub fn ttl(mut self, ttl: impl Into<Option<Duration>>) -> Self {
        self.ttl = ttl.into();
        self
    }

    /// After the question, e.g. `"y = quit · any other key cancels"`.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// Put up `question` about `what`, replacing any question before.
    pub fn ask(&mut self, what: T, question: impl Into<String>) {
        self.asked = Some(Asked {
            what,
            question: question.into(),
            since: Instant::now(),
        });
    }

    /// Take the question down unanswered.
    pub fn cancel(&mut self) -> Option<T> {
        self.asked.take().map(|a| a.what)
    }

    /// What the question up is about, unless it has lapsed.
    pub fn pending(&self) -> Option<&T> {
        self.live().map(|a| &a.what)
    }

    /// Until the question lapses, for the event loop's poll timeout.
    pub fn remaining(&self) -> Option<Duration> {
        remaining(self.asked.as_ref()?.since, self.ttl)
    }

    /**
    Answer the question with `key`, if one is up: `y` without Ctrl or Alt
    is yes, any other key press no. Call it before the app's own keys and
    let them have the key only on [`Reply::NotAsked`].
    */
    pub fn key(&mut self, key: KeyEvent) -> Reply<T> {
        if self.live().is_none() {
            self.asked = None;
            return Reply::NotAsked;
        }
        if key.kind != KeyEventKind::Press {
            return Reply::Wait;
        }
        let Some(asked) = self.asked.take() else {
            return Reply::NotAsked;
        };
        match confirm_answer(key) {
            Some(Answer::Yes) => Reply::Yes(asked.what),
            _ => Reply::No(asked.what),
        }
    }

    /// The question and the hint in the prompt's style, for [`StatusBar::prompt`].
    pub fn line(&self) -> Option<Line<'static>> {
        let asked = self.live()?;
        let text = match self.hint.is_empty() {
            true => format!(" {} ", asked.question),
            false => format!(" {} {} ", asked.question, self.hint),
        };
        Some(Line::styled(text, self.style))
    }

    fn live(&self) -> Option<&Asked<T>> {
        self.asked.as_ref().filter(|a| alive(a.since, self.ttl))
    }
}

/* ===== helpers ============================================================ */

fn alive(since: Instant, ttl: Option<Duration>) -> bool {
    ttl.is_none_or(|ttl| since.elapsed() < ttl)
}

fn remaining(since: Instant, ttl: Option<Duration>) -> Option<Duration> {
    ttl.map(|ttl| ttl.saturating_sub(since.elapsed()))
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyModifiers;

    use crate::{buffer_text, render_buffer};

    static HINTS: &[(&str, &str)] = &[("h", "help"), ("↑↓", "select"), ("q", "quit")];
    const LONG_AGO: Duration = Duration::from_secs(3600);

    fn screen(widget: impl Widget, w: u16) -> String {
        buffer_text(&render_buffer(widget, w, 1))
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn lapse<T>(prompt: &mut Prompt<T>) {
        prompt.asked.as_mut().unwrap().since = Instant::now() - LONG_AGO;
    }

    #[test]
    fn hints_drop_out_whole_from_the_end() {
        let hints = KeyHints::new(HINTS);
        assert_eq!(hints.width(), "h help  ↑↓ select  q quit".width());
        assert_eq!(
            hints.line(hints.width()).to_string(),
            "h help  ↑↓ select  q quit"
        );
        // one column short: the last two go, "…" fits after the first
        assert_eq!(
            hints.line(hints.width() - 1).to_string(),
            "h help  ↑↓ select …"
        );
        assert_eq!(hints.line(8).to_string(), "h help …");
        assert_eq!(hints.line(7).to_string(), "…");
        assert_eq!(hints.line(0).to_string(), "…");
    }

    #[test]
    fn hints_measure_display_columns() {
        let wide: &[(&str, &str)] = &[("⏎", "選択"), ("", "note")];
        let hints = KeyHints::new(wide).separator(" · ");
        assert_eq!(hints.width(), 1 + 1 + 4 + 3 + 4);
        assert_eq!(hints.line(hints.width()).to_string(), "⏎ 選択 · note");
    }

    #[test]
    fn the_bar_gives_way_hints_first_and_right_last() {
        let bar = || {
            StatusBar::new()
                .left(" ok ")
                .right(" 3s ago ")
                .hints(KeyHints::new(HINTS))
        };
        assert_eq!(
            screen(bar(), 40),
            " ok  h help  ↑↓ select  q quit   3s ago "
        );
        assert_eq!(screen(bar(), 26), " ok  h help …      3s ago ");
        // no room for hints, the left cut, the right whole
        assert_eq!(screen(bar(), 10), " o 3s ago ");
        assert_eq!(screen(bar(), 6), " 3s ag");
    }

    #[test]
    fn hints_can_sit_right() {
        let hints = KeyHints::new(&HINTS[..1]).alignment(Alignment::Right);
        assert_eq!(
            screen(StatusBar::new().left("x").hints(hints), 12),
            "x    h help "
        );
    }

    #[test]
    fn a_prompt_takes_the_whole_line() {
        let mut prompt = Prompt::new();
        prompt.ask((), "quit?");
        let bar = StatusBar::new()
            .left("left")
            .hints(KeyHints::new(HINTS))
            .prompt(prompt.line());
        let buf = render_buffer(bar, 16, 1);
        assert_eq!(buffer_text(&buf), " quit? y/N      ");
        assert_eq!(
            buf.content[15].bg,
            Color::Yellow,
            "the style runs to the end"
        );
    }

    #[test]
    fn y_answers_and_anything_else_cancels() {
        let mut prompt = Prompt::new();
        assert_eq!(prompt.key(press(KeyCode::Char('y'))), Reply::NotAsked);
        prompt.ask(1, "one?");
        assert_eq!(prompt.key(press(KeyCode::Char('Y'))), Reply::Yes(1));
        assert_eq!(prompt.pending(), None);
        prompt.ask(2, "two?");
        assert_eq!(prompt.key(press(KeyCode::Enter)), Reply::No(2));
        prompt.ask(3, "three?");
        let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(prompt.key(ctrl_y), Reply::No(3));
    }

    #[test]
    fn a_release_or_repeat_leaves_the_question_up() {
        let mut prompt = Prompt::new();
        prompt.ask("q", "quit?");
        let mut release = press(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        assert_eq!(prompt.key(release), Reply::Wait);
        let mut repeat = press(KeyCode::Char('q'));
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(prompt.key(repeat), Reply::Wait);
        assert_eq!(prompt.pending(), Some(&"q"));
    }

    #[test]
    fn an_unanswered_question_lapses() {
        let mut prompt = Prompt::new();
        prompt.ask((), "quit?");
        assert!(prompt.remaining().is_some_and(|r| r <= PROMPT_TTL));
        lapse(&mut prompt);
        assert_eq!(prompt.pending(), None);
        assert!(prompt.line().is_none());
        assert_eq!(prompt.remaining(), Some(Duration::ZERO));
        assert_eq!(prompt.key(press(KeyCode::Char('y'))), Reply::NotAsked);
        assert_eq!(prompt.remaining(), None, "a late key takes it down");

        let mut forever = Prompt::new().ttl(None);
        forever.ask((), "quit?");
        lapse(&mut forever);
        assert_eq!(forever.key(press(KeyCode::Char('y'))), Reply::Yes(()));
    }

    #[test]
    fn a_toast_shows_until_replaced_or_lapsed() {
        let mut toast = Toast::new();
        assert!(toast.line().is_none());
        toast.info("saved");
        toast.error("write failed");
        assert_eq!(toast.get(), Some((LogLevel::Error, "write failed")));
        assert_eq!(toast.line().unwrap().to_string(), " write failed ");
        assert_eq!(toast.remaining(), None);
        toast.msg.as_mut().unwrap().since = Instant::now() - LONG_AGO;
        assert!(toast.get().is_some(), "no ttl: it stays");

        let mut toast = Toast::new().ttl(Duration::from_secs(5));
        toast.notice("done");
        assert!(toast.get().is_some());
        toast.msg.as_mut().unwrap().since = Instant::now() - LONG_AGO;
        assert_eq!(toast.get(), None);
        toast.clear();
        assert_eq!(toast.remaining(), None);
    }
}
