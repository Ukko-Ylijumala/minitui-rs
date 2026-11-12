// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Text shown as it is: [`Visible`] turns text that came from outside (a file
under suspicion, a path, a header) into lines a terminal shows faithfully.
From wp-scanner's file viewer.

ratatui drops control characters silently, so the ESC of an escape sequence
vanishes without a trace and leaves its tail behind as if it were text; a
bidirectional override reorders what the terminal shows after it (Trojan
Source); a zero-width character hides in a name that then matches nothing
the reader types. Here each of them is a glyph in a style of its own, and
a tab runs to the next stop in display columns.
*/

use std::borrow::Cow;
use std::iter::{self, Peekable};
use std::mem;
use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/* ===== consts and statics ================================================= */

/// The columns between tab stops unless set: code's four, not a terminal's eight.
const TAB_COLS: usize = 4;
/// The caret notation's offset: `^[` for ESC (0x1b + 0x40).
const CARET_OFFSET: u32 = 0x40;
/// DEL, shown as `^?`.
const DEL: char = '\u{7f}';

/* ===== types ============================================================== */

/**
How text is made visible: the tab stops, and the style of the glyphs that
stand for what a terminal would not show (red, reversed: unmissable).

```
use minitui::Visible;
use ratatui::style::{Color, Style};

let text = "<?php\n\t$a = '\u{1b}[2J';\n";
// the open tag in green; no styles at all is `[]`
let lines = Visible::new().lines(text, [(0..5, Style::new().fg(Color::Green))]);
assert_eq!(lines[1].to_string(), "    $a = '^[[2J';");
```
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct Visible {
    tab_cols: usize,
    glyph: Style,
}

/// Styled byte ranges in order, walked once from the text's start: the style at an offset.
struct Styles<I: Iterator<Item = (Range<usize>, Style)>> {
    /// From the first range not wholly before the offsets asked so far.
    rest: Peekable<I>,
}

/* ===== code =============================================================== */

impl Default for Visible {
    fn default() -> Self {
        Self {
            tab_cols: TAB_COLS,
            glyph: Style::new().fg(Color::Red).add_modifier(Modifier::REVERSED),
        }
    }
}

impl Visible {
    /// Tab stops every four columns, glyphs in reversed red.
    pub fn new() -> Self {
        Self::default()
    }

    /// The columns between tab stops (at least one).
    pub fn tab_cols(mut self, cols: usize) -> Self {
        self.tab_cols = cols.max(1);
        self
    }

    /// The style of the glyphs, whatever the style of the text around them.
    pub fn glyph_style(mut self, style: impl Into<Style>) -> Self {
        self.glyph = style.into();
        self
    }

    /**
    The lines of `text`, each character in the style of the range of
    `styles` it is in (byte ranges of `text`, in order and apart, as a
    lexer gives its tokens; the text between them is unstyled): tabs
    expanded to the next stop, a line's closing `\r` (a file saved on
    Windows) dropped, and the characters of [`Visible::glyph`] as their
    glyphs. A last newline ends the last line rather than starting another.

    The lines are owned: the text may go once they are made.
    */
    pub fn lines<I>(&self, text: &str, styles: I) -> Vec<Line<'static>>
    where
        I: IntoIterator<Item = (Range<usize>, Style)>,
    {
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut styles = Styles { rest: styles.into_iter().peekable() };
        let mut start = 0;
        let mut out = Vec::new();
        for line in body.split('\n') {
            out.push(self.one_line(line.strip_suffix('\r').unwrap_or(line), start, &mut styles));
            start += line.len() + 1;
        }
        out
    }

    /**
    `text` as one line, a newline in it a glyph (`^J`) like any other
    control character: a path, a name, a header's value. The glyphs are in
    their own style and the rest unstyled; style the line for more.
    */
    pub fn line(&self, text: &str) -> Line<'static> {
        self.one_line(text, 0, &mut Styles { rest: iter::empty().peekable() })
    }

    /**
    `text` as one line in a plain string, for where a style has no place (a
    table's sort key, a title, a message): borrowed when every character
    of it shows as it is.
    */
    pub fn plain<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match text.chars().any(|c| c == '\t' || Self::glyph(c).is_some()) {
            true => Cow::Owned(self.line(text).to_string()),
            false => Cow::Borrowed(text),
        }
    }

    /**
    The glyph for a character a terminal would not show as it is, `None`
    for one it would: `^[` for a C0 control (the tab too, which
    [`Visible::lines`] expands instead), `^?` for DEL, and `<U+XXXX>` for
    a C1 control, a bidirectional control, a line or paragraph separator
    and the invisible characters (soft hyphen, the zero-width space and
    joiners, the word joiner and the invisible operators, the combining
    grapheme joiner, the Mongolian vowel separator, the Hangul fillers,
    the interlinear annotation marks, BOM, the tag characters).
    */
    pub fn glyph(c: char) -> Option<String> {
        let code = u32::from(c);
        match c {
            '\0'..='\u{1f}' => char::from_u32(code + CARET_OFFSET).map(|k| format!("^{k}")),
            DEL => Some("^?".to_string()),
            '\u{80}'..='\u{9f}'
            | '\u{ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}' => Some(format!("<U+{code:04X}>")),
            _ => None,
        }
    }

    /// One line, which starts at byte `start` of the text: its spans, a run of one style each.
    fn one_line<I>(&self, line: &str, start: usize, styles: &mut Styles<I>) -> Line<'static>
    where
        I: Iterator<Item = (Range<usize>, Style)>,
    {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let (mut run, mut run_style) = (String::new(), Style::new());
        let mut col = 0;
        for (i, c) in line.char_indices() {
            // a glyph only for what is not a tab, and only it allocates
            let glyph = (c != '\t').then(|| Self::glyph(c)).flatten();
            let style = match glyph {
                Some(_) => self.glyph,
                None => styles.at(start + i),
            };
            if style != run_style && !run.is_empty() {
                spans.push(Span::styled(mem::take(&mut run), run_style));
            }
            run_style = style;
            match glyph {
                None if c == '\t' => {
                    let to = self.tab_cols - col % self.tab_cols;
                    run.extend(iter::repeat_n(' ', to));
                    col += to;
                }
                // a glyph is ASCII: a column a character
                Some(text) => {
                    col += text.len();
                    run.push_str(&text);
                }
                None => {
                    col += c.width().unwrap_or(0);
                    run.push(c);
                }
            }
        }
        if !run.is_empty() || spans.is_empty() {
            spans.push(Span::styled(run, run_style));
        }
        Line::from(spans)
    }
}

impl<I: Iterator<Item = (Range<usize>, Style)>> Styles<I> {
    /// The style of the character at byte `at`; the offsets asked only ever grow.
    fn at(&mut self, at: usize) -> Style {
        while self.rest.peek().is_some_and(|(range, _)| range.end <= at) {
            self.rest.next();
        }
        match self.rest.peek() {
            Some((range, style)) if range.start <= at => *style,
            _ => Style::new(),
        }
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(text: &str) -> Vec<String> {
        Visible::new().lines(text, []).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn lines_end_at_newlines_and_tabs_at_stops() {
        assert_eq!(shown("a\tb\n\tc\r\nab\t|\n"), ["a   b", "    c", "ab  |"]);
        assert_eq!(shown(""), [""]);
        assert_eq!(shown("\n\n"), ["", ""]);
        assert_eq!(shown("no newline"), ["no newline"]);
        // a \r that does not close the line is a character like any other
        assert_eq!(shown("a\rb"), ["a^Mb"]);
        let wide = Visible::new().tab_cols(8);
        assert_eq!(wide.lines("a\tb", [])[0].to_string(), "a       b");
        assert_eq!(Visible::new().tab_cols(0).lines("a\tb", [])[0].to_string(), "a b", "one column at least");
    }

    #[test]
    fn a_tab_stop_is_a_display_column() {
        // 日 takes two columns: the tab after it runs two more, not three
        assert_eq!(shown("日\tx"), ["日  x"]);
        assert_eq!(shown("日本\tx"), ["日本    x"]);
        // a combining mark takes none
        assert_eq!(shown("e\u{301}\tx"), ["e\u{301}   x"]);
        // a glyph takes its own columns, for the next stop
        assert_eq!(shown("\x01\tx"), ["^A  x"]);
        assert_eq!(shown("\u{200d}\tx"), ["<U+200D>    x"]);
    }

    #[test]
    fn what_a_terminal_would_hide_is_a_glyph() {
        // an escape sequence keeps its ESC
        assert_eq!(shown("x\x1b[2Jy\x7f"), ["x^[[2Jy^?"]);
        // a right-to-left override, and the isolate that would close it
        assert_eq!(
            shown("\u{feff}<?php // \u{202e}evil\u{2066}"),
            ["<U+FEFF><?php // <U+202E>evil<U+2066>"]
        );
        // a zero-width joiner inside a name
        assert_eq!(shown("ad\u{200d}min"), ["ad<U+200D>min"]);
        assert_eq!(shown("\u{85}"), ["<U+0085>"]);
        assert_eq!(
            shown("a\u{ad}b\u{180e}\u{2062}\u{3164}\u{e0041}\u{e007f}"),
            ["a<U+00AD>b<U+180E><U+2062><U+3164><U+E0041><U+E007F>"]
        );
        assert_eq!(shown("a\u{2028}b\u{034f}\u{115f}\u{ffa0}\u{fffa}"), ["a<U+2028>b<U+034F><U+115F><U+FFA0><U+FFFA>"]);
        // what shows as it is stays: accents, CJK, an emoji, a no-break space
        for c in ['é', '日', '😀', '\u{a0}', '\u{301}', ' ', '~'] {
            assert_eq!(Visible::glyph(c), None, "{c:?}");
        }
        assert_eq!(Visible::glyph('\t').as_deref(), Some("^I"));
    }

    #[test]
    fn glyphs_have_their_own_style() {
        let glyph = Style::new().fg(Color::Yellow);
        let line = &Visible::new().glyph_style(glyph).lines("a\x1bb", [])[0];
        assert_eq!(line.spans, [Span::raw("a"), Span::styled("^[", glyph), Span::raw("b")]);
    }

    #[test]
    fn styled_by_byte_ranges() {
        let (tag, var, string) = (
            Style::new().fg(Color::Magenta),
            Style::new().fg(Color::Cyan),
            Style::new().fg(Color::Green),
        );
        //          0     6    11
        let text = "<?php\n$a = '\x1b\t';\n";
        let styles = [(0..5, tag), (6..8, var), (11..15, string)];
        let v = Visible::new();
        let lines = v.lines(text, styles);
        assert_eq!(lines[0].spans, [Span::styled("<?php", tag)]);
        assert_eq!(
            lines[1].spans,
            [
                Span::styled("$a", var),
                Span::raw(" = "),
                Span::styled("'", string),
                // a glyph is a glyph, inside a string or not
                Span::styled("^[", v.glyph),
                // the tab from column 8 to the next stop, a string's still
                Span::styled("    '", string),
                Span::raw(";"),
            ]
        );
        // a range that spans lines styles each of them; one past the end is no harm
        let lines = v.lines("ab\ncd\nef", [(1..7, tag), (40..50, var)]);
        assert_eq!(lines[0].spans, [Span::raw("a"), Span::styled("b", tag)]);
        assert_eq!(lines[1].spans, [Span::styled("cd", tag)]);
        assert_eq!(lines[2].spans, [Span::styled("e", tag), Span::raw("f")]);
    }

    #[test]
    fn one_line_and_a_plain_string() {
        let v = Visible::new();
        assert_eq!(v.line("/home/u/a\nb.php").to_string(), "/home/u/a^Jb.php");
        assert_eq!(v.line("").spans.len(), 1, "an empty line is still a line");
        assert!(matches!(v.plain("/home/u/日本.php"), Cow::Borrowed(_)));
        assert_eq!(v.plain("up\u{202e}php.txt"), "up<U+202E>php.txt");
        assert_eq!(v.plain("a\tb"), "a   b");
    }
}
