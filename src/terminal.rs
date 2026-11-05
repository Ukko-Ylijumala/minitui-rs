// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Terminal lifecycle. [`TerminalGuard`] puts the terminal into TUI mode (raw
mode, alternate screen, hidden cursor and, if asked, the kitty keyboard
protocol) and restores it on drop, when setup fails partway through, and
from a panic hook.

Merged from the six per-app copies (mping-rs, edge-xdp-flood, squawk,
zfs-browser, nft-browser, collector-status), keeping every fix any of them
had:

- every setup step that succeeded is undone if a later one fails;
  bailing out with `?` before the guard exists means `Drop` never runs,
  stranding the shell in raw mode with no visible error (only flood got
  this right);
- the TUI counts as active from the moment setup starts, *before* the
  screen switch, so log echo can't race into the new screen (flood);
- teardown is idempotent, so the panic hook and the unwinding `Drop` don't
  both undo it (collector-status);
- the panic hook is installed once per process and passes the panic on to
  the hook that was there before (flood). While no guard is alive it only
  passes the panic on, so panics outside the TUI's lifetime print exactly
  as they would without this crate;
- an optional `on_panic` callback, typically the app's quit path, so other
  threads stop drawing on a terminal that has been torn down (mping,
  collector-status);
- [`PanicPolicy::UiThreadOnly`]: only a panic on the UI thread tears the
  terminal down; a worker panic is held for the app to report (zfs-browser);
- optional kitty keyboard enhancement, pushed only where the terminal
  supports it and popped only if it was pushed (zfs-browser, nft-browser).
*/

use std::fmt;
use std::io::{Error, ErrorKind, Result, Stdout, stdout};
use std::mem;
use std::panic::{self, PanicHookInfo};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, PoisonError};
use std::thread::{self, ThreadId};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/* ===== consts and statics ================================================= */

/*
Terminal state as bits in one atomic, so teardown can take and clear all of
it in a single `swap` (idempotent: a second caller sees 0 and does nothing).
CLAIMED is set first, before any terminal change, and stays set until
teardown: it is what `tui_active` reports. The others record which setup
steps actually happened, so teardown undoes exactly those.
*/
const CLAIMED: u8 = 1 << 0;
const RAW_MODE: u8 = 1 << 1;
const ALT_SCREEN: u8 = 1 << 2;
const KB_FLAGS: u8 = 1 << 3;

static STATE: AtomicU8 = AtomicU8::new(0);
/// The process-wide panic hook is installed (once, by the first guard).
static HOOK: Once = Once::new();
/// The live guard's panic settings; `None` while no guard is alive.
static CONFIG: Mutex<Option<Arc<HookConfig>>> = Mutex::new(None);
/// Panics on non-UI threads, held while the terminal was ours.
static HELD: Mutex<Vec<HeldPanic>> = Mutex::new(Vec::new());

static ERR_ACTIVE: &str = "a TerminalGuard is already active";
/// Shown when a panic payload is neither a `&str` nor a `String`.
static OPAQUE_PAYLOAD: &str = "Box<dyn Any>";
/// Std's own name for a thread without one.
static UNNAMED_THREAD: &str = "<unnamed>";

/* ===== types ============================================================== */

/// The terminal type a [`TerminalGuard`] hands out.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

type PanicCallback = Box<dyn Fn(&PanicHookInfo) + Send + Sync>;

/// Which panics may tear the terminal down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanicPolicy {
    /// Any panic, on any thread, restores the terminal before it is reported.
    #[default]
    Always,
    /**
    Only a panic on the UI thread (the one that built the guard) restores the
    terminal. A panic elsewhere (a worker, a sampler, a rayon task) leaves
    the UI drawing on a terminal that is still in TUI mode: tearing it down
    under the UI thread would interleave escape codes with a frame in
    progress. Its report is held instead (see [`held_panics`]), so the UI
    can say what died and the app can print it once the terminal is
    restored. The UI is expected to notice a dead worker on its own (a
    disconnected channel, say).
    */
    UiThreadOnly,
}

/// A panic held under [`PanicPolicy::UiThreadOnly`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldPanic {
    /// The panicking thread's name (`<unnamed>` as std says, if it has none).
    pub thread: String,
    /// `file:line:col`, when the panic carried one.
    pub location: Option<String>,
    /// The panic message alone, fit for one line of a status bar.
    pub message: String,
}

/// The live guard's panic settings, read by the hook.
struct HookConfig {
    policy: PanicPolicy,
    ui_thread: ThreadId,
    on_panic: Option<PanicCallback>,
    prefix: Option<String>,
}

/**
Owns the terminal in TUI mode until dropped. Build one with
[`TerminalGuard::new`] (defaults) or [`TerminalGuard::builder`]; only one
can be alive at a time.

```no_run
use crossterm::event::KeyboardEnhancementFlags;
use minitui::{PanicPolicy, TerminalGuard};

let mut guard = TerminalGuard::builder()
    .keyboard_enhancement(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    .panic_policy(PanicPolicy::UiThreadOnly)
    .panic_prefix("zfs-browser")
    .build()?;
guard.term.draw(|frame| { /* ... */ })?;
# Ok::<(), std::io::Error>(())
```
*/
pub struct TerminalGuard {
    pub term: Tui,
}

/// Settings for a [`TerminalGuard`]; finish with [`GuardBuilder::build`].
#[derive(Default)]
#[must_use]
pub struct GuardBuilder {
    keyboard: Option<KeyboardEnhancementFlags>,
    policy: PanicPolicy,
    on_panic: Option<PanicCallback>,
    prefix: Option<String>,
}

/* ===== the guard ========================================================== */

impl TerminalGuard {
    /// A guard with the defaults: no keyboard enhancement, [`PanicPolicy::Always`].
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }

    pub fn builder() -> GuardBuilder {
        GuardBuilder::default()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
        set_config(None);
    }
}

impl GuardBuilder {
    /**
    Ask for kitty keyboard protocol `flags` (e.g. `DISAMBIGUATE_ESCAPE_CODES`,
    so Shift-Space no longer folds into Space). Best-effort: they are pushed
    only where the terminal says it supports them, and the legacy encoding
    stays elsewhere. Note that the support query blocks for a moment on
    terminals that don't answer.
    */
    pub fn keyboard_enhancement(mut self, flags: KeyboardEnhancementFlags) -> Self {
        self.keyboard = Some(flags);
        self
    }

    pub fn panic_policy(mut self, policy: PanicPolicy) -> Self {
        self.policy = policy;
        self
    }

    /**
    Run `f` from the panic hook on every panic while the guard is alive,
    before the terminal is restored. Typically the app's quit path: once the
    hook has torn down the terminal, the other threads must stop drawing.
    It runs inside the panic hook, so it must not panic itself (that
    aborts) and should not block.
    */
    pub fn on_panic<F>(mut self, f: F) -> Self
    where
        F: Fn(&PanicHookInfo) + Send + Sync + 'static,
    {
        self.on_panic = Some(Box::new(f));
        self
    }

    /**
    Report panics as `"{prefix}: {info}"` instead of passing them to the
    previously installed hook (by default std's, which also prints the
    thread name and the `RUST_BACKTRACE` note).
    */
    pub fn panic_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /**
    Enter TUI mode. Fails with [`ErrorKind::AlreadyExists`] while another
    guard is alive; on any other failure, whatever setup already happened is
    undone before returning.
    */
    pub fn build(self) -> Result<TerminalGuard> {
        if STATE
            .compare_exchange(0, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::new(ErrorKind::AlreadyExists, ERR_ACTIVE));
        }
        // the hook before any terminal change: a panic from here on restores it
        install_hook();
        set_config(Some(Arc::new(HookConfig {
            policy: self.policy,
            ui_thread: thread::current().id(),
            on_panic: self.on_panic,
            prefix: self.prefix,
        })));
        match enter(self.keyboard) {
            Ok(term) => Ok(TerminalGuard { term }),
            Err(e) => {
                restore_terminal();
                set_config(None);
                Err(e)
            }
        }
    }
}

/// The setup steps, each recorded in [`STATE`] as soon as it has happened.
fn enter(keyboard: Option<KeyboardEnhancementFlags>) -> Result<Tui> {
    enable_raw_mode()?;
    STATE.fetch_or(RAW_MODE, Ordering::AcqRel);
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, Hide)?;
    STATE.fetch_or(ALT_SCREEN, Ordering::AcqRel);
    if let Some(flags) = keyboard
        && matches!(supports_keyboard_enhancement(), Ok(true))
        && execute!(out, PushKeyboardEnhancementFlags(flags)).is_ok()
    {
        STATE.fetch_or(KB_FLAGS, Ordering::AcqRel);
    }
    Terminal::new(CrosstermBackend::new(out))
}

/**
Undo whatever TUI setup is in effect, in reverse order. Idempotent and safe
from any thread; a no-op while no guard is active. The guard calls this on
drop and the panic hook calls it on a panic. Call it yourself only to leave
TUI mode early, e.g. from a signal handler that is about to exit.
*/
pub fn restore_terminal() {
    let state = STATE.swap(0, Ordering::AcqRel);
    let mut out = stdout();
    /*
    Pop before leaving the alternate screen: kitty keeps a separate flag
    stack per screen, so popping after the switch would pop the main
    screen's stack instead.
    */
    if state & KB_FLAGS != 0 {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    if state & ALT_SCREEN != 0 {
        let _ = execute!(out, LeaveAlternateScreen, Show);
    }
    if state & RAW_MODE != 0 {
        let _ = disable_raw_mode();
    }
}

/**
Whether a guard currently owns the terminal: from the start of its setup
until it is restored (by drop or by the panic hook). While it is, nothing
else may write to stdout/stderr. A render loop on another thread should
also check this before drawing, since the panic hook may have restored the
terminal before the unwinding reaches the app's own shutdown path.
*/
pub fn tui_active() -> bool {
    STATE.load(Ordering::Acquire) & CLAIMED != 0
}

/// `eprintln!` unless the TUI is active; see the [`eprintln_safe!`](crate::eprintln_safe!) macro.
pub fn eprintln_safe(args: fmt::Arguments<'_>) {
    if !tui_active() {
        eprintln!("{args}");
    }
}

/* ===== the panic hook ===================================================== */

fn install_hook() {
    HOOK.call_once(|| {
        let prev = panic::take_hook();
        panic::set_hook(Box::new(move |info| on_panic(info, &*prev)));
    });
}

fn on_panic(info: &PanicHookInfo, prev: &(dyn Fn(&PanicHookInfo) + Send + Sync)) {
    // clone the Arc out: no lock is held while the callbacks run
    let Some(cfg) = lock(&CONFIG).clone() else {
        return prev(info);
    };
    if let Some(f) = &cfg.on_panic {
        f(info);
    }
    // once the terminal is back (or before it was taken), holding is pointless
    if cfg.policy == PanicPolicy::UiThreadOnly
        && thread::current().id() != cfg.ui_thread
        && tui_active()
    {
        lock(&HELD).push(HeldPanic::from_info(info));
        return;
    }
    // restore first, so the report lands on the real screen
    restore_terminal();
    match &cfg.prefix {
        Some(prefix) => eprintln!("{prefix}: {info}"),
        None => prev(info),
    }
}

/// Panics held from non-UI threads so far (see [`PanicPolicy::UiThreadOnly`]), oldest first.
pub fn held_panics() -> Vec<HeldPanic> {
    lock(&HELD).clone()
}

/// Like [`held_panics`], but empties the list.
pub fn take_held_panics() -> Vec<HeldPanic> {
    mem::take(&mut *lock(&HELD))
}

impl HeldPanic {
    fn from_info(info: &PanicHookInfo) -> Self {
        let me = thread::current();
        Self {
            thread: me.name().unwrap_or(UNNAMED_THREAD).to_string(),
            location: info.location().map(|l| l.to_string()),
            message: payload_str(info).unwrap_or(OPAQUE_PAYLOAD).to_string(),
        }
    }
}

/// One line, std's wording: `thread 'x' panicked at src/a.rs:1:2: message`.
impl fmt::Display for HeldPanic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location {
            Some(at) => write!(
                f,
                "thread '{}' panicked at {at}: {}",
                self.thread, self.message
            ),
            None => write!(f, "thread '{}' panicked: {}", self.thread, self.message),
        }
    }
}

/* ===== helpers ============================================================ */

fn set_config(cfg: Option<Arc<HookConfig>>) {
    *lock(&CONFIG) = cfg;
}

/// The panic message, if it is a string (`PanicHookInfo::payload_as_str` needs 1.91).
fn payload_str<'a>(info: &'a PanicHookInfo) -> Option<&'a str> {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
}

/// Lock, shrugging off poisoning: these statics hold plain data, never torn.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/* ===== tests ============================================================== */

/*
The tests drive the real process-wide hook and statics, so they run one at
a time (TEST_LOCK) and only ever set the CLAIMED bit: with no RAW_MODE /
ALT_SCREEN / KB_FLAGS recorded, a teardown writes nothing to the terminal
running the tests. Building a real guard needs a tty, which cargo test
doesn't provide; the `guard_demo` example covers that by hand.

The hook also sees other tests' panics (a failing assert on a thread of
their own), so these tests count and inspect only their own, marked with
TEST_PANIC. A foreign panic landing in a UiThreadOnly window is held and
dropped by `reset`: a failing test elsewhere may lose its message then, but
its failure still shows.
*/
#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::catch_unwind;
    use std::sync::atomic::AtomicUsize;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    /// The message of every panic these tests raise on purpose.
    static TEST_PANIC: &str = "minitui test panic";

    fn is_ours(info: &PanicHookInfo) -> bool {
        payload_str(info) == Some(TEST_PANIC)
    }

    /// The held panics these tests raised.
    fn held_ours() -> Vec<HeldPanic> {
        held_panics()
            .into_iter()
            .filter(|p| p.message == TEST_PANIC)
            .collect()
    }

    /// Fake a live guard: hook installed, CLAIMED set, `policy` configured.
    fn fake_guard(policy: PanicPolicy, calls: &Arc<AtomicUsize>) -> MutexGuard<'static, ()> {
        let serial = lock(&TEST_LOCK);
        install_hook();
        STATE.store(CLAIMED, Ordering::Release);
        take_held_panics();
        let calls = calls.clone();
        set_config(Some(Arc::new(HookConfig {
            policy,
            ui_thread: thread::current().id(),
            on_panic: Some(Box::new(move |info| {
                if is_ours(info) {
                    calls.fetch_add(1, Ordering::Relaxed);
                }
            })),
            prefix: Some("test".into()),
        })));
        serial
    }

    fn reset() {
        set_config(None);
        STATE.store(0, Ordering::Release);
        take_held_panics();
    }

    fn panic_on_worker(name: &str) {
        let worker = thread::Builder::new()
            .name(name.into())
            .spawn(|| panic!("{TEST_PANIC}"))
            .unwrap();
        assert!(worker.join().is_err());
    }

    #[test]
    fn worker_panic_is_held_under_ui_thread_only() {
        let calls = Arc::new(AtomicUsize::new(0));
        let _serial = fake_guard(PanicPolicy::UiThreadOnly, &calls);
        panic_on_worker("worker");

        assert!(tui_active(), "a worker panic must leave the terminal alone");
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "on_panic runs for every panic"
        );
        let held = held_ours();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].thread, "worker");
        assert!(
            held[0]
                .location
                .as_deref()
                .is_some_and(|l| l.contains("terminal.rs"))
        );
        assert!(take_held_panics().iter().any(|p| *p == held[0]));
        assert!(held_ours().is_empty());
        reset();
    }

    #[test]
    fn ui_thread_panic_restores_under_ui_thread_only() {
        let calls = Arc::new(AtomicUsize::new(0));
        let _serial = fake_guard(PanicPolicy::UiThreadOnly, &calls);
        assert!(catch_unwind(|| panic!("{TEST_PANIC}")).is_err());

        assert!(!tui_active());
        assert!(held_ours().is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        reset();
    }

    #[test]
    fn any_panic_restores_under_always() {
        let calls = Arc::new(AtomicUsize::new(0));
        let _serial = fake_guard(PanicPolicy::Always, &calls);
        panic_on_worker("worker");

        assert!(!tui_active());
        assert!(held_ours().is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        reset();
    }

    #[test]
    fn nothing_is_held_once_the_terminal_is_back() {
        let calls = Arc::new(AtomicUsize::new(0));
        let _serial = fake_guard(PanicPolicy::UiThreadOnly, &calls);
        restore_terminal();
        panic_on_worker("late");

        assert!(
            held_ours().is_empty(),
            "reported, not held: no TUI to hide it from"
        );
        reset();
    }

    #[test]
    fn without_a_guard_the_hook_only_passes_panics_on() {
        let calls = Arc::new(AtomicUsize::new(0));
        let _serial = fake_guard(PanicPolicy::UiThreadOnly, &calls);
        reset();
        panic_on_worker("bystander");

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(held_ours().is_empty());
    }

    #[test]
    fn a_second_guard_is_refused_without_touching_the_terminal() {
        let calls = Arc::new(AtomicUsize::new(0));
        let _serial = fake_guard(PanicPolicy::Always, &calls);
        let err = TerminalGuard::new().err().expect("second guard");

        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(
            STATE.load(Ordering::Acquire),
            CLAIMED,
            "first guard's state untouched"
        );
        reset();
    }

    #[test]
    fn restore_is_idempotent() {
        let _serial = lock(&TEST_LOCK);
        STATE.store(CLAIMED, Ordering::Release);
        restore_terminal();
        restore_terminal();
        assert!(!tui_active());
    }

    #[test]
    fn held_panic_displays_like_std() {
        let mut p = HeldPanic {
            thread: "w".into(),
            location: Some("src/a.rs:1:2".into()),
            message: "boom".into(),
        };
        assert_eq!(p.to_string(), "thread 'w' panicked at src/a.rs:1:2: boom");
        p.location = None;
        assert_eq!(p.to_string(), "thread 'w' panicked: boom");
    }
}
