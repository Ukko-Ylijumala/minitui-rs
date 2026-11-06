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
- **HelpPopup**: a help overlay, rendered over the whole frame
    - pre-formatted help text (`HelpPopup::new`), or a key / description table with styled keys lined up on the widest key (`HelpPopup::from_rows`); an empty key continues the row above
    - sized to its content plus the block's actual borders, padding and titles, centered, capped to the screen, cells beneath cleared
    - `title`, `style`, `footer` (notes after a blank line), or a whole custom `block`
    - scrollable when taller than the screen: rendered through a `Scroll` that the keys move, with `lines a–b of N` in the bottom border; or a bare `scroll(rows)`, with `overflow(area)` telling how far
- **wrap_lines**: how many rows a text takes word-wrapped to a width, counted by ratatui's own wrapper (so it can't disagree with the rendering), for sizing popups
- **range_label**: `rows 21–40 of 300` for a title
- **ellipsize_start / ellipsize_end**: fit a string into a number of columns with a leading / trailing `…`; display width aware (CJK and emoji take two columns), borrowed when it already fits
- **LineEditor**: a single-line text editor for prompts
    - ← → Home End, Ctrl-← / Ctrl-→ (or Alt-) by word, Backspace / Delete, Ctrl-Backspace / Ctrl-Delete a word
    - readline: Ctrl-A / E / B / F / H, Alt-B / F by word; kills Ctrl-U / K / W, Alt-D, Alt-Backspace, and Ctrl-Y puts the last one back
    - moves and deletes by grapheme: an accented letter, a flag or a ZWJ emoji is one step
    - other Ctrl / Alt chords are ignored, never typed as their bare letter; Ctrl+Alt (AltGr on some layouts) is text
    - `handle_key` returns `Changed` / `Handled` / `Ignored`: Enter, Esc, Tab, Ctrl-C are left to the prompt
    - renders as one line that scrolls sideways by display columns to keep the cursor in view; `secret(true)` masks the text with `•`; `insert` takes a bracketed paste
    - `cursor_column` for apps that show the terminal's own cursor instead of the highlighted cell; `idle_line` for an unfocused field; `take` / `clear` after a submit
- **InputDialog / ConfirmDialog**: modal prompts over the app's own state (what the text or the `y` means stays with the app)
    - input: title, prompt (any number of lines), the editor, an optional `[x]` checkbox, and the validation error in place of the key hint
    - confirm: title, message, the equivalent command in yellow, `y confirm · n / Esc cancel`; `confirm_answer(key)` maps the keys (no Enter-to-confirm, no held-down repeat)
    - sized to their text with a minimum width (64 / 40, without `clamp`, which panics on a narrower terminal), centered, capped to the screen, and tall enough that every wrapped line shows
- **Form**: a modal form of labeled fields: text (a `LineEditor`), checkbox, choice
    - Tab / ↓ and Shift-Tab / ↑ move the focus; Enter submits from any field (a checkbox too: it doesn't toggle); Esc cancels; Space toggles, ←→ choose
    - optional Submit / Cancel buttons in the focus ring; `handle_key` returns `Changed` / `Handled` / `Ignored` / `Submit` / `Cancel`
    - the app's note ("resolving…", the last submit's result) below the fields; the validation error in place of the hint, cleared by the next change; `fail(field, error)` also focuses the field
    - fields by index with typed accessors (`text`, `checked`, `selected`, `editor_mut`); unfocused text fields show their start, cut with `…`
    - sized to its fields, centered, capped to the screen; fields that don't fit the height show a page at a time around the focus
- **ListCursor**: the selection and scroll offset of a list or table, kept across frames
    - `window(len, rows)` before rendering: clamps the selection, scrolls just enough to show it (the last page always full), returns the rows to format; `selected_in_window`, or `list_state` / `table_state` to render the whole list
    - `nav(len).key(key)`: ↑ ↓ Home End, PgUp / PgDn by the rows of the last draw less one; modifier chords are left to the app; `Moved` / `Handled` / `Ignored`
    - `nav(len).wrap()` wraps single steps around; `nav(len).selectable(|i| ..)` skips separators and headings
    - `clamp(len)` after a delete; `reselect(found, len)` keeps the selection on its item across a refresh or a re-sort, and says when the item is gone
- **Scroll**: the offset of a view taller than its area, kept across frames
    - `window(len, rows)` before rendering: clamps the offset so the last page is full, returns the lines to show; `label(what)` for the title when it overflows
    - `key(key)`: ↑ ↓ Home End, PgUp / PgDn by the rows of the last draw less one, never past the end (so ↑ moves at once); `key_with(key, KeyModifiers::SHIFT)` for a detail pane beside a list
- **TextView**: text in a `Scroll`ed view, optionally word-wrapped (scrolling by wrapped rows); only the lines in view are handed to the renderer; `lines a–b of N` in the bottom border when it overflows
- **CanvasView**: a whole layout drawn on an off-screen buffer as tall as it needs, the rows in view copied into the frame, so panels of charts and tables scroll by line however small the terminal
- **LogBuffer / LogView**: a log pane
    - `LogBuffer`: the last N messages, each with its time and level (critical … trace, syslog's plus trace), pushed from any thread (`log.warn(..)`, `log.push(level, ..)`); optionally echoed to the shell (stdout or stderr) while the TUI isn't up, so setup messages stay visible
    - `LogView`: the newest messages, one line each, `time LEVEL: message` styled by level (or the app's own `format`), the time in UTC as date + time, time only or none; only the lines in view are formatted
    - `LogScroll`: follows the newest line until moved up, then stays on the lines it shows while new ones arrive and old ones drop out; End (or ↓ back to the bottom) follows again; `lines a–b of N` in the border while scrolled up
    - `LogSink` (feature `log`): a `log` crate backend feeding a `LogBuffer`
- **StatusBar**: the bottom line
    - `StatusBar`: a status on the left, one on the right sized to its text, and the key legend in between; when narrow the hints give way first, then the left, and the right is cut last
    - `KeyHints`: `(key, what it does)` pairs, keys in bold; only whole hints are shown, the rest dropping out from the end with an `…` in their place
    - `Toast`: a one-shot message styled by its level, kept until replaced or cleared, or lapsing after a set time
    - `Prompt`: an inline y/N question that takes the whole line: `y` goes ahead, any other key cancels (so a double tap of the key that asked can't confirm), and unanswered it lapses after 5 s
    - `pill`: ` text ` in a style, for markers like ` ⏳ working… `
- **TimeChart**: time series as braille lines
    - `Trace`: a line of `(seconds before now, value)` points, borrowed or owned, or of samples taken at a fixed interval; a NaN value is a gap rather than a line bridging it, and a lone point between gaps still shows
    - `TimeChart`: traces from `-span` (or the oldest point) to now; y from 0, a set floor or the lowest value in view, up to a set top or the peak in view with head room, optionally rounded up to a nice 1/2/5 figure (`nice_ceil`); labelled at the bottom, middle and top, x as `-1m`, `-30s`, `now`, both formats replaceable; the traces' names and latest values in the top border instead of ratatui's legend box
    - `braille_spark`: a sparkline in a string, two values a cell, a trickle always visible
- Popup titles are given bare (`"keys"`, `"destroy"`) and shown padded with a space on either side

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies.minitui]
git = "https://github.com/Ukko-Ylijumala/minitui-rs"
version = "0.1"
```

Releases are tagged `vX.Y.Z`; use `tag = "v0.1.3"` instead of `version` to pin one exactly.

Features: `log` adds `LogSink`, a backend for the `log` crate (`features = ["log"]`).

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

The help popup and text fitting, inside `guard.term.draw(|frame| ...)`:

```rust
use minitui::{HelpPopup, ellipsize_start};
use ratatui::style::{Color, Style, Stylize};

static HELP_TEXT: &str = "  q    quit\n  h    this help";
static HELP_ROWS: &[(&str, &str)] = &[("q", "quit"), ("h / F1", "this help")];

// pre-formatted text in the default "keys" box
frame.render_widget(HelpPopup::new(HELP_TEXT).style(Color::Cyan), frame.area());
// or a table, with a footer, scrolled by a Scroll (help_scroll.key(key) in the key handler)
let help = HelpPopup::from_rows(HELP_ROWS, Style::new().bold()).footer("press any key".dark_gray());
frame.render_stateful_widget(help, frame.area(), &mut help_scroll);

// a breadcrumb that keeps its most specific end
let trail = ellipsize_start("tank/data/home/user/projects", area.width as usize);
```

A prompt: the app keeps the editor (and what the text is for), the dialog only draws it:

```rust
use minitui::{Answer, ConfirmDialog, EditOutcome, InputDialog, LineEditor, confirm_answer};

// drawing
let dialog = InputDialog::new("rename", "new name for tank/data:", &editor).error(error.as_deref());
frame.render_widget(dialog, frame.area());

// a key
match editor.handle_key(key) {
    EditOutcome::Changed => error = None,
    EditOutcome::Handled => {}
    EditOutcome::Ignored => match key.code {
        KeyCode::Enter => match validate(editor.text()) {
            Ok(action) => /* open a ConfirmDialog for it, or run it */,
            Err(e) => error = Some(e),
        },
        KeyCode::Esc => /* close */,
        _ => {}
    },
}

// the confirmation
frame.render_widget(ConfirmDialog::new("destroy", "Destroy 'tank/data'?").command("zfs destroy tank/data"), frame.area());
match confirm_answer(key) {
    Some(Answer::Yes) => /* run it */,
    Some(Answer::No) => /* close */,
    None => {}
}
```

## Examples

`guard_demo` checks the guard, the signal handler, the help popup and the dialogs by hand in a real terminal (`cargo test` has no tty):

```sh
cargo run --example guard_demo            # PanicPolicy::Always
cargo run --example guard_demo -- held    # PanicPolicy::UiThreadOnly
cargo run --example guard_demo -- bare    # SignalHandler without a callback
```

Keys: `p` panics on the UI thread, `w` on a worker thread, `h` toggles the help popup (↑ / ↓ scroll it in a small terminal), `i` opens an input dialog (Tab masks the text, Enter asks for confirmation), `q` quits. The shell must come back clean after each, with the panic report on it. `kill <pid>` (the pid is shown on screen) quits through the callback, or in `bare` mode restores the terminal and dies by the signal.

## License

Copyright (c) 2026 Mikko Tanner. All rights reserved.

License: MIT OR Apache-2.0

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Version History

- 0.1.3: Forms, lists and scrolling views
    - `ListCursor`, merged from the list and table selection in zfs-browser, mping-rs, squawk, nft-browser and wiremeup
    - `Form` with text, checkbox and choice `Field`s, from wiremeup's form, mping-rs's add-target dialog and edge-xdp-flood's config dialog
    - `LineEditor` covers what those had from tui-input: grapheme-wise editing, the kill buffer (Ctrl-Y), the Alt word keys; plus `idle_line`, `take` and `clear`
    - `Scroll`, `TextView` and `CanvasView`, from the detail panes of squawk, zfs-browser and nft-browser, squawk's off-screen host view, wiremeup's preview and mping-rs's log popup; `HelpPopup` scrolls through a `Scroll`; `range_label`
- 0.1.2: Popups and prompts
    - `HelpPopup`, from the help overlays in zfs-browser, nft-browser, squawk, collector-status and edge-xdp-flood
    - `LineEditor`, `InputDialog`, `ConfirmDialog` and `confirm_answer`, from zfs-browser's and nft-browser's modals
    - `wrap_lines`, `ellipsize_start` and `ellipsize_end`, measuring in display columns
- 0.1.1: Signal handling
    - `SignalHandler` for termination signals, from mping-rs's signal-hook listener and edge-xdp-flood's SIGHUP handling
- 0.1.0: Initial library version
    - `TerminalGuard`, merged from the per-app copies in mping-rs, edge-xdp-flood, squawk, zfs-browser, nft-browser and collector-status

This library collects the TUI plumbing that had been copied (and had drifted) between several of my applications, so that a fix lands in all of them at once.
