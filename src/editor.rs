// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A single-line text editor for prompts: [`LineEditor`]. Lifted from
zfs-browser's modal `Input`, the furthest along of the per-app copies
(nft-browser's scrolled by `char`s, so wide text overflowed the box, and
had no readline keys; mping-rs and edge-xdp-flood use the tui-input crate).
*/

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

/* ===== consts and statics ================================================= */

/// Shown before the text, or [`SCROLLED_PREFIX`] when its start is scrolled off.
static PREFIX: &str = "> ";
static SCROLLED_PREFIX: &str = "‹ ";
/// Display width of either prefix.
const PREFIX_W: usize = 2;
/// Stands in for every char of a secret.
const MASK: char = '•';

/* ===== types ============================================================== */

/**
A single-line text editor: the text, a cursor, and the keys that edit them.
It renders as one line, `> ` and the text, scrolled sideways (in display
columns, so wide characters fit) to keep the cursor in view, the cursor
drawn as a highlighted cell; `‹ ` replaces `> ` when the start is scrolled
off. A secret editor shows `•` for every character.

Keys ([`LineEditor::handle_key`]):
- ← → Home End move; Ctrl-← / Ctrl-→ by word
- Backspace / Delete delete a character
- readline: Ctrl-A / Ctrl-E start / end, Ctrl-U / Ctrl-K delete to the
  start / end, Ctrl-W the word before the cursor
- any other Ctrl or Alt chord is ignored, never typed as its bare letter;
  Ctrl+Alt together is AltGr on some layouts (`@` on Nordic ones): text

Enter, Esc, Tab, Ctrl-C and the like are [`EditOutcome::Ignored`]: what
they mean is the prompt's business, not the editor's.
*/
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEditor {
    text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
    secret: bool,
    style: EditorStyle,
}

/// The styles a [`LineEditor`] renders with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorStyle {
    /// The prefix (`> ` / `‹ `).
    pub prefix: Style,
    pub text: Style,
    /// The cell under the cursor.
    pub cursor: Style,
}

/// What a key did to a [`LineEditor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    /// The text changed (a prompt clears its validation error on this).
    Changed,
    /// An editing key that left the text as it was: a cursor move, or a
    /// delete with nothing to delete.
    Handled,
    /// Not an editing key: Enter, Esc, Tab, Ctrl-C, F-keys, a key release.
    Ignored,
}

/// The visible part of the text for a given width.
struct Window {
    /// Chars (masked if secret) from the first visible one to past the last.
    chars: Vec<char>,
    /// The cursor's index in `chars`; `chars.len()` when past the end.
    cursor: usize,
    /// The start of the text is scrolled off.
    scrolled: bool,
}

/* ===== LineEditor ========================================================= */

impl LineEditor {
    /// An editor holding `text`, the cursor at its end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            cursor: text.len(),
            text,
            ..Self::default()
        }
    }

    /// Show `•` for every character (passphrases, keys).
    pub fn secret(mut self, secret: bool) -> Self {
        self.secret = secret;
        self
    }

    pub fn style(mut self, style: EditorStyle) -> Self {
        self.style = style;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn is_secret(&self) -> bool {
        self.secret
    }

    /// The cursor as a byte offset into [`LineEditor::text`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the text, the cursor at its end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    /// Move the cursor to byte offset `at`, clamped to the text and moved back
    /// onto a char boundary.
    pub fn set_cursor(&mut self, at: usize) {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.cursor = at;
    }

    /**
    Insert `s` at the cursor, e.g. a bracketed paste (`Event::Paste`).
    Control characters (a pasted newline or tab) can't be part of one line
    and are left out. `Changed` if anything was inserted.
    */
    pub fn insert(&mut self, s: &str) -> EditOutcome {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        if clean.is_empty() {
            return EditOutcome::Handled;
        }
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        EditOutcome::Changed
    }

    /// Apply one key; see the type docs for the bindings.
    pub fn handle_key(&mut self, key: KeyEvent) -> EditOutcome {
        if key.kind == KeyEventKind::Release {
            return EditOutcome::Ignored;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // a Ctrl or an Alt chord, but not both (that is AltGr: text)
        if ctrl != alt
            && let KeyCode::Char(c) = key.code
        {
            return match ctrl {
                true => self.readline(c),
                false => EditOutcome::Ignored,
            };
        }
        match key.code {
            KeyCode::Char(c) => self.insert_char(c),
            KeyCode::Left if ctrl => self.move_to(self.word_start()),
            KeyCode::Right if ctrl => self.move_to(self.word_end()),
            KeyCode::Left => self.move_to(prev_boundary(&self.text, self.cursor)),
            KeyCode::Right => self.move_to(next_boundary(&self.text, self.cursor)),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(self.text.len()),
            KeyCode::Backspace => self.delete(prev_boundary(&self.text, self.cursor), self.cursor),
            KeyCode::Delete => self.delete(self.cursor, next_boundary(&self.text, self.cursor)),
            _ => EditOutcome::Ignored,
        }
    }

    /**
    The editor as one line `width` columns wide (prefix included), styled,
    for embedding in a prompt's `Paragraph`. Never wider than `width` from
    3 columns up: the prefix and the cursor cell always show.
    */
    pub fn line(&self, width: u16) -> Line<'static> {
        let win = self.window(width);
        let prefix = if win.scrolled {
            SCROLLED_PREFIX
        } else {
            PREFIX
        };
        let before: String = win.chars[..win.cursor].iter().collect();
        let under = win
            .chars
            .get(win.cursor)
            .map_or(" ".to_string(), char::to_string);
        let after: String = win
            .chars
            .get(win.cursor + 1..)
            .unwrap_or_default()
            .iter()
            .collect();
        Line::from(vec![
            Span::styled(prefix, self.style.prefix),
            Span::styled(before, self.style.text),
            Span::styled(under, self.style.cursor),
            Span::styled(after, self.style.text),
        ])
    }

    /**
    The cursor's column within [`LineEditor::line`] of the same `width`, for
    an app that shows the terminal's own cursor there
    (`frame.set_cursor_position`) instead of the highlighted cell.
    */
    pub fn cursor_column(&self, width: u16) -> u16 {
        let win = self.window(width);
        let cols: usize = win.chars[..win.cursor].iter().map(|&c| char_w(c)).sum();
        u16::try_from(PREFIX_W + cols).unwrap_or(u16::MAX)
    }

    /* ----- editing ----- */

    fn insert_char(&mut self, c: char) -> EditOutcome {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        EditOutcome::Changed
    }

    fn move_to(&mut self, at: usize) -> EditOutcome {
        self.cursor = at;
        EditOutcome::Handled
    }

    /// Delete `from..to` (char boundaries), leaving the cursor at `from`.
    fn delete(&mut self, from: usize, to: usize) -> EditOutcome {
        if from == to {
            return EditOutcome::Handled;
        }
        self.text.replace_range(from..to, "");
        self.cursor = from;
        EditOutcome::Changed
    }

    /// Ctrl-`c`: readline editing; other letters are ignored, not typed.
    fn readline(&mut self, c: char) -> EditOutcome {
        match c {
            'a' => self.move_to(0),
            'e' => self.move_to(self.text.len()),
            'u' => self.delete(0, self.cursor),
            'k' => self.delete(self.cursor, self.text.len()),
            'w' => self.delete(self.word_start(), self.cursor),
            _ => EditOutcome::Ignored,
        }
    }

    /**
    The start of the word before the cursor: back over any whitespace, then
    over the word. Just past a whitespace char, so a char boundary even for
    multi-byte whitespace (U+3000).
    */
    fn word_start(&self) -> usize {
        self.text[..self.cursor]
            .trim_end()
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8())
    }

    /// The end of the word after the cursor: over any whitespace, then the word.
    fn word_end(&self) -> usize {
        let rest = &self.text[self.cursor..];
        let word = rest.len() - rest.trim_start().len();
        rest[word..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(self.text.len(), |(i, _)| self.cursor + word + i)
    }

    /* ----- rendering ----- */

    /**
    The visible chars for a line `width` columns wide. Scrolls by display
    columns, not chars: a wide (CJK) character takes two cells. The window
    starts as far left as still shows the cursor cell (a column of its own
    past the end), then takes whatever fits after it.
    */
    fn window(&self, width: u16) -> Window {
        let room = usize::from(width).saturating_sub(PREFIX_W).max(1);
        let chars: Vec<char> = match self.secret {
            true => self.text.chars().map(|_| MASK).collect(),
            false => self.text.chars().collect(),
        };
        let cursor = self.text[..self.cursor].chars().count();

        let mut start = cursor;
        let mut used = chars.get(cursor).map_or(1, |&c| char_w(c).max(1));
        while start > 0 && used + char_w(chars[start - 1]) <= room {
            start -= 1;
            used += char_w(chars[start]);
        }
        let mut end = cursor.min(chars.len());
        let mut shown: usize = chars[start..end].iter().map(|&c| char_w(c)).sum();
        while end < chars.len() && shown + char_w(chars[end]) <= room {
            shown += char_w(chars[end]);
            end += 1;
        }
        Window {
            chars: chars[start..end].to_vec(),
            cursor: cursor - start,
            scrolled: start > 0,
        }
    }
}

/// Renders [`LineEditor::line`] into the area's first row.
impl Widget for &LineEditor {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.line(area.width).render(area, buf);
    }
}

impl Default for EditorStyle {
    /// zfs-browser's: bold white text, the cursor black on cyan.
    fn default() -> Self {
        Self {
            prefix: Style::new(),
            text: Style::new().fg(Color::White).bold(),
            cursor: Style::new().fg(Color::Black).bg(Color::Cyan),
        }
    }
}

/* ===== helpers ============================================================ */

fn char_w(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// The char boundary before byte offset `at` (0 at the start).
fn prev_boundary(s: &str, at: usize) -> usize {
    s[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

/// The char boundary after byte offset `at` (`at` at the end).
fn next_boundary(s: &str, at: usize) -> usize {
    s[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_str(ed: &mut LineEditor, s: &str) {
        for c in s.chars() {
            assert_eq!(ed.handle_key(press(KeyCode::Char(c))), EditOutcome::Changed);
        }
    }

    fn width(line: &Line) -> usize {
        line.spans.iter().map(|s| s.width()).sum()
    }

    /// The line's text, without styles.
    fn shown(ed: &LineEditor, w: u16) -> String {
        ed.line(w).to_string()
    }

    #[test]
    fn cursor_edits_mid_string() {
        // zfs-browser's rename case: prefilled, cursor at the end
        let mut ed = LineEditor::new("tank/data");
        assert_eq!(ed.handle_key(press(KeyCode::Home)), EditOutcome::Handled);
        type_str(&mut ed, "X");
        ed.handle_key(press(KeyCode::End));
        for _ in 0..4 {
            ed.handle_key(press(KeyCode::Left));
        }
        type_str(&mut ed, "_");
        assert_eq!(ed.handle_key(press(KeyCode::Delete)), EditOutcome::Changed);
        assert_eq!(ed.text(), "Xtank/_ata");
        assert_eq!(
            ed.handle_key(press(KeyCode::Backspace)),
            EditOutcome::Changed
        );
        assert_eq!(ed.text(), "Xtank/ata");
    }

    #[test]
    fn multibyte_chars_edit_as_one() {
        let mut ed = LineEditor::new("aä写");
        ed.handle_key(press(KeyCode::Left));
        assert_eq!(ed.cursor(), "aä".len());
        ed.handle_key(press(KeyCode::Backspace));
        assert_eq!(ed.text(), "a写");
        ed.handle_key(press(KeyCode::Delete));
        assert_eq!(ed.text(), "a");
    }

    #[test]
    fn nothing_to_delete_is_handled_not_changed() {
        let mut ed = LineEditor::new("ab");
        assert_eq!(ed.handle_key(press(KeyCode::Delete)), EditOutcome::Handled);
        ed.handle_key(press(KeyCode::Home));
        assert_eq!(
            ed.handle_key(press(KeyCode::Backspace)),
            EditOutcome::Handled
        );
        assert_eq!(ed.text(), "ab");
    }

    #[test]
    fn ctrl_chords_edit_not_insert() {
        let mut ed = LineEditor::new("prefilled");
        assert_eq!(ed.handle_key(ctrl('u')), EditOutcome::Changed);
        assert_eq!(
            ed.text(),
            "",
            "Ctrl-U kills the prefilled text, types no 'u'"
        );
        type_str(&mut ed, "one two\u{3000}three");
        ed.handle_key(ctrl('w'));
        assert_eq!(
            ed.text(),
            "one two\u{3000}",
            "Ctrl-W stops at multi-byte whitespace"
        );
        ed.handle_key(ctrl('a'));
        assert_eq!(ed.cursor(), 0);
        ed.handle_key(ctrl('k'));
        assert_eq!(ed.text(), "");
        // AltGr arrives as Ctrl+Alt on some terminals: still text
        let altgr = KeyEvent::new(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(ed.handle_key(altgr), EditOutcome::Changed);
        assert_eq!(ed.text(), "@");
        // other chords are nobody's text
        assert_eq!(ed.handle_key(ctrl('x')), EditOutcome::Ignored);
        let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(ed.handle_key(alt_b), EditOutcome::Ignored);
        assert_eq!(ed.text(), "@");
    }

    #[test]
    fn the_prompts_keys_are_left_to_the_prompt() {
        let mut ed = LineEditor::new("x");
        for key in [
            press(KeyCode::Enter),
            press(KeyCode::Esc),
            press(KeyCode::Tab),
            ctrl('c'),
        ] {
            assert_eq!(ed.handle_key(key), EditOutcome::Ignored);
        }
        let mut release = press(KeyCode::Char('y'));
        release.kind = KeyEventKind::Release;
        assert_eq!(ed.handle_key(release), EditOutcome::Ignored);
        assert_eq!(ed.text(), "x");
    }

    #[test]
    fn ctrl_arrows_move_by_word() {
        let ctrl_key = |code| KeyEvent::new(code, KeyModifiers::CONTROL);
        let mut ed = LineEditor::new("one  two three");
        ed.handle_key(ctrl_key(KeyCode::Left));
        assert_eq!(ed.cursor(), "one  two ".len());
        ed.handle_key(ctrl_key(KeyCode::Left));
        assert_eq!(ed.cursor(), "one  ".len());
        ed.handle_key(ctrl_key(KeyCode::Right));
        assert_eq!(ed.cursor(), "one  two".len());
        ed.handle_key(ctrl_key(KeyCode::Right));
        assert_eq!(ed.cursor(), "one  two three".len());
    }

    #[test]
    fn a_paste_drops_control_chars() {
        let mut ed = LineEditor::new("ab");
        ed.set_cursor(1);
        assert_eq!(ed.insert("x\ny\t"), EditOutcome::Changed);
        assert_eq!(ed.text(), "axyb");
        assert_eq!(ed.cursor(), 3);
        assert_eq!(ed.insert("\n"), EditOutcome::Handled);
    }

    #[test]
    fn set_cursor_lands_on_a_char_boundary() {
        let mut ed = LineEditor::new("aä");
        ed.set_cursor(2); // inside 'ä'
        assert_eq!(ed.cursor(), 1);
        ed.set_cursor(99);
        assert_eq!(ed.cursor(), "aä".len());
    }

    #[test]
    fn the_line_shows_the_cursor_cell() {
        let mut ed = LineEditor::new("abc");
        assert_eq!(
            shown(&ed, 20),
            "> abc ",
            "past the end: a blank cursor cell"
        );
        ed.set_cursor(1);
        let line = ed.line(20);
        assert_eq!(line.spans[2].content, "b");
        assert_eq!(line.spans[2].style, EditorStyle::default().cursor);
        assert_eq!(ed.cursor_column(20), 3);
    }

    #[test]
    fn a_long_text_scrolls_to_the_cursor() {
        let mut ed = LineEditor::new("0123456789abcdef");
        // 10 columns: prefix 2 + 8; the cursor cell past the end takes one
        assert_eq!(shown(&ed, 10), "‹ 9abcdef ");
        ed.set_cursor(0);
        assert_eq!(shown(&ed, 10), "> 01234567");
    }

    #[test]
    fn wide_characters_fit_the_width() {
        // zfs-browser's case: wide chars must never overflow the box
        let name = "写真".repeat(30) + ".jpg";
        let mut ed = LineEditor::new(name.clone());
        for cursor in [0, name.len(), "写真".repeat(10).len()] {
            ed.set_cursor(cursor);
            let line = ed.line(28);
            assert!(width(&line) <= 28, "cursor {cursor}: {} cols", width(&line));
            assert!(usize::from(ed.cursor_column(28)) < 28);
        }
        ed.set_cursor(0);
        assert!(!shown(&ed, 28).contains(".jpg"));
    }

    #[test]
    fn a_secret_is_masked() {
        let ed = LineEditor::new("hunter2").secret(true);
        assert_eq!(shown(&ed, 20), "> ••••••• ");
        assert_eq!(ed.text(), "hunter2");
    }

    #[test]
    fn a_tiny_width_still_shows_the_cursor() {
        let ed = LineEditor::new("abc");
        for w in 0..4 {
            let _ = ed.line(w);
        }
        assert_eq!(shown(&ed, 3), "‹  ");
    }
}
