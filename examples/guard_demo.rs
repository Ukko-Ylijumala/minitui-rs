// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Manual check for [`TerminalGuard`] in a real terminal (cargo test has no tty).

    cargo run --example guard_demo            # PanicPolicy::Always
    cargo run --example guard_demo -- held    # PanicPolicy::UiThreadOnly

Keys: `p` panics on the UI thread, `w` on a worker thread, `q` quits. The
shell must come back clean after each, with the panic report on it. In
`held` mode a worker panic leaves the TUI up and shows the held panic.
*/

use std::env;
use std::io::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyboardEnhancementFlags};
use minitui::{
    PanicPolicy, TerminalGuard, eprintln_safe, held_panics, take_held_panics, tui_active,
};
use ratatui::widgets::{Block, Paragraph};

const TICK: Duration = Duration::from_millis(100);
static HELP: &str = "p: panic on the UI thread · w: panic on a worker · q: quit";
/// The quit path: set by `q` and, through `on_panic`, by any panic.
static QUIT: AtomicBool = AtomicBool::new(false);

fn main() -> Result<()> {
    let policy = match env::args().nth(1).as_deref() {
        Some("held") => PanicPolicy::UiThreadOnly,
        _ => PanicPolicy::Always,
    };
    let mut builder = TerminalGuard::builder()
        .keyboard_enhancement(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        .panic_policy(policy)
        .panic_prefix("guard_demo");
    /*
    Under `Always` a worker panic restores the terminal while the loop below
    runs on: `on_panic` stops it, and `tui_active` keeps it from drawing a
    frame onto the restored shell in between. Under `UiThreadOnly` a worker
    panic is held instead and the UI carries on.
    */
    if policy == PanicPolicy::Always {
        builder = builder.on_panic(|_| QUIT.store(true, Ordering::Release));
    }
    let mut guard = builder.build()?;
    eprintln_safe!("this line must not appear: the TUI is up");

    while tui_active() && !QUIT.load(Ordering::Acquire) {
        let held = held_panics();
        let text = match held.last() {
            Some(p) => format!("{policy:?}\n{HELP}\n\nheld: {}", p.message),
            None => format!("{policy:?}\n{HELP}"),
        };
        guard.term.draw(|frame| {
            let body = Paragraph::new(text).block(Block::bordered().title(" guard_demo "));
            frame.render_widget(body, frame.area());
        })?;
        if !event::poll(TICK)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') => QUIT.store(true, Ordering::Release),
            KeyCode::Char('p') => panic!("UI thread panic, as asked"),
            KeyCode::Char('w') => {
                let _ = thread::Builder::new()
                    .name("worker".into())
                    .spawn(|| panic!("worker panic, as asked"))?
                    .join();
            }
            _ => {}
        }
    }
    drop(guard);
    eprintln_safe!("guard dropped: this line must appear");
    for p in take_held_panics() {
        eprintln!("held: {p}");
    }
    Ok(())
}
