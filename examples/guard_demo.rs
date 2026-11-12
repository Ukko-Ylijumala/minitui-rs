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
a [`Form`], `v` a wrapped [`TextView`]; `c` clears the log and `q` quits,
both after a y/N [`Prompt`] in the [`StatusBar`] (it lapses in 5 s, and
results show there as a [`Toast`]). Shift with ↑ ↓ Home End PgUp PgDn
scrolls the [`LogView`] beside the list, which a ticker thread feeds.
Under it a [`TimeChart`] draws two made-up signals, one with a gap every
20 s; the newest of it is a [`braille_spark`] in the footer. Keys come in
through an [`Input`] thread; it, the ticker and the signal callback wake
the loop through a [`Waker`], and Ctrl-C asks to quit like `q`. Ctrl-←/→
move the [`Split`] between the list and the log; the [`Breadcrumb`] on top
shows the selected row.
The shell must come back clean after each, with the panic report on it.
In `held` mode a worker panic leaves the TUI up and shows the held panic.
The log's first and last lines echo on the shell, before the TUI comes up
and after it is gone.

stderr is captured ([`GuardBuilder::capture_stderr`](minitui::GuardBuilder::capture_stderr)):
`e` has a worker thread `eprintln!` two lines, which must show in the log
pane, one of them as a warning, and nowhere on the screen. A panic report
(`p`, or `w` outside `held` mode) must still reach the shell, and on the
way out the demo says how many lines it caught.

Signals: `kill <pid>` (shown on screen) quits through the callback; in
`bare` mode it restores the terminal and kills the demo by the signal.
*/

use std::collections::VecDeque;
use std::env;
use std::f64::consts::TAU;
use std::io::Result;
use std::mem;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
};
use minitui::{
    Answer, Breadcrumb, ConfirmDialog, DrawErrors, Echo, EditOutcome, Field, Form, FormAction,
    HelpPopup, Input, InputDialog, KeyHints, LineEditor, ListCursor, LogBuffer, LogLevel,
    LogScroll, LogView, NavOutcome, PanicPolicy, Prompt, Reply, Scroll, SignalHandler, Split,
    Stamp, StatusBar, TerminalGuard, TextView, TimeChart, Toast, Trace, Waker, braille_spark,
    confirm_answer,
    eprintln_safe, held_panics, is_ctrl_c, pill, range_label, take_held_panics, tui_active,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

/// The loop's heartbeat: it draws at least this often, and samples the chart.
const TICK: Duration = Duration::from_millis(100);
/// The footer's legend; what doesn't fit drops out from the end.
static HINTS: &[(&str, &str)] = &[
    ("h", "help"),
    ("q", "quit"),
    ("i", "input"),
    ("f", "form"),
    ("v", "view"),
    ("c", "clear log"),
    ("e", "stderr"),
    ("⇧↑↓", "scroll log"),
    ("^←→", "split"),
    ("p/w", "panic"),
];
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
    ("Ctrl-← →", "move the split between the list and the log"),
    ("e", "a worker writes two lines to stderr: they show in the log"),
    ("c", "clear the log (asks y/N)"),
    ("q / Ctrl-C", "quit (asks y/N)"),
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
/// Rows of the breadcrumb, of the text above the list (borders included)
/// and of the footer.
const CRUMB_ROWS: u16 = 1;
const HEAD_ROWS: u16 = 6;
const FOOTER_ROWS: u16 = 1;
/// The list's share of the width, which Ctrl-←/→ move, and its least columns.
const LIST_PCT: u16 = 33;
const LIST_MIN_COLS: u16 = 16;
/// How long a result stays in the footer.
const TOAST_TTL: Duration = Duration::from_secs(4);
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
/// The chart: a sample every [`SAMPLE_EVERY`], [`CHART_SPAN`]'s worth kept.
const SAMPLE_EVERY: Duration = Duration::from_millis(250);
const CHART_SPAN: Duration = Duration::from_secs(60);
const CHART_ROWS: u16 = 12;
/// The signals' periods and levels, and the gap in the first: samples
/// [`GAP_FROM`] to [`GAP_TO`] of every [`GAP_EVERY`].
const SLOW_PERIOD: f64 = 20.0;
const FAST_PERIOD: f64 = 7.0;
const SLOW_MID: f64 = 50.0;
const SLOW_AMPLITUDE: f64 = 40.0;
const FAST_MID: f64 = 30.0;
const FAST_AMPLITUDE: f64 = 25.0;
const GAP_EVERY: u64 = 80;
const GAP_FROM: u64 = 70;
const GAP_TO: u64 = 74;
/// The footer's sparkline: cells, and its fixed top.
const SPARK_CELLS: usize = 12;
const SPARK_MAX: f64 = 100.0;
/// What both of `e`'s lines say, to count them on the way out.
static STRAY: &str = "stderr";
/// The quit path: set by `q`, a signal and, through `on_panic`, by any panic.
static QUIT: AtomicBool = AtomicBool::new(false);

/// What the footer prompt asks about.
#[derive(Debug, Clone, Copy)]
enum Ask {
    Quit,
    ClearLog,
}

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
    // the loop sleeps on this between frames; whatever changes the screen wakes it
    let waker = Waker::new();
    // the signal handler first: a signal from here on can't strand the terminal
    let wake = waker.clone();
    match bare {
        true => SignalHandler::install()?,
        false => SignalHandler::builder()
            .on_signal(move |sig| {
                eprintln_safe!("this line must not appear: {sig} arrived while the TUI is up");
                QUIT.store(true, Ordering::Release);
                wake.wake();
            })
            .install()?,
    }
    let log = Arc::new(LogBuffer::new(LOG_CAPACITY).echo(Echo::Stdout, LogLevel::Notice));
    log.notice("guard_demo starting: this line must appear on the shell");
    let mut builder = TerminalGuard::builder()
        .keyboard_enhancement(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        // from here to the guard's end, stderr is lines of the log
        .capture_stderr(log.clone())
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
    let input = Input::spawn(&waker)?;
    spawn_ticker(log.clone(), waker.clone())?;
    let mut draw_errors = DrawErrors::new();

    let (mut show_help, mut help_scroll) = (false, Scroll::new());
    let mut cursor = ListCursor::new();
    let mut split = Split::horizontal(LIST_PCT).min_cells(LIST_MIN_COLS);
    let mut log_scroll = LogScroll::new();
    let mut modal = Modal::None;
    let mut prompt = Prompt::new();
    let mut toast = Toast::new().ttl(TOAST_TTL);
    let keep = (CHART_SPAN.as_millis() / SAMPLE_EVERY.as_millis()) as usize;
    let mut samples: VecDeque<(f64, f64)> = VecDeque::with_capacity(keep + 1);
    let (mut taken, mut next_sample) = (0u64, Instant::now());
    while tui_active() && !QUIT.load(Ordering::Acquire) {
        while Instant::now() >= next_sample {
            samples.push_back(signals(taken));
            if samples.len() > keep {
                samples.pop_front();
            }
            taken += 1;
            next_sample += SAMPLE_EVERY;
        }
        let held = held_panics();
        let head = format!("{policy:?} · pid {} · bare: {bare}", process::id());
        let text = match held.last() {
            Some(p) => format!("{head}\n\nheld: {}", p.message),
            None => head,
        };
        let trail = match cursor.selected() {
            Some(row) => format!("{policy:?} / ListCursor / row {row}"),
            None => format!("{policy:?} / ListCursor"),
        };
        let drawn = guard.term.draw(|frame| {
            let [crumb, head, list, footer] = Layout::vertical([
                Constraint::Length(CRUMB_ROWS),
                Constraint::Length(HEAD_ROWS),
                Constraint::Fill(1),
                Constraint::Length(FOOTER_ROWS),
            ])
            .areas(frame.area());
            frame.render_widget(Breadcrumb::new(&trail).badge("guard_demo"), crumb);
            let body = Paragraph::new(text).block(Block::bordered().title(" guard_demo "));
            frame.render_widget(body, head);
            let [list, right_area] = split.areas(list);
            let [log_area, chart_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(CHART_ROWS)])
                    .areas(right_area);
            draw_list(frame, list, &mut cursor);
            let log_view = LogView::new(&log)
                .stamp(Stamp::Time)
                .block(Block::bordered().title(" LogView "));
            frame.render_stateful_widget(log_view, log_area, &mut log_scroll);
            let chart = TimeChart::new([
                Trace::samples("slow", samples.iter().map(|s| s.0), SAMPLE_EVERY)
                    .style(Color::Cyan),
                Trace::samples("fast", samples.iter().map(|s| s.1), SAMPLE_EVERY)
                    .style(Color::Yellow),
            ])
            .span(CHART_SPAN)
            .nice(true)
            .block(Block::bordered().title(" TimeChart "));
            frame.render_widget(chart, chart_area);
            let slow: Vec<f64> = samples.iter().map(|s| s.0).collect();
            let spark = braille_spark(&slow, SPARK_CELLS, Some(SPARK_MAX));
            let mut right = vec![
                spark.cyan(),
                format!(" {} messages ", log.len()).dark_gray(),
            ];
            if !log_scroll.following() {
                right.insert(0, pill("log held", Style::new().black().on_cyan()));
            }
            let bar = StatusBar::new()
                .left(toast.line().unwrap_or_default())
                .hints(KeyHints::new(HINTS).label_style(Style::new().dark_gray()))
                .right(right)
                .prompt(prompt.line());
            frame.render_widget(bar, footer);
            if show_help {
                let help = HelpPopup::from_rows(HELP_ROWS, Style::new().bold().cyan())
                    .footer(HELP_FOOTER.dark_gray());
                frame.render_stateful_widget(help, frame.area(), &mut help_scroll);
            }
            draw_modal(frame, &mut modal);
        });
        if let Some(report) = draw_errors.record(drawn)? {
            log.push(report.level(), report.to_string());
        }
        // a key, a wake (the ticker, a signal) or the next sample: draw again
        let Some(event) = input.next(TICK) else {
            continue;
        };
        let key = match event {
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
        // a question in the footer takes the next key
        match prompt.key(key) {
            Reply::NotAsked => {}
            Reply::Wait => continue,
            Reply::Yes(Ask::Quit) => {
                QUIT.store(true, Ordering::Release);
                continue;
            }
            Reply::Yes(Ask::ClearLog) => {
                log.clear();
                toast.notice("log cleared");
                continue;
            }
            Reply::No(_) => {
                toast.info("cancelled");
                continue;
            }
        }
        // an open dialog takes every key
        if !matches!(modal, Modal::None) {
            if let Some(a) = modal_key(&mut modal, key) {
                log.info(format!("accepted: {a}"));
                toast.notice(format!("accepted: {a}"));
            }
            continue;
        }
        if log_scroll.key_with(key, KeyModifiers::SHIFT) != NavOutcome::Ignored {
            continue;
        }
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // raw mode: Ctrl-C is a key, and asks like `q` (its code is `c` too)
        if is_ctrl_c(key) {
            prompt.ask(Ask::Quit, "quit the demo?");
            continue;
        }
        if let Some(pct) = split.key(key) {
            toast.info(format!("list: {pct}% of the width"));
            continue;
        }
        match key.code {
            KeyCode::Char('q') => prompt.ask(Ask::Quit, "quit the demo?"),
            KeyCode::Char('c') => prompt.ask(Ask::ClearLog, "clear the log?"),
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
            KeyCode::Char('e') => {
                let wake = waker.clone();
                thread::Builder::new().name("stray".into()).spawn(move || {
                    eprintln!("a stray line on stderr, from a worker thread");
                    eprintln!("warning: \x1b[33mand one on stderr that calls itself a warning\x1b[0m");
                    wake.wake();
                })?;
            }
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
    drop(input);
    drop(guard);
    eprintln_safe!("guard dropped: this line must appear");
    let stray = log.with(|entries| entries.iter().filter(|e| e.msg.contains(STRAY)).count());
    eprintln!("stderr is the shell's again; {stray} lines of it were caught in the log");
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

/// The chart's two signals at sample `n`; the slow one has a gap now and then.
fn signals(n: u64) -> (f64, f64) {
    let t = n as f64 * SAMPLE_EVERY.as_secs_f64();
    let slow = match n % GAP_EVERY {
        GAP_FROM..GAP_TO => f64::NAN,
        _ => SLOW_MID + SLOW_AMPLITUDE * (t * TAU / SLOW_PERIOD).sin(),
    };
    (slow, FAST_MID + FAST_AMPLITUDE * (t * TAU / FAST_PERIOD).sin())
}

/// A thread adding a message to `log` every [`LOG_EVERY`], waking the loop, until the demo quits.
fn spawn_ticker(log: Arc<LogBuffer>, waker: Waker) -> Result<()> {
    thread::Builder::new().name("ticker".into()).spawn(move || {
        for (n, level) in (1..).zip(TICKER_LEVELS.iter().cycle()) {
            if QUIT.load(Ordering::Acquire) {
                break;
            }
            log.push(*level, format!("tick {n}: a {level} message from the ticker thread"));
            waker.wake();
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
