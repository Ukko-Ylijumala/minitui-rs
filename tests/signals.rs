// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Real signal delivery to a [`SignalHandler`]. Each case runs in a child
process (this test binary again, running only `child` with `CHILD_MODE`
set), since a terminating signal would take the test runner down with it.
The child installs the handler, says `ready`, and loops until its quit
flag is set or the signal kills it; the parent sends `kill` and checks how
it ended.
*/

use std::env;
use std::io::{BufRead, BufReader, Lines};
use std::os::unix::process::ExitStatusExt;
use std::process::{self, Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use minitui::{Signal, SignalHandler};

/* ===== consts and statics ================================================= */

static CHILD_MODE: &str = "MINITUI_SIGNAL_CHILD";
static READY: &str = "ready";
/// Printed by the child's callback: `got SIGTERM`.
static GOT: &str = "got";
static QUIT: AtomicBool = AtomicBool::new(false);

const CHILD_TICK: Duration = Duration::from_millis(10);
/// The child gives up (exit 3) if nothing ends it by then.
const CHILD_LIMIT: Duration = Duration::from_secs(10);
const CHILD_GAVE_UP: i32 = 3;

/* ===== child side ========================================================= */

/// The child's entry point; a no-op when run as an ordinary test.
#[test]
fn child() {
    let Ok(mode) = env::var(CHILD_MODE) else {
        return;
    };
    let builder = SignalHandler::builder();
    match mode.as_str() {
        // the callback quits: a graceful exit
        "graceful" => builder.on_signal(|sig| {
            println!("{GOT} {sig}");
            QUIT.store(true, Ordering::Release);
        }),
        // the callback never quits: the second signal must force it
        "stubborn" => builder.on_signal(|sig| println!("{GOT} {sig}")),
        "default" => builder,
        other => panic!("unknown {CHILD_MODE}: {other}"),
    }
    .install()
    .expect("install");
    println!("{READY}");

    let start = Instant::now();
    while !QUIT.load(Ordering::Acquire) {
        if start.elapsed() > CHILD_LIMIT {
            process::exit(CHILD_GAVE_UP);
        }
        thread::sleep(CHILD_TICK);
    }
}

/* ===== parent side ======================================================== */

fn spawn(mode: &str) -> (Child, Lines<BufReader<ChildStdout>>) {
    let mut child = Command::new(env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CHILD_MODE, mode)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn child");
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    wait_for(&mut lines, READY);
    (child, lines)
}

/**
Read the child's stdout up to `want`, returning the line from there on.
Not necessarily at a line start: libtest prints `test child ... ` with no
newline before the test's own output.
*/
fn wait_for(lines: &mut Lines<BufReader<ChildStdout>>, want: &str) -> String {
    lines
        .map_while(Result::ok)
        .find_map(|l| l.find(want).map(|at| l[at..].to_string()))
        .unwrap_or_else(|| panic!("child never said {want:?}"))
}

fn kill(child: &Child, sig: Signal) {
    let status = Command::new("kill")
        .args([format!("-{}", sig.number()), child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(status.success());
}

fn finish(mut child: Child) -> ExitStatus {
    child.wait().expect("wait child")
}

#[test]
fn a_callback_gets_the_signal_and_the_app_exits_on_its_own() {
    let (child, mut lines) = spawn("graceful");
    kill(&child, Signal::Terminate);
    assert_eq!(wait_for(&mut lines, GOT), "got SIGTERM");
    let status = finish(child);
    assert!(status.success(), "graceful exit, not killed: {status}");
}

#[test]
fn a_repeated_signal_forces_a_hung_shutdown() {
    let (child, mut lines) = spawn("stubborn");
    kill(&child, Signal::Hangup);
    assert_eq!(wait_for(&mut lines, GOT), "got SIGHUP");
    kill(&child, Signal::Terminate);
    let status = finish(child);
    assert_eq!(
        status.signal(),
        Some(Signal::Terminate.number()),
        "{status}"
    );
}

#[test]
fn without_a_callback_the_signal_kills_as_by_default() {
    let (child, _lines) = spawn("default");
    kill(&child, Signal::Interrupt);
    let status = finish(child);
    assert_eq!(
        status.signal(),
        Some(Signal::Interrupt.number()),
        "{status}"
    );
}
