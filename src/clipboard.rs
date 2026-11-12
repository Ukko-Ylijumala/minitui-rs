// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Copying to the clipboard by OSC 52: an escape sequence asking the terminal
to put a text on the clipboard of the machine it runs on. No clipboard tool
and no display are needed where the app runs, so it is the one way that
works for a TUI on a server, through SSH: the sequence travels with the
screen.

Whether it works is the terminal's choice. kitty, foot, alacritty, WezTerm,
Ghostty and Windows Terminal take it as they come; iTerm2 and xterm when
allowed in their settings; inside tmux it takes `set -g set-clipboard on`.
A terminal that will not simply ignores it, and none of them answers: an
app can say it asked, not that it worked.
*/

use std::io::{Error, ErrorKind, Result, Write, stdout};

/* ===== consts and statics ================================================= */

/// `ESC ] 52 ; c ;`: set the selection `c`, the clipboard.
static OSC52_START: &str = "\x1b]52;c;";
/// The string terminator, `ESC \`.
static ST: &str = "\x1b\\";
/// The longest sequence sent: xterm takes no longer one, and nobody means
/// to copy more than this from a TUI.
const SEQUENCE_MAX: usize = 100_000;
static BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// base64: three bytes in, four characters out.
const BASE64_IN: usize = 3;
const BASE64_OUT: usize = 4;
const BASE64_PAD: char = '=';

static ERR_TOO_LONG: &str = "too long to copy through the terminal";

/// The longest text [`copy_to_clipboard`] sends, in bytes.
pub const OSC52_MAX_BYTES: usize = (SEQUENCE_MAX - OSC52_START.len() - ST.len()) / BASE64_OUT * BASE64_IN;

/* ===== code =============================================================== */

/**
Ask the terminal to put `text` on the clipboard, by OSC 52 on stdout (see
the module docs for where that works). Call it from the thread that draws,
between frames: the sequence must not land inside one.

Fails with [`ErrorKind::InvalidInput`] for a text longer than
[`OSC52_MAX_BYTES`], with nothing written, and with stdout's own error.
Success means the terminal was asked: say `copied` with that in mind.

```no_run
use minitui::copy_to_clipboard;

let path = "/home/u12/public_html/wp-content/uploads/x.php";
match copy_to_clipboard(path) {
    Ok(()) => { /* toast.info("path copied") */ }
    Err(e) => { /* toast.warn(e.to_string()) */ }
}
```
*/
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    write_osc52(&mut stdout().lock(), text)
}

/**
The OSC 52 sequence that sets the clipboard to `text`, for an app that
writes to the terminal its own way; `None` for a text longer than
[`OSC52_MAX_BYTES`].

```
assert_eq!(minitui::osc52("hi").as_deref(), Some("\x1b]52;c;aGk=\x1b\\"));
```
*/
pub fn osc52(text: &str) -> Option<String> {
    (text.len() <= OSC52_MAX_BYTES).then(|| format!("{OSC52_START}{}{ST}", base64(text.as_bytes())))
}

fn write_osc52(out: &mut impl Write, text: &str) -> Result<()> {
    let sequence = osc52(text).ok_or_else(|| Error::new(ErrorKind::InvalidInput, ERR_TOO_LONG))?;
    out.write_all(sequence.as_bytes())?;
    out.flush()
}

/// Standard base64, padded.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(BASE64_IN) * BASE64_OUT);
    for chunk in bytes.chunks(BASE64_IN) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let bits = byte(0) << 16 | byte(1) << 8 | byte(2);
        for i in 0..BASE64_OUT {
            // n bytes fill n + 1 characters; the rest of the four is padding
            out.push(match i <= chunk.len() {
                true => char::from(BASE64[(bits >> (18 - 6 * i) & 0x3f) as usize]),
                false => BASE64_PAD,
            });
        }
    }
    out
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_to_four() {
        for (text, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("ä", "w6Q="),
        ] {
            assert_eq!(base64(text.as_bytes()), encoded, "{text:?}");
        }
        // every sextet: 0xfb 0xff 0xbf is `+/+/`
        assert_eq!(base64(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(base64(&[0, 0, 0]), "AAAA");
    }

    #[test]
    fn the_sequence_for_a_short_text() {
        let path = "/home/u12/public_html/x.php";
        assert_eq!(
            osc52(path).as_deref(),
            Some("\x1b]52;c;L2hvbWUvdTEyL3B1YmxpY19odG1sL3gucGhw\x1b\\")
        );
        let mut out: Vec<u8> = Vec::new();
        write_osc52(&mut out, "hi").unwrap();
        assert_eq!(out, b"\x1b]52;c;aGk=\x1b\\");
        // an empty text is a sequence too: it empties the clipboard
        assert_eq!(osc52("").as_deref(), Some("\x1b]52;c;\x1b\\"));
    }

    #[test]
    fn a_long_text_is_sent_whole_or_not_at_all() {
        let longest = "x".repeat(OSC52_MAX_BYTES);
        let sequence = osc52(&longest).expect("the longest text fits");
        assert!(sequence.len() <= SEQUENCE_MAX, "{}", sequence.len());
        assert!(sequence.len() > SEQUENCE_MAX - BASE64_OUT, "and no shorter one would be the longest");
        assert!(sequence.ends_with(ST));

        let too_long = "x".repeat(OSC52_MAX_BYTES + 1);
        assert_eq!(osc52(&too_long), None);
        let mut out: Vec<u8> = Vec::new();
        let err = write_osc52(&mut out, &too_long).expect_err("refused");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(out.is_empty(), "nothing of it was written");
    }
}
