// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Manual check for [`TerminalGuard`], [`SignalHandler`] and the popups in a
real terminal (cargo test has no tty).

    cargo run --example guard_demo            # PanicPolicy::Always
    cargo run --example guard_demo -- held    # PanicPolicy::UiThreadOnly
    cargo run --example guard_demo -- bare    # signals: no quit callback

Keys: `p` panics on the UI thread, `w` on a worker thread, `h` toggles the
[`HelpPopup`] (↑ ↓ Home End PgUp PgDn scroll it when it doesn't fit;
otherwise they move in the [`ListCursor`] list), `i` opens an
[`InputDialog`] (Tab masks the text, Enter asks a [`ConfirmDialog`]), `f`
a [`Form`], `v` a wrapped [`TextView`], `q` quits; Shift with ↑ ↓ Home
End PgUp PgDn scrolls the [`LogView`] beside the list, which a ticker
thread feeds. The shell must come back clean after each, with the panic
report on it. In `held` mode a worker panic leaves the TUI up and shows
the held panic. The log's first and last lines echo on the shell, before
the TUI comes up and after it is gone.

Signals: `kill <pid>` (shown on screen) quits through the callback; in
`bare` mode it restores the terminal and kills the demo by the signal.
*/

use std::env;
use std::io::Result;
use std::mem;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
};
use minitui::{
    Answer, ConfirmDialog, Echo, EditOutcome, Field, Form, FormAction, HelpPopup, InputDialog,
    LineEditor, ListCursor, LogBuffer, LogLevel, LogScroll, LogView, NavOutcome, PanicPolicy,
    Scroll, SignalHandler, Stamp, TerminalGuard, TextView, confirm_answer, eprintln_safe,
    held_panics, range_label, take_held_panics, tui_active,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

const TICK: Duration = Duration::from_millis(100);
static HINT: &str = "h: help · i: input · f: form · v: view · q: quit";
/// Key, what it does; an empty key continues the row above.
static HELP_ROWS: &[(&str, &str)] = &[
    ("p", "panic on the UI thread"),
    ("w", "panic on a worker thread"),
    ("", "(held in `held` mode, else the demo quits)"),
    (
        "i",
        "an input dialog: Tab masks the text, Enter confirms it",
    ),
    ("f", "a form: Enter submits it, an empty name is refused"),
    ("v", "a text viewer: wrapped, scrolled by row; Esc closes"),
    ("h / F1", "toggle this help"),
    ("↑ ↓ Home End", "scroll this help, when it doesn't fit;"),
    ("PgUp PgDn", "else move in the list"),
    ("⇧ + those", "scroll the log; ⇧End follows it again"),
    ("q", "quit"),
];
static HELP_FOOTER: &str = "kill <pid> quits through the signal callback";
static INPUT_PROMPT: &str = "type something\n(readline keys: Ctrl-A/E/U/K/W, Ctrl-←/→)";
static MASK_LABEL: &str = "mask the text";
static ERR_EMPTY: &str = "type something first";
static ERR_NO_NAME: &str = "name: required";
static FAMILIES: [&str; 3] = ["any", "ipv4", "ipv6"];
/// The demo form's fields, by index.
const FORM_NAME: usize = 0;
const FORM_PSK: usize = 1;
const FORM_PAUSED: usize = 2;
const FORM_FAMILY: usize = 3;
/// Rows in the demo list; every [`LIST_SEPARATOR_EVERY`]th can't be selected.
const LIST_LEN: usize = 200;
const LIST_SEPARATOR_EVERY: usize = 10;
/// Rows of the text above the list, borders included.
const HEAD_ROWS: u16 = 8;
/// The viewer's text: numbered paragraphs, each long enough to wrap.
const VIEW_PARAGRAPHS: usize = 40;
static VIEW_FILLER: &str = "a paragraph long enough to wrap in most terminals, so that the \
    viewer scrolls by wrapped rows rather than by lines of the text.";
/// The viewer's share of the screen, both ways.
const VIEW_PERCENT: u16 = 80;
/// Messages the demo log keeps, and how often the ticker adds one.
const LOG_CAPACITY: usize = 500;
const LOG_EVERY: Duration = Duration::from_millis(700);
/// The ticker's levels, in turn.
static TICKER_LEVELS: [LogLevel; 6] = [
    LogLevel::Info,
    LogLevel::Debug,
    LogLevel::Notice,
    LogLevel::Info,
    LogLevel::Warn,
    LogLevel::Error,
];
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
    Form(Form),
    View(Scroll),
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
    let log = Arc::new(LogBuffer::new(LOG_CAPACITY).echo(Echo::Stdout, LogLevel::Notice));
    log.notice("guard_demo starting: this line must appear on the shell");
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
    log.notice("the TUI is up: this line must not appear on the shell");
    spawn_ticker(log.clone())?;

    let (mut show_help, mut help_scroll) = (false, Scroll::new());
    let mut cursor = ListCursor::new();
    let mut log_scroll = LogScroll::new();
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
            let [head, list] =
                Layout::vertical([Constraint::Length(HEAD_ROWS), Constraint::Fill(1)])
                    .areas(frame.area());
            let body = Paragraph::new(text).block(Block::bordered().title(" guard_demo "));
            frame.render_widget(body, head);
            let [list, log_area] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Fill(2)]).areas(list);
            draw_list(frame, list, &mut cursor);
            let log_view = LogView::new(&log)
                .stamp(Stamp::Time)
                .block(Block::bordered().title(" LogView "));
            frame.render_stateful_widget(log_view, log_area, &mut log_scroll);
            if show_help {
                let help = HelpPopup::from_rows(HELP_ROWS, Style::new().bold().cyan())
                    .footer(HELP_FOOTER.dark_gray());
                frame.render_stateful_widget(help, frame.area(), &mut help_scroll);
            }
            draw_modal(frame, &mut modal);
        })?;
        if !event::poll(TICK)? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(key) => key,
            Event::Paste(s) => {
                match &mut modal {
                    Modal::Input { editor, error, .. } => {
                        if editor.insert(&s) == EditOutcome::Changed {
                            *error = None;
                        }
                    }
                    Modal::Form(form) => _ = form.insert(&s),
                    Modal::None | Modal::Confirm { .. } | Modal::View(_) => {}
                }
                continue;
            }
            _ => continue,
        };
        // an open dialog takes every key
        if !matches!(modal, Modal::None) {
            if let Some(a) = modal_key(&mut modal, key) {
                log.info(format!("accepted: {a}"));
                accepted = Some(a);
            }
            continue;
        }
        if log_scroll.key_with(key, KeyModifiers::SHIFT) != NavOutcome::Ignored {
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
            KeyCode::Char('f') => modal = Modal::Form(demo_form()),
            KeyCode::Char('v') => modal = Modal::View(Scroll::new()),
            KeyCode::Char('h') | KeyCode::F(1) => show_help = !show_help,
            KeyCode::Char('p') => panic!("UI thread panic, as asked"),
            KeyCode::Char('w') => {
                let _ = thread::Builder::new()
                    .name("worker".into())
                    .spawn(|| panic!("worker panic, as asked"))?
                    .join();
            }
            // the help takes the scroll keys while it is up, else the list
            _ if show_help => _ = help_scroll.key(key),
            _ => _ = cursor.nav(LIST_LEN).selectable(|i| !is_separator(i)).key(key),
        }
    }
    drop(guard);
    eprintln_safe!("guard dropped: this line must appear");
    log.notice("guard_demo done: this line must appear on the shell");
    for p in take_held_panics() {
        eprintln!("held: {p}");
    }
    Ok(())
}

fn draw_modal(frame: &mut Frame, modal: &mut Modal) {
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
        Modal::Form(form) => frame.render_widget(&*form, frame.area()),
        Modal::View(scroll) => {
            let share = Constraint::Percentage(VIEW_PERCENT);
            let area = frame.area().centered(share, share);
            let text = (1..=VIEW_PARAGRAPHS)
                .map(|i| format!("{i}. {VIEW_FILLER}"))
                .collect::<Vec<_>>()
                .join("\n");
            let view = TextView::new(text)
                .wrap(true)
                .indicator("rows")
                .block(Block::bordered().title(" TextView "));
            frame.render_widget(Clear, area);
            frame.render_stateful_widget(view, area, scroll);
        }
    }
}

/// A thread adding a message to `log` every [`LOG_EVERY`] until the demo quits.
fn spawn_ticker(log: Arc<LogBuffer>) -> Result<()> {
    thread::Builder::new().name("ticker".into()).spawn(move || {
        for (n, level) in (1..).zip(TICKER_LEVELS.iter().cycle()) {
            if QUIT.load(Ordering::Acquire) {
                break;
            }
            log.push(*level, format!("tick {n}: a {level} message from the ticker thread"));
            thread::sleep(LOG_EVERY);
        }
    })?;
    Ok(())
}

/// Only the rows in the cursor's window are formatted, as with a huge list.
fn draw_list(frame: &mut Frame, area: Rect, cursor: &mut ListCursor) {
    let block = Block::bordered().title(" ListCursor ");
    let window = cursor.window(LIST_LEN, block.inner(area).height.into());
    let title = format!(" {} ", range_label("rows", window.clone(), LIST_LEN));
    let items: Vec<ListItem> = window
        .map(|i| match is_separator(i) {
            true => ListItem::new(format!("── {i} ──")).dark_gray(),
            false => ListItem::new(format!("row {i}")),
        })
        .collect();
    let list = List::new(items)
        .block(block.title_bottom(title))
        .highlight_style(Style::new().reversed());
    let mut state = ListState::default().with_selected(cursor.selected_in_window());
    frame.render_stateful_widget(list, area, &mut state);
}

fn is_separator(i: usize) -> bool {
    i.is_multiple_of(LIST_SEPARATOR_EVERY)
}

fn demo_form() -> Form {
    Form::new(
        "form",
        vec![
            Field::text("name", ""),
            Field::editor("pre-shared key", LineEditor::default().secret(true)),
            Field::checkbox("start paused", false),
            Field::choice("family", FAMILIES, 0),
        ],
    )
    .buttons("submit", "cancel")
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
        Modal::Form(form) => match form.handle_key(key) {
            FormAction::Submit if form.text(FORM_NAME).trim().is_empty() => {
                form.fail(FORM_NAME, ERR_NO_NAME);
                None
            }
            FormAction::Submit => {
                let summary = format!(
                    "{} (key: {} chars, paused: {}, family: {})",
                    form.text(FORM_NAME).trim(),
                    form.text(FORM_PSK).chars().count(),
                    form.checked(FORM_PAUSED),
                    FAMILIES[form.selected(FORM_FAMILY)],
                );
                *modal = Modal::None;
                Some(summary)
            }
            FormAction::Cancel => {
                *modal = Modal::None;
                None
            }
            FormAction::Ignored
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                *modal = Modal::None;
                None
            }
            FormAction::Changed | FormAction::Handled | FormAction::Ignored => None,
        },
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
        Modal::View(scroll) => {
            let close = key.kind == KeyEventKind::Press
                && matches!(key.code, KeyCode::Esc | KeyCode::Char('v' | 'q'));
            match close {
                true => *modal = Modal::None,
                false => _ = scroll.key(key),
            }
            None
        }
    }
}
