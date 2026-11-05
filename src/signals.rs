// Copyright (c) 2026 Mikko Tanner. All rights reserved.

//! Termination signals for a TUI process; see [`SignalHandler`].

use std::fmt;
use std::io::{Error, ErrorKind, Result};
use std::os::raw::c_int;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;
use signal_hook::low_level::emulate_default_handler;

use crate::restore_terminal;

/* ===== consts and statics ================================================= */

/// A handler has been installed in this process (there can be only one).
static INSTALLED: AtomicBool = AtomicBool::new(false);

static ERR_INSTALLED: &str = "a SignalHandler is already installed";
static ERR_NO_SIGNALS: &str = "no signals to handle";
static THREAD_NAME: &str = "minitui-signals";

/// Exit status of a process killed by signal N, by shell convention.
const SIGNAL_EXIT_BASE: i32 = 128;

/* ===== types ============================================================== */

/// The termination signals a [`SignalHandler`] can take over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    /// SIGINT: `kill -INT` (Ctrl-C only outside raw mode).
    Interrupt,
    /// SIGTERM: `kill`, systemd stopping a unit.
    Terminate,
    /// SIGHUP: the controlling terminal went away, e.g. a dropped SSH session.
    Hangup,
    /// SIGQUIT: `kill -QUIT` (Ctrl-\ only outside raw mode); by default it dumps core.
    Quit,
}

type SignalCallback = Box<dyn Fn(Signal) + Send>;

/// Settings for a [`SignalHandler`]; finish with [`SignalBuilder::install`].
#[must_use]
pub struct SignalBuilder {
    signals: Vec<Signal>,
    on_signal: Option<SignalCallback>,
    force_on_repeat: bool,
}

/**
The process-wide termination-signal listener: a thread (signal-hook's
iterator, so the callback is ordinary code, not a signal handler) for
SIGINT, SIGTERM, SIGHUP and SIGQUIT:

- with an `on_signal` callback, the first signal calls it: the app's quit
  path, which drops the [`TerminalGuard`](crate::TerminalGuard) and does
  its own cleanup on the way out;
- a second signal while that is still under way (a hung shutdown) restores
  the terminal and terminates the process through the signal's default
  action, unless [`SignalBuilder::force_on_repeat`] is off;
- without a callback, the first signal already does that: the terminal is
  restored before the process dies, instead of being left in raw mode with
  the alternate screen up as an unhandled `kill` would leave it.

Raw mode turns off the terminal's own signal keys: Ctrl-C and Ctrl-\ arrive
as key events, not SIGINT/SIGQUIT, so the app's key map must handle them.
The signals here come from outside: `kill`, systemd, or a hang-up (SIGHUP)
when an SSH session drops.

The handler lives for the rest of the process: signal-hook can't give a
signal back to its default disposition once registered (dropping the
registration would leave SIGTERM ignored), so the listener is never stopped.

Install it once, early in `main`, before the [`TerminalGuard`](crate::TerminalGuard).

```no_run
use std::sync::atomic::{AtomicBool, Ordering};
use minitui::SignalHandler;

static QUIT: AtomicBool = AtomicBool::new(false);

SignalHandler::builder()
    .on_signal(|_| QUIT.store(true, Ordering::Release))
    .install()?;
# Ok::<(), std::io::Error>(())
```
*/
pub struct SignalHandler;

/// What the listener does with one delivered signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Hand it to the app's callback.
    Notify,
    /// Restore the terminal, then die by the signal's default action.
    Terminate,
}

/* ===== the handler ======================================================== */

impl SignalHandler {
    /// No callback: any of the four signals restores the terminal and terminates.
    pub fn install() -> Result<()> {
        Self::builder().install()
    }

    /// Defaults: all four signals, no callback, force on a repeated signal.
    pub fn builder() -> SignalBuilder {
        SignalBuilder {
            signals: Signal::ALL.to_vec(),
            on_signal: None,
            force_on_repeat: true,
        }
    }
}

impl SignalBuilder {
    /// Handle only these signals; the rest keep their current disposition.
    pub fn signals(mut self, signals: impl IntoIterator<Item = Signal>) -> Self {
        self.signals = signals.into_iter().collect();
        self
    }

    /**
    Call `f` on the listener thread for a signal (a normal thread, so
    anything goes: locks, allocation, printing via
    [`eprintln_safe`](crate::eprintln_safe())). Typically the app's quit
    path: set the quit flag, cancel the tokens, wake the loops.
    */
    pub fn on_signal<F>(mut self, f: F) -> Self
    where
        F: Fn(Signal) + Send + 'static,
    {
        self.on_signal = Some(Box::new(f));
        self
    }

    /**
    Whether a signal arriving after the first one has been passed to
    `on_signal` terminates the process (after restoring the terminal),
    instead of calling `on_signal` again. On by default: a shutdown that
    hangs can still be ended without `kill -9`. Turn it off when skipping
    the app's own cleanup would do more harm than a hung exit.
    */
    pub fn force_on_repeat(mut self, force: bool) -> Self {
        self.force_on_repeat = force;
        self
    }

    /**
    Register the signals and start the listener thread. Fails with
    [`ErrorKind::AlreadyExists`] if a handler is already installed, and with
    [`ErrorKind::InvalidInput`] for an empty signal list.
    */
    pub fn install(self) -> Result<()> {
        if self.signals.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, ERR_NO_SIGNALS));
        }
        if INSTALLED.swap(true, Ordering::AcqRel) {
            return Err(Error::new(ErrorKind::AlreadyExists, ERR_INSTALLED));
        }
        let signals = match Signals::new(self.signals.iter().map(|s| s.number())) {
            Ok(signals) => signals,
            Err(e) => {
                INSTALLED.store(false, Ordering::Release);
                return Err(e);
            }
        };
        /*
        A failed spawn drops `signals` with the closure, which unregisters
        them and leaves them ignored (see the module docs). Spawning a thread
        this early only fails when the process is out of resources anyway.
        */
        thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || listen(signals, self.on_signal, self.force_on_repeat))?;
        Ok(())
    }
}

/// The listener thread: runs for the rest of the process.
fn listen(mut signals: Signals, on_signal: Option<SignalCallback>, force_on_repeat: bool) {
    let mut notified = false;
    for raw in signals.forever() {
        let Some(sig) = Signal::from_number(raw) else {
            continue;
        };
        match next_action(notified, on_signal.is_some(), force_on_repeat) {
            Action::Notify => {
                notified = true;
                if let Some(f) = &on_signal {
                    f(sig);
                }
            }
            Action::Terminate => terminate(sig, notified),
        }
    }
}

fn next_action(notified: bool, has_callback: bool, force_on_repeat: bool) -> Action {
    match (has_callback, notified && force_on_repeat) {
        (true, false) => Action::Notify,
        _ => Action::Terminate,
    }
}

/// Restore the terminal, then die the way `sig` would have killed us.
fn terminate(sig: Signal, repeated: bool) -> ! {
    restore_terminal();
    if repeated {
        eprintln!("{sig} again: exiting without cleanup");
    }
    // resets the default disposition and re-raises: the parent sees the signal
    let _ = emulate_default_handler(sig.number());
    // only reached if the default action didn't end the process
    process::exit(SIGNAL_EXIT_BASE + sig.number());
}

/* ===== Signal ============================================================= */

impl Signal {
    /// Every signal a [`SignalHandler`] handles by default.
    pub const ALL: [Signal; 4] = [Self::Interrupt, Self::Terminate, Self::Hangup, Self::Quit];

    /// The raw signal number.
    pub const fn number(self) -> c_int {
        match self {
            Self::Interrupt => SIGINT,
            Self::Terminate => SIGTERM,
            Self::Hangup => SIGHUP,
            Self::Quit => SIGQUIT,
        }
    }

    pub fn from_number(n: c_int) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.number() == n)
    }

    /// The conventional name, e.g. `SIGTERM`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Hangup => "SIGHUP",
            Self::Quit => "SIGQUIT",
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/* ===== tests ============================================================== */

/*
Delivering real signals is covered by tests/signals.rs, which runs each case
in a child process: a terminating signal would take the test binary down.
*/
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_signal_notifies_a_callback_and_a_repeat_forces() {
        assert_eq!(next_action(false, true, true), Action::Notify);
        assert_eq!(next_action(true, true, true), Action::Terminate);
        assert_eq!(
            next_action(true, true, false),
            Action::Notify,
            "force off: notify again"
        );
    }

    #[test]
    fn without_a_callback_every_signal_terminates() {
        for (notified, force) in [(false, true), (false, false), (true, false)] {
            assert_eq!(next_action(notified, false, force), Action::Terminate);
        }
    }

    #[test]
    fn numbers_and_names_round_trip() {
        for sig in Signal::ALL {
            assert_eq!(Signal::from_number(sig.number()), Some(sig));
        }
        assert_eq!(
            Signal::from_number(SIGTERM).map(Signal::name),
            Some("SIGTERM")
        );
        assert_eq!(Signal::Hangup.to_string(), "SIGHUP");
        assert_eq!(Signal::from_number(0), None);
    }

    #[test]
    fn an_empty_signal_list_is_refused() {
        let err = SignalHandler::builder()
            .signals([])
            .install()
            .expect_err("refused");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }
}
