// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Manual check for [`TerminalGuard`], [`SignalHandler`] and the popups in a
real terminal (cargo test has no tty).

    cargo run --example guard_demo            # PanicPolicy::Always
    cargo run --example guard_demo -- held    # PanicPolicy::UiThreadOnly
    cargo run --example guard_demo -- bare    # signals: no quit callback

Keys: `p` panics on the UI thread, `w` on a worker thread, `h` toggles the
[`HelpPopup`] (↑ / ↓ scroll it when it doesn't fit), `i` opens an
[`InputDialog`] (Tab masks the text, Enter asks a [`ConfirmDialog`]), `q`
quits. The
shell must come back clean after each, with the panic report on it. In
`held` mode a worker panic leaves the TUI up and shows the held panic.

Signals: `kill <pid>` (shown on screen) quits through the callback; in
`bare` mode it restores the terminal and kills the demo by the signal.
*/

use std::env;
use std::io::Result;
use std::mem;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
};
use minitui::{
    Answer, ConfirmDialog, EditOutcome, HelpPopup, InputDialog, LineEditor, PanicPolicy,
    SignalHandler, TerminalGuard, confirm_answer, eprintln_safe, held_panics, take_held_panics,
    tui_active,
};
use ratatui::Frame;
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{Block, Paragraph};

const TICK: Duration = Duration::from_millis(100);
static HINT: &str = "h: help · i: input · q: quit";
/// Key, what it does; an empty key continues the row above.
static HELP_ROWS: &[(&str, &str)] = &[
    ("p", "panic on the UI thread"),
    ("w", "panic on a worker thread"),
    ("", "(held in `held` mode, else the demo quits)"),
    (
        "i",
        "an input dialog: Tab masks the text, Enter confirms it",
    ),
    ("h / F1", "toggle this help"),
    ("↑ / ↓", "scroll this help, when it doesn't fit"),
    ("q", "quit"),
];
static HELP_FOOTER: &str = "kill <pid> quits through the signal callback";
static INPUT_PROMPT: &str = "type something\n(readline keys: Ctrl-A/E/U/K/W, Ctrl-←/→)";
static MASK_LABEL: &str = "mask the text";
static ERR_EMPTY: &str = "type something first";
/// The quit path: set by `q`, a signal and, through `on_panic`, by any panic.
static QUIT: AtomicBool = AtomicBool::new(false);

/// The open dialog, if any.
enum Modal {
    None,
    Input {
        editor: LineEditor,
        error: Option<String>,
        masked: bool,
    },
    Confirm {
        text: String,
    },
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let policy = match args.iter().any(|a| a == "held") {
        true => PanicPolicy::UiThreadOnly,
        false => PanicPolicy::Always,
    };
    let bare = args.iter().any(|a| a == "bare");
    // the signal handler first: a signal from here on can't strand the terminal
    match bare {
        true => SignalHandler::install()?,
        false => SignalHandler::builder()
            .on_signal(|sig| {
                eprintln_safe!("this line must not appear: {sig} arrived while the TUI is up");
                QUIT.store(true, Ordering::Release);
            })
            .install()?,
    }
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

    let (mut show_help, mut help_scroll) = (false, 0u16);
    let mut modal = Modal::None;
    let mut accepted: Option<String> = None;
    while tui_active() && !QUIT.load(Ordering::Acquire) {
        let held = held_panics();
        let head = format!("{policy:?} · pid {} · bare: {bare}\n{HINT}", process::id());
        let mut text = match held.last() {
            Some(p) => format!("{head}\n\nheld: {}", p.message),
            None => head,
        };
        if let Some(a) = &accepted {
            text.push_str(&format!("\n\naccepted: {a}"));
        }
        guard.term.draw(|frame| {
            let body = Paragraph::new(text).block(Block::bordered().title(" guard_demo "));
            frame.render_widget(body, frame.area());
            if show_help {
                let help = HelpPopup::from_rows(HELP_ROWS, Style::new().bold().cyan())
                    .footer(HELP_FOOTER.dark_gray());
                // keep the scroll within what is actually hidden
                help_scroll = help_scroll.min(help.overflow(frame.area()));
                frame.render_widget(help.scroll(help_scroll), frame.area());
            }
            draw_modal(frame, &modal);
        })?;
        if !event::poll(TICK)? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(key) => key,
            Event::Paste(s) => {
                if let Modal::Input { editor, error, .. } = &mut modal
                    && editor.insert(&s) == EditOutcome::Changed
                {
                    *error = None;
                }
                continue;
            }
            _ => continue,
        };
        // an open dialog takes every key
        if !matches!(modal, Modal::None) {
            if let Some(a) = modal_key(&mut modal, key) {
                accepted = Some(a);
            }
            continue;
        }
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') => QUIT.store(true, Ordering::Release),
            KeyCode::Char('i') => {
                modal = Modal::Input {
                    editor: LineEditor::default(),
                    error: None,
                    masked: false,
                };
            }
            KeyCode::Char('h') | KeyCode::F(1) => show_help = !show_help,
            KeyCode::Up => help_scroll = help_scroll.saturating_sub(1),
            KeyCode::Down => help_scroll = help_scroll.saturating_add(1),
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

fn draw_modal(frame: &mut Frame, modal: &Modal) {
    match modal {
        Modal::None => {}
        Modal::Input {
            editor,
            error,
            masked,
        } => {
            let dialog = InputDialog::new("input", INPUT_PROMPT, editor)
                .error(error.as_deref())
                .checkbox(MASK_LABEL, *masked);
            frame.render_widget(dialog, frame.area());
        }
        Modal::Confirm { text } => {
            let dialog = ConfirmDialog::new("confirm", format!("Accept '{text}'?"))
                .command(format!("echo {text}"));
            frame.render_widget(dialog, frame.area());
        }
    }
}

/// One key for the open dialog; the accepted text when a confirm says yes.
fn modal_key(modal: &mut Modal, key: KeyEvent) -> Option<String> {
    match modal {
        Modal::None => None,
        Modal::Input {
            editor,
            error,
            masked,
        } => {
            match editor.handle_key(key) {
                EditOutcome::Changed => *error = None,
                EditOutcome::Handled => {}
                // the prompt's own keys
                EditOutcome::Ignored if key.kind != KeyEventKind::Press => {}
                EditOutcome::Ignored => match key.code {
                    KeyCode::Esc => *modal = Modal::None,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        *modal = Modal::None;
                    }
                    KeyCode::Tab => {
                        *masked = !*masked;
                        *editor = mem::take(editor).secret(*masked);
                    }
                    KeyCode::Enter if editor.is_empty() => *error = Some(ERR_EMPTY.into()),
                    KeyCode::Enter => {
                        *modal = Modal::Confirm {
                            text: editor.text().to_string(),
                        };
                    }
                    _ => {}
                },
            }
            None
        }
        Modal::Confirm { text } => match confirm_answer(key)? {
            Answer::Yes => {
                let text = mem::take(text);
                *modal = Modal::None;
                Some(text)
            }
            Answer::No => {
                *modal = Modal::None;
                None
            }
        },
    }
}
