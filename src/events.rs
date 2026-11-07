// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Event-loop pieces: [`Waker`] lets other threads ask the render loop for a
frame now; [`Input`] reads the terminal on a thread of its own, so one wait
covers keys, wakes and the heartbeat; [`next_event`] / [`next_key`] are
the single-threaded poll-and-read; [`DrawErrors`] keeps a failed frame from
ending the run. Merged from the apps' loops:
- flood woke its render loop through a Condvar and a flag, collector-status
  through a channel it drained after each frame, mping-rs through a tokio
  `Notify`: a burst of wakes collapses into one frame either way;
- flood's keyboard thread napped when polling failed (no usable terminal:
  `poll` fails at once, and the thread spun a core without the nap);
- wiremeup's `next_key` dropped every key event but `Press`, collector-status
  only `Release`: under the kitty protocol with event types reported, a
  held key sends `Repeat`, and wiremeup's lists moved one row per hold;
- flood counted failed frames rather than end the run on the first one
  (that would have lifted every drop it had installed), logging the first,
  every 30th and the recovery;
- five apps tested for Ctrl-C by hand ([`is_ctrl_c`]).
*/

use std::fmt;
use std::io;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::LogLevel;
use crate::terminal::lock;

/* ===== consts and statics ================================================= */

/**
How long [`Input`]'s thread waits for the terminal before looking at its
stop flag again, and naps when polling fails (flood's and mping-rs's 50 ms).
*/
const INPUT_NAP: Duration = Duration::from_millis(50);
static INPUT_THREAD: &str = "minitui-input";
/// By default [`DrawErrors`] reports the first failure and every this many.
const DRAW_REPORT_EVERY: u64 = 30;

/* ===== types ============================================================== */

/**
Asks the render loop for a frame now. Clone it into every thread that
changes what is shown, and into the signal callback; the loop
[waits](Self::wait) on it with its heartbeat as the timeout. A wake that
comes while the loop is drawing is kept for its next wait, and any number
of them make one frame.
*/
#[derive(Debug, Clone, Default)]
pub struct Waker(Arc<WakeState>);

#[derive(Debug, Default)]
struct WakeState {
    woken: Mutex<bool>,
    cv: Condvar,
}

/**
The terminal's events, read on a thread of its own and handed over through
[`next`](Self::next), which waits on a [`Waker`]: one wait for a key, a
wake from another thread or the heartbeat, whichever comes first. Key
releases are dropped ([`next_event`]). Dropping it stops the thread.
*/
#[derive(Debug)]
pub struct Input {
    events: Receiver<Event>,
    waker: Waker,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/**
Counts the frames that failed to reach the terminal in a row, so a draw
error need not end the run: [`record`](Self::record) each frame's result,
and log the [`DrawReport`] it hands back when one is due: the first
failure, every 30th after it, and the recovery. Optionally it gives up
after so many in a row (a terminal that is gone for good).
*/
#[derive(Debug, Clone)]
pub struct DrawErrors {
    streak: u64,
    report_every: u64,
    give_up_after: Option<u64>,
}

/// What [`DrawErrors::record`] has to report.
#[derive(Debug)]
pub enum DrawReport {
    /// A frame failed; `streak` in a row now.
    Failed { streak: u64, error: io::Error },
    /// A frame got through after `streak` failed ones.
    Recovered { streak: u64 },
}

/* ===== Waker ============================================================== */

impl Waker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for a frame.
    pub fn wake(&self) {
        *lock(&self.0.woken) = true;
        self.0.cv.notify_all();
    }

    /// Wait for a wake or `timeout`, whichever is first; `true` if woken.
    pub fn wait(&self, timeout: Duration) -> bool {
        let woken = lock(&self.0.woken);
        let (mut woken, _) = self
            .0
            .cv
            .wait_timeout_while(woken, timeout, |woken| !*woken)
            .unwrap_or_else(PoisonError::into_inner);
        mem::take(&mut *woken)
    }

    /// [`wait`](Self::wait) until `deadline` (the next tick, say).
    pub fn wait_until(&self, deadline: Instant) -> bool {
        self.wait(deadline.saturating_duration_since(Instant::now()))
    }
}

/* ===== Input ============================================================== */

impl Input {
    /**
    Start reading the terminal, waking `waker` on each event. Start it
    after the [`TerminalGuard`](crate::TerminalGuard): building one may
    read the terminal's answer to a query.
    */
    pub fn spawn(waker: &Waker) -> io::Result<Self> {
        let (send, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new().name(INPUT_THREAD.into()).spawn({
            let (waker, stop) = (waker.clone(), stop.clone());
            move || read_input(&send, &waker, &stop)
        })?;
        Ok(Self {
            events,
            waker: waker.clone(),
            stop,
            thread: Some(thread),
        })
    }

    /**
    The next event, waiting up to `timeout` for one. `None` when the wait
    ended without one: a wake from another thread, or the timeout. Draw a
    frame either way.
    */
    pub fn next(&self, timeout: Duration) -> Option<Event> {
        if let Ok(event) = self.events.try_recv() {
            return Some(event);
        }
        self.waker.wait(timeout);
        self.events.try_recv().ok()
    }

    /// The next event if one is waiting.
    pub fn try_next(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// [`Input`]'s thread: until stopped, or until nobody is listening.
fn read_input(send: &Sender<Event>, waker: &Waker, stop: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        let event = match event::poll(INPUT_NAP) {
            Ok(false) => continue,
            Ok(true) => event::read(),
            Err(e) => Err(e),
        };
        match event {
            Ok(event) if is_release(&event) => {}
            Ok(event) => {
                if send.send(event).is_err() {
                    return;
                }
                waker.wake();
            }
            // no usable terminal: poll fails at once, so nap rather than spin
            Err(_) => thread::sleep(INPUT_NAP),
        }
    }
}

/* ===== single-threaded loops ============================================== */

/**
The next terminal event within `timeout`, if any, skipping key releases
(sent only under the kitty protocol with event types reported). A held
key's `Repeat`s are kept, so holding ↓ keeps moving.
*/
pub fn next_event(timeout: Duration) -> io::Result<Option<Event>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    let event = event::read()?;
    Ok((!is_release(&event)).then_some(event))
}

/// The next key press or repeat within `timeout`; other events are `None`.
pub fn next_key(timeout: Duration) -> io::Result<Option<KeyEvent>> {
    Ok(match next_event(timeout)? {
        Some(Event::Key(key)) => Some(key),
        _ => None,
    })
}

/**
Ctrl-C, which raw mode delivers as a key rather than a signal (with or
without other modifiers, Caps Lock included).
*/
pub fn is_ctrl_c(key: KeyEvent) -> bool {
    key.kind != KeyEventKind::Release
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c' | 'C'))
}

fn is_release(event: &Event) -> bool {
    matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release)
}

/* ===== DrawErrors ========================================================= */

impl Default for DrawErrors {
    fn default() -> Self {
        Self {
            streak: 0,
            report_every: DRAW_REPORT_EVERY,
            give_up_after: None,
        }
    }
}

impl DrawErrors {
    /// Reports the first failure and every 30th; never gives up.
    pub fn new() -> Self {
        Self::default()
    }

    /// Report every `n`th failure in a row after the first.
    pub fn report_every(mut self, n: u64) -> Self {
        self.report_every = n.max(1);
        self
    }

    /// Give up (pass the error on) after `n` failures in a row; `None` never.
    pub fn give_up_after(mut self, n: impl Into<Option<u64>>) -> Self {
        self.give_up_after = n.into();
        self
    }

    /// Failed frames in a row now.
    pub fn streak(&self) -> u64 {
        self.streak
    }

    /**
    Count a frame: `term.draw(..)`'s result. A report when one is due;
    the error itself once [`give_up_after`](Self::give_up_after) frames in
    a row have failed, for the loop to end the run with.
    */
    pub fn record<T>(&mut self, drawn: io::Result<T>) -> io::Result<Option<DrawReport>> {
        match drawn {
            Ok(_) if self.streak > 0 => Ok(Some(DrawReport::Recovered {
                streak: mem::take(&mut self.streak),
            })),
            Ok(_) => Ok(None),
            Err(error) => {
                self.streak += 1;
                let streak = self.streak;
                if self.give_up_after.is_some_and(|n| streak >= n) {
                    return Err(error);
                }
                let due = streak == 1 || streak.is_multiple_of(self.report_every);
                Ok(due.then_some(DrawReport::Failed { streak, error }))
            }
        }
    }
}

impl DrawReport {
    /// [`LogLevel::Error`] for a failure, [`LogLevel::Notice`] for the recovery.
    pub fn level(&self) -> LogLevel {
        match self {
            Self::Failed { .. } => LogLevel::Error,
            Self::Recovered { .. } => LogLevel::Notice,
        }
    }
}

impl fmt::Display for DrawReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed { streak, error } => {
                write!(f, "drawing failed ({streak} in a row): {error}")
            }
            Self::Recovered { streak } => {
                write!(f, "drawing recovered after {streak} failed frame(s)")
            }
        }
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    const SHORT: Duration = Duration::from_millis(20);
    const LONG: Duration = Duration::from_secs(10);

    fn failed() -> io::Result<()> {
        Err(io::Error::other("stdout is gone"))
    }

    #[test]
    fn a_wake_is_kept_for_the_next_wait() {
        let waker = Waker::new();
        waker.wake();
        waker.wake();
        let started = Instant::now();
        assert!(waker.wait(LONG), "woken before the wait");
        assert!(started.elapsed() < LONG / 2);
        assert!(!waker.wait(SHORT), "two wakes made one");
        assert!(!waker.wait_until(Instant::now() + SHORT));
    }

    #[test]
    fn another_thread_wakes_the_loop() {
        let waker = Waker::new();
        let other = waker.clone();
        let started = Instant::now();
        let thread = thread::spawn(move || {
            thread::sleep(SHORT);
            other.wake();
        });
        assert!(waker.wait(LONG));
        assert!(started.elapsed() < LONG / 2);
        thread.join().unwrap();
    }

    #[test]
    fn releases_are_skipped_and_ctrl_c_found() {
        let mut key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(is_ctrl_c(key) && !is_release(&Event::Key(key)));
        key.code = KeyCode::Char('C');
        key.modifiers |= KeyModifiers::SHIFT;
        assert!(is_ctrl_c(key));
        key.kind = KeyEventKind::Release;
        assert!(!is_ctrl_c(key) && is_release(&Event::Key(key)));
        key.kind = KeyEventKind::Repeat;
        assert!(!is_release(&Event::Key(key)));
        assert!(!is_ctrl_c(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
        assert!(!is_release(&Event::FocusGained));
    }

    #[test]
    fn draw_errors_report_the_first_every_nth_and_the_recovery() {
        let mut errors = DrawErrors::new().report_every(3);
        assert!(errors.record(Ok(())).unwrap().is_none());
        let reports: Vec<Option<u64>> = (0..7)
            .map(|_| match errors.record(failed()).unwrap() {
                Some(DrawReport::Failed { streak, .. }) => Some(streak),
                _ => None,
            })
            .collect();
        assert_eq!(reports, [Some(1), None, Some(3), None, None, Some(6), None]);
        assert_eq!(errors.streak(), 7);
        let recovered = errors.record(Ok(())).unwrap().unwrap();
        assert_eq!(
            recovered.to_string(),
            "drawing recovered after 7 failed frame(s)"
        );
        assert_eq!(recovered.level(), LogLevel::Notice);
        assert_eq!(errors.streak(), 0);
        let failed_again = errors.record(failed()).unwrap().unwrap();
        assert_eq!(
            failed_again.to_string(),
            "drawing failed (1 in a row): stdout is gone"
        );
        assert_eq!(failed_again.level(), LogLevel::Error);
    }

    #[test]
    fn draw_errors_can_give_up() {
        let mut errors = DrawErrors::new().give_up_after(2);
        assert!(errors.record(failed()).is_ok());
        assert!(errors.record(failed()).is_err());
    }
}
