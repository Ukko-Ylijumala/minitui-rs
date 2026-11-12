// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
stderr while a guard is up (`GuardBuilder::capture_stderr`): file descriptor
2 becomes a pipe, and a thread of its own turns what is written to it into
the lines of a [`LogBuffer`]. For an app that grew a TUI after it grew its
diagnostics: every `eprintln!` of its own and of the crates it uses, and
whatever a child process says, lands in the log pane instead of on the
screen, with no call site changed. The guard draws on stdout, so no frame
goes down the pipe.

- the redirect is of the descriptor, not of Rust's `stderr()`: it covers C
  libraries and children too;
- file descriptor 2 is put back by the guard's teardown, so also by the
  panic hook before a report is printed and by the signal handler before
  the process dies: what is printed then reaches the shell;
- putting it back never waits on a writer: a child that inherited the pipe
  and outlives the guard keeps its end, and its later lines go nowhere;
- the lines are in the buffer for the app to print once the terminal is
  back; they are never echoed (an echo on stderr would come straight back
  in).

What it cannot save is the last words of a process that aborts (a stack
overflow, a panic inside a panic): they are written into the pipe and die
with the reader. The terminal is left in raw mode then anyway.
*/

use std::io::{ErrorKind, PipeReader, PipeWriter, Read, Result, pipe, stderr};
use std::iter;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::{Errno, ioctl_fionbio};
use rustix::stdio::dup2_stderr;
use unicode_width::UnicodeWidthChar;

use crate::terminal::lock;
use crate::{LogBuffer, LogLevel};

/* ===== consts and statics ================================================= */

/// The live capture; `None` while stderr is its own.
static CAPTURE: Mutex<Option<Capture>> = Mutex::new(None);

static THREAD_NAME: &str = "minitui-stderr";

/// Read from the pipe this much at a time (a Linux pipe holds 64 KiB).
const CHUNK: usize = 16 * 1024;
/// A line longer than this is cut into several: a stream that never ends a
/// line must not grow without bound.
const LINE_CAP: usize = 16 * 1024;
/// The columns between tab stops, as a terminal has them.
const TAB_COLS: usize = 8;
/**
How long putting stderr back waits for the reader to take what is left in
the pipe. It is done in microseconds unless the log buffer's lock is held
by the very thread that is waiting (a panic while reading the buffer, the
hook then restoring the terminal): the wait gives up, and the lines arrive
when the lock is free.
*/
const DRAIN_LIMIT: Duration = Duration::from_millis(250);

const ESC: char = '\u{1b}';
const BEL: char = '\u{7}';

/* ===== types ============================================================== */

/// The level of a captured line, from its text.
pub(crate) type LevelFn = fn(&str) -> LogLevel;

/// What putting stderr back needs.
struct Capture {
    /// The stderr the process had.
    saved: OwnedFd,
    /// Dropping it tells the reader to finish.
    stop: PipeWriter,
    /// Disconnects when the reader has finished.
    done: Receiver<()>,
    reader: ThreadId,
}

/// Bytes off the pipe, cut into lines.
#[derive(Default)]
struct Lines {
    pending: Vec<u8>,
}

/// Where [`clean`] is in an escape sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seq {
    Text,
    /// After ESC.
    Escape,
    /// `ESC [ … final`: colors, cursor moves, erasing.
    Csi,
    /// `ESC ] … BEL` or `… ESC \`: titles, hyperlinks.
    Osc,
    /// After an ESC inside an OSC: its closing `ESC \`.
    OscEscape,
}

/* ===== start and stop ===================================================== */

/**
Point file descriptor 2 at a pipe and start the thread that reads it into
`log`. On failure stderr is as it was. One capture at a time: the guard is
the only caller, and there is one guard.
*/
pub(crate) fn start(log: Arc<LogBuffer>, level: LevelFn) -> Result<()> {
    let saved = stderr().as_fd().try_clone_to_owned()?;
    let (from_stderr, to_reader) = pipe()?;
    let (stopped, stop) = pipe()?;
    // the read end only: a writer must block on a full pipe, not fail
    ioctl_fionbio(&from_stderr, true)?;
    let (finished, done) = channel();
    let reader = thread::Builder::new()
        .name(THREAD_NAME.into())
        .spawn(move || read_loop(from_stderr, stopped, &log, level, finished))?;
    // a failure drops `stop`, which ends the reader
    dup2_stderr(&to_reader)?;
    *lock(&CAPTURE) = Some(Capture {
        saved,
        stop,
        done,
        reader: reader.thread().id(),
    });
    Ok(())
}

/**
Put the process's own stderr back and wait (briefly, see [`DRAIN_LIMIT`])
for the lines still in the pipe. A no-op without a capture; safe from any
thread, the reader's own included.
*/
pub(crate) fn stop() {
    let Some(capture) = lock(&CAPTURE).take() else {
        return;
    };
    let Capture { saved, stop, done, reader } = capture;
    // closes the pipe's write end that fd 2 was; nothing to do about a failure
    let _ = dup2_stderr(&saved);
    drop(stop);
    if thread::current().id() != reader {
        let _ = done.recv_timeout(DRAIN_LIMIT);
    }
}

/* ===== the reader ========================================================= */

/// The reader thread: `pipe` into `log` line by line, until `stopped` hangs up.
fn read_loop(mut pipe: PipeReader, stopped: PipeReader, log: &LogBuffer, level: LevelFn, _finished: Sender<()>) {
    let mut lines = Lines::default();
    let mut chunk = vec![0u8; CHUNK];
    let mut emit = |line: String| log.push_quiet(level(&line), line);
    loop {
        let stopping = {
            let mut fds = [PollFd::new(&pipe, PollFlags::IN), PollFd::new(&stopped, PollFlags::IN)];
            match poll(&mut fds, None) {
                Ok(_) => !fds[1].revents().is_empty(),
                Err(Errno::INTR) => continue,
                // nothing left to wait on: take what is there and leave
                Err(_) => true,
            }
        };
        // after `stopped`, too: what was written before stderr went back is in the pipe
        let open = drain(&mut pipe, &mut chunk, &mut lines, &mut emit);
        if stopping || !open {
            break;
        }
    }
    lines.flush(&mut emit);
}

/// Read `pipe` until it has no more for now; false once every write end is closed.
fn drain(pipe: &mut PipeReader, chunk: &mut [u8], lines: &mut Lines, emit: &mut impl FnMut(String)) -> bool {
    loop {
        match pipe.read(chunk) {
            Ok(0) => return false,
            Ok(n) => lines.feed(&chunk[..n], emit),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

impl Lines {
    /// More bytes: every line they complete goes to `emit`.
    fn feed(&mut self, mut bytes: &[u8], emit: &mut impl FnMut(String)) {
        while let Some(at) = bytes.iter().position(|&b| b == b'\n') {
            self.pending.extend_from_slice(&bytes[..at]);
            self.flush(emit);
            bytes = &bytes[at + 1..];
        }
        self.pending.extend_from_slice(bytes);
        if self.pending.len() >= LINE_CAP {
            self.flush(emit);
        }
    }

    /// What is pending, as a line; an empty one is dropped.
    fn flush(&mut self, emit: &mut impl FnMut(String)) {
        let line = clean(&self.pending);
        self.pending.clear();
        if !line.is_empty() {
            emit(line);
        }
    }
}

/**
A line as a terminal would have left it, for a log pane: what follows the
last carriage return (a progress line redrawn in place shows its final
state), escape sequences left out (colors, cursor moves, titles), tabs to
the next of every [`TAB_COLS`]th column, other control characters dropped,
bytes that are not UTF-8 as `�`.
*/
fn clean(bytes: &[u8]) -> String {
    let end = bytes.iter().rposition(|&b| b != b'\r').map_or(0, |at| at + 1);
    let bytes = &bytes[..end];
    let start = bytes.iter().rposition(|&b| b == b'\r').map_or(0, |at| at + 1);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let mut out = String::with_capacity(text.len());
    let (mut seq, mut col) = (Seq::Text, 0usize);
    for c in text.chars() {
        seq = match (seq, c) {
            (Seq::Text, ESC) => Seq::Escape,
            (Seq::Text, '\t') => {
                let to = TAB_COLS - col % TAB_COLS;
                out.extend(iter::repeat_n(' ', to));
                col += to;
                Seq::Text
            }
            (Seq::Text, c) => {
                if !c.is_control() {
                    out.push(c);
                    col += c.width().unwrap_or(0);
                }
                Seq::Text
            }
            (Seq::Escape, '[') => Seq::Csi,
            (Seq::Escape, ']') => Seq::Osc,
            // a two-character sequence
            (Seq::Escape, _) => Seq::Text,
            (Seq::Csi, '@'..='~') => Seq::Text,
            (Seq::Csi, _) => Seq::Csi,
            (Seq::Osc, BEL) => Seq::Text,
            (Seq::Osc, ESC) => Seq::OscEscape,
            (Seq::Osc, _) => Seq::Osc,
            (Seq::OscEscape, _) => Seq::Text,
        };
    }
    out.truncate(out.trim_end().len());
    out
}

/* ===== tests ============================================================== */

/*
File descriptor 2 is the whole process's, so these run one at a time with
the guard's own tests (TEST_LOCK), and each first points it at a pipe of its
own, standing for the terminal: what a capture lets through is read back
from there, and the test runner's stderr gets nothing. They write with
`stderr().write_all`: `eprintln!` in a test goes to libtest's capture and
never reaches the descriptor.
*/
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use std::process::Command;
    use std::time::Instant;

    use crate::Echo;
    use crate::terminal::TEST_LOCK;

    /// A pipe in stderr's place for a test's lifetime; the real one is back on drop.
    pub(crate) struct FakeTerminal {
        real: OwnedFd,
        shown: PipeReader,
    }

    impl FakeTerminal {
        pub(crate) fn new() -> Self {
            let real = stderr().as_fd().try_clone_to_owned().unwrap();
            let (shown, screen) = pipe().unwrap();
            ioctl_fionbio(&shown, true).unwrap();
            dup2_stderr(&screen).unwrap();
            Self { real, shown }
        }

        /// What has reached the "terminal" since the last call.
        pub(crate) fn shown(&mut self) -> String {
            let mut bytes = Vec::new();
            // non-blocking: it ends in WouldBlock while fd 2 holds the write end
            let _ = self.shown.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }

    impl Drop for FakeTerminal {
        fn drop(&mut self) {
            let _ = dup2_stderr(&self.real);
        }
    }

    /// Which file fd 2 is now.
    pub(crate) fn stderr_id() -> (u64, u64) {
        let meta = File::from(stderr().as_fd().try_clone_to_owned().unwrap()).metadata().unwrap();
        (meta.dev(), meta.ino())
    }

    fn messages(log: &LogBuffer) -> Vec<(LogLevel, String)> {
        log.with(|entries| entries.iter().map(|e| (e.level, e.msg.clone())).collect())
    }

    fn cleaned(text: &str) -> String {
        clean(text.as_bytes())
    }

    #[test]
    fn what_is_written_to_stderr_is_in_the_log_and_not_on_the_terminal() {
        let _serial = lock(&TEST_LOCK);
        let mut terminal = FakeTerminal::new();
        let before = stderr_id();
        let log = Arc::new(LogBuffer::new(100));

        start(log.clone(), LogLevel::of_line).unwrap();
        assert_ne!(stderr_id(), before, "fd 2 is the pipe");
        stderr().write_all(b"minitui-capture one\nwarning: minitui-capture two\nminitui-capture par").unwrap();
        stderr().write_all(b"tial").unwrap();
        stop();

        assert_eq!(stderr_id(), before, "fd 2 is the terminal again");
        stderr().write_all(b"minitui-capture after\n").unwrap();
        assert_eq!(terminal.shown(), "minitui-capture after\n");
        assert_eq!(
            messages(&log),
            [
                (LogLevel::Info, "minitui-capture one".to_string()),
                (LogLevel::Warn, "warning: minitui-capture two".to_string()),
                // a line never ended is still a line
                (LogLevel::Info, "minitui-capture partial".to_string()),
            ]
        );
        assert!(lock(&CAPTURE).is_none());
        stop(); // twice is once
        assert_eq!(stderr_id(), before);
    }

    #[test]
    fn a_child_that_keeps_the_pipe_does_not_hold_the_teardown() {
        let _serial = lock(&TEST_LOCK);
        let _terminal = FakeTerminal::new();
        let log = Arc::new(LogBuffer::new(100));

        start(log.clone(), LogLevel::of_line).unwrap();
        // it inherits fd 2, the pipe, and says nothing for longer than the test waits
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        stderr().write_all(b"before the child is done\n").unwrap();
        let began = Instant::now();
        stop();
        let took = began.elapsed();
        child.kill().unwrap();
        child.wait().unwrap();

        assert!(took < DRAIN_LIMIT, "no EOF to wait for: {took:?}");
        assert_eq!(messages(&log), [(LogLevel::Info, "before the child is done".to_string())]);
    }

    #[test]
    fn a_capture_never_echoes() {
        let _serial = lock(&TEST_LOCK);
        let mut terminal = FakeTerminal::new();
        // an echo on stderr would be read back in, forever
        let log = Arc::new(LogBuffer::new(100).echo(Echo::Stderr, LogLevel::Trace));

        start(log.clone(), LogLevel::of_line).unwrap();
        stderr().write_all(b"once\n").unwrap();
        stop();

        assert_eq!(messages(&log).len(), 1);
        assert_eq!(terminal.shown(), "");
    }

    #[test]
    fn lines_are_cut_at_newlines_whatever_the_writes() {
        let mut lines = Lines::default();
        let mut got: Vec<String> = Vec::new();
        let mut emit = |line: String| got.push(line);
        lines.feed(b"one\ntw", &mut emit);
        lines.feed(b"o\n\n\nthr", &mut emit);
        lines.feed(b"ee", &mut emit);
        lines.flush(&mut emit);
        lines.flush(&mut emit);
        assert_eq!(got, ["one", "two", "three"], "blank lines dropped");

        // no newline, ever: cut at the cap rather than kept
        let mut got: Vec<String> = Vec::new();
        let mut emit = |line: String| got.push(line);
        for _ in 0..3 {
            lines.feed(&vec![b'x'; LINE_CAP / 2], &mut emit);
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), LINE_CAP);
        assert_eq!(lines.pending.len(), LINE_CAP / 2);
    }

    #[test]
    fn a_line_as_the_terminal_would_have_left_it() {
        assert_eq!(cleaned("plain"), "plain");
        assert_eq!(cleaned("windows\r"), "windows");
        assert_eq!(cleaned(" 10%\r 50%\r100% done\r\r"), "100% done", "redrawn in place");
        assert_eq!(cleaned("\x1b[1;31merror\x1b[0m: no\x1b[K"), "error: no");
        assert_eq!(cleaned("\x1b]0;a title\x07shown\x1b]8;;http://x\x1b\\link"), "shownlink");
        assert_eq!(cleaned("a\tb\x07\x00c"), "a       bc");
        assert_eq!(cleaned("日\tx"), "日      x", "a tab stop is a column, not a char");
        assert_eq!(clean(b"caf\xe9"), "caf\u{fffd}");
        assert_eq!(cleaned("trailing   \t"), "trailing");
        assert_eq!(cleaned("\x1b[2K\r"), "");
    }
}
