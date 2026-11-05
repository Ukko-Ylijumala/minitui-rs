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
- [`wrap_lines`], [`ellipsize_start`], [`ellipsize_end`]: measuring and
  fitting text in display columns.
*/

mod popup;
mod signals;
mod terminal;
mod text;

pub use popup::HelpPopup;
pub use signals::{Signal, SignalBuilder, SignalHandler};
pub use terminal::{
    GuardBuilder, HeldPanic, PanicPolicy, TerminalGuard, Tui, eprintln_safe, held_panics,
    restore_terminal, take_held_panics, tui_active,
};
pub use text::{ellipsize_end, ellipsize_start, wrap_lines};

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
