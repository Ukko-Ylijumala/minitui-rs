// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Small shared building blocks for ratatui + crossterm TUIs: the parts every
app needs but none of them is about. Not a framework — there is no app trait
and no run loop; each app keeps its own event loop and draws its own views.

- [`TerminalGuard`]: raw mode + alternate screen (+ optional kitty keyboard
  protocol) for the guard's lifetime, restored on drop, on a failed setup
  and from a panic hook ([`PanicPolicy`] picks which threads' panics may
  tear the terminal down; the others are held, see [`held_panics`]).
- [`tui_active`], [`eprintln_safe()`] / [`eprintln_safe!`]: keep stray stderr
  output (and log echoes) off the TUI while it is up.
- [`GuardBuilder::capture_stderr`]: or let it all be written: stderr itself
  goes into a [`LogBuffer`] while the guard is alive, whoever writes to it,
  and is the shell's again before a panic report or a last word is printed;
  [`LogLevel::of_line`] reads a line's level off its first word.
- [`SignalHandler`]: SIGINT / SIGTERM / SIGHUP / SIGQUIT go to the app's
  quit path; a repeat (or no quit path at all) restores the terminal and
  terminates, so a `kill` never leaves the shell in raw mode.
- [`HelpPopup`]: a help overlay sized to its text (or a key / description
  table), centered, capped to the screen, scrollable.
- [`wrap_lines`], [`ellipsize_start`], [`ellipsize_end`], [`range_label`]:
  measuring and fitting text in display columns.
- [`LineEditor`]: a single-line text editor with readline keys, scrolling
  by display width, optional masking.
- [`InputDialog`], [`ConfirmDialog`], [`confirm_answer`]: modal prompts
  around the app's own state.
- [`ListCursor`]: selection and scroll offset of a list or table, kept
  across frames; keys, paging by the last draw, the visible window.
- [`Form`]: a modal form of text, checkbox and choice [`Field`]s, with
  focus, optional buttons, a note and the validation error.
- [`Scroll`], [`TextView`], [`CanvasView`]: a view taller than its area,
  scrolled by line with the last page kept full: text, or a whole layout
  drawn on an off-screen buffer. Text can also scroll sideways and number
  its lines.
- [`LogBuffer`], [`LogView`], [`LogScroll`]: a log pane: a bounded ring of
  messages any thread pushes to (echoed to the shell while the TUI isn't
  up), its tail following new lines; `LogSink` feeds it from the `log`
  crate (feature `log`).
- [`StatusBar`], [`KeyHints`], [`Toast`], [`Prompt`]: the bottom line: a
  status either side, the key legend fitted between them hint by hint, a
  one-shot message, and an inline y/N question that lapses unanswered.
- [`TimeChart`], [`Trace`], [`braille_spark`]: time series as braille
  lines up to now, gaps kept, scaled to what is in view, the latest values
  in the border; and a sparkline in a string.
- [`Waker`], [`Input`], [`next_event`], [`next_key`], [`is_ctrl_c`],
  [`DrawErrors`]: event-loop pieces: a redraw asked for from any thread,
  the terminal read on its own thread so one wait covers keys and wakes,
  key releases skipped, and failed frames counted rather than fatal.
- [`ColumnTable`], [`Column`], [`Sort`], [`TableRow`]: a table whose
  columns fit the width, the least important left out when narrow, cells
  cut with an `…`, the sort column marked; [`TabBar`]: a row of tabs that
  becomes `‹ tab ›` when narrow; [`fit_line`]: a styled line cut to fit.
- [`Filter`], [`FilterBar`]: a list's filter typed as you go, Enter to
  keep it and Esc to drop it, the list's keys passed through; [`hbar`]:
  a horizontal bar in eighth blocks.
- [`centered_rect`], [`Split`], [`Breadcrumb`]: a popup's place, two panes
  whose share Ctrl-←/→ moves, a header of the app's name and where the
  user is.
- [`render_buffer`], [`buffer_rows`], [`buffer_text`]: a rendered buffer
  read back as text, for tests.
*/

mod breadcrumb;
mod capture;
mod chart;
mod dialog;
mod editor;
mod events;
mod filter;
mod form;
mod layout;
mod list;
mod logview;
mod popup;
mod scroll;
mod signals;
mod statusbar;
mod table;
mod tabs;
mod terminal;
mod testing;
mod text;

pub use breadcrumb::Breadcrumb;
pub use chart::{TimeChart, Trace, braille_spark, hbar, nice_ceil};
pub use dialog::{Answer, ConfirmDialog, InputDialog, confirm_answer};
pub use editor::{EditOutcome, EditorStyle, LineEditor};
pub use events::{
    DrawErrors, DrawReport, Input, Waker, is_ctrl_c, next_event, next_key,
};
pub use filter::{Filter, FilterBar, FilterOutcome};
pub use form::{Field, Focus, Form, FormAction};
pub use layout::{Split, centered_rect};
pub use list::{ListCursor, Nav, NavOutcome};
#[cfg(feature = "log")]
pub use logview::LogSink;
pub use logview::{Echo, LogBuffer, LogEntry, LogLevel, LogScroll, LogView, Stamp};
pub use popup::HelpPopup;
pub use scroll::{CanvasView, Scroll, TextView};
pub use signals::{Signal, SignalBuilder, SignalHandler};
pub use statusbar::{KeyHints, PROMPT_TTL, Prompt, Reply, StatusBar, Toast, pill};
pub use table::{Column, ColumnTable, Sort, TableRow, Width};
pub use tabs::{TabBar, tab_step};
pub use terminal::{
    GuardBuilder, HeldPanic, PanicPolicy, TerminalGuard, Tui, eprintln_safe, held_panics,
    restore_terminal, take_held_panics, tui_active,
};
pub use testing::{buffer_rows, buffer_text, render_buffer};
pub use text::{Cut, ellipsize_end, ellipsize_start, fit_line, range_label, wrap_lines};

/**
`eprintln!` that stays silent while a [`TerminalGuard`] owns the terminal
(see [`tui_active`]), so a stray diagnostic can't scribble over the TUI.
It only gates *your* prints: other crates' output and panics are not
covered (the guard's panic hook handles the latter, and
[`GuardBuilder::capture_stderr`] the former).
*/
#[macro_export]
macro_rules! eprintln_safe {
    ($($arg:tt)*) => {
        $crate::eprintln_safe(::std::format_args!($($arg)*))
    };
}
