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
  drawn on an off-screen buffer.
- [`LogBuffer`], [`LogView`], [`LogScroll`]: a log pane: a bounded ring of
  messages any thread pushes to (echoed to the shell while the TUI isn't
  up), its tail following new lines; `LogSink` feeds it from the `log`
  crate (feature `log`).
*/

mod dialog;
mod editor;
mod form;
mod list;
mod logview;
mod popup;
mod scroll;
mod signals;
mod terminal;
mod text;

pub use dialog::{Answer, ConfirmDialog, InputDialog, confirm_answer};
pub use editor::{EditOutcome, EditorStyle, LineEditor};
pub use form::{Field, Focus, Form, FormAction};
pub use list::{ListCursor, Nav, NavOutcome};
#[cfg(feature = "log")]
pub use logview::LogSink;
pub use logview::{Echo, LogBuffer, LogEntry, LogLevel, LogScroll, LogView, Stamp};
pub use popup::HelpPopup;
pub use scroll::{CanvasView, Scroll, TextView};
pub use signals::{Signal, SignalBuilder, SignalHandler};
pub use terminal::{
    GuardBuilder, HeldPanic, PanicPolicy, TerminalGuard, Tui, eprintln_safe, held_panics,
    restore_terminal, take_held_panics, tui_active,
};
pub use text::{ellipsize_end, ellipsize_start, range_label, wrap_lines};

/**
`eprintln!` that stays silent while a [`TerminalGuard`] owns the terminal
(see [`tui_active`]), so a stray diagnostic can't scribble over the TUI.
It only gates *your* prints: other crates' output and panics are not
covered (the guard's panic hook handles the latter).
*/
#[macro_export]
macro_rules! eprintln_safe {
    ($($arg:tt)*) => {
        $crate::eprintln_safe(::std::format_args!($($arg)*))
    };
}
