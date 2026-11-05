# Minitui

Minimal plumbing for implementing a safe TUI in Rust with [ratatui](https://ratatui.rs) and [crossterm](https://github.com/crossterm-rs/crossterm): the parts every TUI app needs but none of them is about.

Not a framework: there is no app trait and no run loop. Each app keeps its own event loop and draws its own views; minitui only makes sure the terminal comes back in one piece.

## Features

- **TerminalGuard**: raw mode, alternate screen and a hidden cursor for the guard's lifetime, plus optional kitty keyboard protocol flags. The terminal is restored on drop, when setup fails partway through, and from a panic hook
    - every setup step that succeeded is undone if a later one fails, so a failed start never strands the shell in raw mode
    - teardown is idempotent: the panic hook and the unwinding `Drop` don't both undo it
    - the panic hook is installed once per process and chains to the previous hook; outside a guard's lifetime panics print as usual
    - only one guard can be alive at a time (a second `build()` fails with `ErrorKind::AlreadyExists`)
- **PanicPolicy**: which panics may tear the terminal down
    - `Always` (default): any panic on any thread restores the terminal before it is reported
    - `UiThreadOnly`: only a panic on the thread that built the guard does; a worker's panic is held (`held_panics` / `take_held_panics`) so the UI can show what died and the app can print it once the terminal is restored
- **on_panic**: a callback run from the panic hook, typically the app's quit path, so other threads stop drawing on a restored terminal
- **tui_active / eprintln_safe(!)**: keep stray stderr output and log echo off the TUI while it is up
- **restore_terminal**: leave TUI mode early, e.g. from a signal handler that is about to exit
- **SignalHandler**: SIGINT, SIGTERM, SIGHUP and SIGQUIT (configurable) on a listener thread, via signal-hook
    - with an `on_signal` callback, the first signal goes to the app's quit path, which drops the guard and does its own cleanup
    - a second signal while that shutdown is still under way restores the terminal and terminates the process by the signal's default action (turn off with `force_on_repeat(false)`)
    - without a callback, the first signal already does that, so a `kill` never leaves the shell in raw mode with the alternate screen up
    - raw mode turns off the terminal's own signal keys: Ctrl-C and Ctrl-\ arrive as key events, so the app's key map still handles them. These signals come from outside: `kill`, systemd, a dropped SSH session (SIGHUP)
    - installed once for the rest of the process: signal-hook can't give a signal back to its default disposition, so the listener is never stopped

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies.minitui]
git = "https://github.com/Ukko-Ylijumala/minitui-rs"
version = "0.1"
```

Releases are tagged `vX.Y.Z`; use `tag = "v0.1.1"` instead of `version` to pin one exactly.

minitui's API uses ratatui and crossterm types (`Terminal`, `KeyboardEnhancementFlags`), so the app must depend on the same versions: ratatui 0.30 and crossterm 0.29.

Requires Rust 1.88 or newer.

## Basic Usage

```rust
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::KeyboardEnhancementFlags;
use minitui::{
    PanicPolicy, SignalHandler, TerminalGuard, eprintln_safe, take_held_panics, tui_active,
};

static QUIT: AtomicBool = AtomicBool::new(false);

fn main() -> std::io::Result<()> {
    // before the guard: a signal from here on can't strand the terminal
    SignalHandler::builder()
        .on_signal(|_| QUIT.store(true, Ordering::Release))
        .install()?;

    let mut guard = TerminalGuard::builder()
        .keyboard_enhancement(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        .panic_policy(PanicPolicy::Always)
        .on_panic(|_| QUIT.store(true, Ordering::Release))
        .panic_prefix("myapp")
        .build()?;

    // check tui_active() too: the panic hook may have restored the terminal already
    while tui_active() && !QUIT.load(Ordering::Acquire) {
        guard.term.draw(|frame| { /* ... */ })?;
        // poll and handle events ...
    }

    drop(guard);
    eprintln_safe!("printed only now that the terminal is back");
    for p in take_held_panics() {
        eprintln!("myapp: {p}");
    }
    Ok(())
}
```

`TerminalGuard::new()` builds one with the defaults: no keyboard flags, `PanicPolicy::Always`, panics reported by the previous hook.

A panic report goes to the previously installed hook (std's, by default) unless `panic_prefix` is set, in which case it is printed as `"{prefix}: {info}"`.

`SignalHandler::install()` installs one with no callback: any of the four signals restores the terminal and terminates, which is the minimum for an app with no quit path to wire in. `.signals([Signal::Terminate, Signal::Hangup])` narrows the set.

## Examples

`guard_demo` checks the guard and the signal handler by hand in a real terminal (`cargo test` has no tty):

```sh
cargo run --example guard_demo            # PanicPolicy::Always
cargo run --example guard_demo -- held    # PanicPolicy::UiThreadOnly
cargo run --example guard_demo -- bare    # SignalHandler without a callback
```

Keys: `p` panics on the UI thread, `w` on a worker thread, `q` quits. The shell must come back clean after each, with the panic report on it. `kill <pid>` (the pid is shown on screen) quits through the callback, or in `bare` mode restores the terminal and dies by the signal.

## License

Copyright (c) 2026 Mikko Tanner. All rights reserved.

License: MIT OR Apache-2.0

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Version History

- 0.1.1: Signal handling
    - `SignalHandler` for termination signals, from mping-rs's signal-hook listener and edge-xdp-flood's SIGHUP handling
- 0.1.0: Initial library version
    - `TerminalGuard`, merged from the per-app copies in mping-rs, edge-xdp-flood, squawk, zfs-browser, nft-browser and collector-status

This library collects the TUI plumbing that had been copied (and had drifted) between several of my applications, so that a fix lands in all of them at once.
