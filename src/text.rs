// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Text measuring and fitting, in terminal display columns: a CJK character or
an emoji takes two cells, a combining mark none. The per-app copies counted
`char`s, which let wide text overflow the space it was measured for.
*/

use std::borrow::Cow;
use std::ops::Range;

use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/* ===== consts and statics ================================================= */

static ELLIPSIS: &str = "…";
/// Display width of [`ELLIPSIS`].
const ELLIPSIS_W: usize = 1;

/* ===== types ============================================================== */

/// Which end of a text gives way when it doesn't fit: a name's end, a path's start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Cut {
    /// Keep the start, a trailing `…` ([`ellipsize_end`]).
    #[default]
    End,
    /// Keep the end, a leading `…` ([`ellipsize_start`]).
    Start,
}

/* ===== code =============================================================== */

/**
How many rows `text` takes when word-wrapped to `width` columns, as a
`Paragraph` with `Wrap { trim: false }` renders it: for sizing a popup so
that nothing in it is clipped. Each `\n`-separated line wraps on its own.

This is ratatui's own count (its word wrapper, graphemes and display
widths), so it can't drift from the rendering. It is behind ratatui's
`unstable-rendered-line-info` feature, so a ratatui upgrade may need a look
here.
*/
pub fn wrap_lines<'a>(text: impl Into<Text<'a>>, width: u16) -> u16 {
    let count = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(width.max(1));
    u16::try_from(count).unwrap_or(u16::MAX)
}

/**
`{what} a–b of N` for the `shown` part of `len` items, counted from 1: a
list's or a view's position for its title (`rows 21–40 of 300`).
*/
pub fn range_label(what: &str, shown: Range<usize>, len: usize) -> String {
    format!("{what} {}–{} of {len}", shown.start + 1, shown.end)
}

/**
Fit `s` into `width` columns by cutting from the start, with a leading `…`,
so the most specific (rightmost) part stays visible: a breadcrumb trail, a
path. Unchanged (borrowed) when it already fits; never wider than `width`.
*/
pub fn ellipsize_start(s: &str, width: usize) -> Cow<'_, str> {
    if s.width() <= width {
        return Cow::Borrowed(s);
    }
    let Some(room) = width.checked_sub(ELLIPSIS_W) else {
        return Cow::Borrowed("");
    };
    let start = fit_from(s.char_indices().rev(), room).unwrap_or(s.len());
    Cow::Owned(format!("{ELLIPSIS}{}", &s[start..]))
}

/**
Fit `s` into `width` columns by cutting from the end, with a trailing `…`:
a name in a table cell. Unchanged (borrowed) when it already fits; never
wider than `width`.
*/
pub fn ellipsize_end(s: &str, width: usize) -> Cow<'_, str> {
    if s.width() <= width {
        return Cow::Borrowed(s);
    }
    let Some(room) = width.checked_sub(ELLIPSIS_W) else {
        return Cow::Borrowed("");
    };
    // the byte offset just past the last char that still fits
    let end = fit_from(s.char_indices(), room)
        .map(|i| i + s[i..].chars().next().map_or(0, char::len_utf8))
        .unwrap_or(0);
    Cow::Owned(format!("{}{ELLIPSIS}", &s[..end]))
}

/**
Fit a styled `line` into `width` columns as [`ellipsize_end`] /
[`ellipsize_start`] fit a string: unchanged when it fits, otherwise cut
at `cut`'s end with an `…` in the style of the span it cut into. The
line's own style and alignment are kept.
*/
pub fn fit_line(line: Line<'_>, width: usize, cut: Cut) -> Line<'_> {
    if line.width() <= width {
        return line;
    }
    let Line { style, alignment, spans } = line;
    let mut kept: Vec<Span> = Vec::with_capacity(spans.len() + 1);
    if let Some(room) = width.checked_sub(ELLIPSIS_W) {
        let mut used = 0;
        let ordered: Box<dyn Iterator<Item = Span>> = match cut {
            Cut::End => Box::new(spans.into_iter()),
            Cut::Start => Box::new(spans.into_iter().rev()),
        };
        for span in ordered {
            let w = span.width();
            if used + w <= room {
                used += w;
                kept.push(span);
                continue;
            }
            let content: &str = &span.content;
            let part: &str = match cut {
                Cut::End => {
                    let end = fit_from(content.char_indices(), room - used)
                        .map(|i| i + content[i..].chars().next().map_or(0, char::len_utf8))
                        .unwrap_or(0);
                    &content[..end]
                }
                Cut::Start => {
                    let start = fit_from(content.char_indices().rev(), room - used).unwrap_or(content.len());
                    &content[start..]
                }
            };
            if !part.is_empty() {
                kept.push(Span::styled(part.to_string(), span.style));
            }
            kept.push(Span::styled(ELLIPSIS, span.style));
            break;
        }
        if cut == Cut::Start {
            kept.reverse();
        }
    }
    let mut out = Line::from(kept).style(style);
    out.alignment = alignment;
    out
}

/**
Walk `chars` (in either direction) while their widths still fit in `room`;
the byte offset of the last char taken, `None` if not even the first fits.
A wide char that would overflow stops the walk, so the result may come out
a column short rather than over.
*/
/**
`line` with its first `cols` display columns left out, for a view
scrolled sideways; the spans keep their styles, and a borrowed span stays
borrowed. A wide character cut in two shows its right half as spaces, so
what follows stays in its column.
*/
pub(crate) fn skip_cols(line: Line<'_>, cols: usize) -> Line<'_> {
    if cols == 0 {
        return line;
    }
    let Line { style, alignment, spans } = line;
    let mut left = cols;
    let mut kept: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        if left == 0 {
            kept.push(span);
            continue;
        }
        let w = span.width();
        if w <= left {
            left -= w;
            continue;
        }
        // the cut is inside this span: where its rest starts, and the cut character's spaces
        let (mut at, mut pad) = (span.content.len(), 0);
        for (i, g) in span.content.grapheme_indices(true) {
            if left == 0 {
                at = i;
                break;
            }
            let gw = g.width();
            if gw > left {
                (at, pad) = (i + g.len(), gw - left);
                break;
            }
            left -= gw;
        }
        left = 0;
        if pad > 0 {
            kept.push(Span::styled(" ".repeat(pad), span.style));
        }
        let rest: Cow<'_, str> = match span.content {
            Cow::Borrowed(s) => Cow::Borrowed(&s[at..]),
            Cow::Owned(s) => Cow::Owned(s[at..].to_string()),
        };
        kept.push(Span::styled(rest, span.style));
    }
    let mut out = Line::from(kept).style(style);
    out.alignment = alignment;
    out
}

fn fit_from(chars: impl Iterator<Item = (usize, char)>, room: usize) -> Option<usize> {
    let mut used = 0;
    let mut last = None;
    for (i, c) in chars {
        used += c.width().unwrap_or(0);
        if used > room {
            break;
        }
        last = Some(i);
    }
    last
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::style::{Color, Style};

    #[test]
    fn styled_lines_fit_at_either_end() {
        let red = Style::new().fg(Color::Red);
        let line = || Line::from(vec![Span::raw("/home/"), Span::styled("u12", red), Span::raw("/public_html")]);
        assert_eq!(fit_line(line(), 21, Cut::End), line(), "it fits");
        let end = fit_line(line(), 10, Cut::End);
        assert_eq!(end, Line::from(vec![Span::raw("/home/"), Span::styled("u12", red), Span::raw("…")]));
        let start = fit_line(line(), 10, Cut::Start);
        assert_eq!(start, Line::from(vec![Span::raw("…"), Span::raw("blic_html")]));
        // the ellipsis takes the style of the span it cut into
        let start = fit_line(line(), 15, Cut::Start);
        assert_eq!(start.spans[..2], [Span::styled("…", red), Span::styled("12", red)]);
        assert_eq!(start.width(), 15);
        // wide characters never overflow; nothing fits in no width
        assert_eq!(fit_line(Line::from("日本語のパス"), 6, Cut::End).width(), 5);
        assert_eq!(fit_line(line(), 0, Cut::End).width(), 0);
        // the line's alignment survives
        assert_eq!(fit_line(line().right_aligned(), 8, Cut::End).alignment, Some(ratatui::layout::Alignment::Right));
    }

    #[test]
    fn wrap_lines_counts_rows_like_the_renderer() {
        // the cases of zfs-browser's own wrap_lines, which this replaces
        assert_eq!(wrap_lines("short", 40), 1);
        assert_eq!(wrap_lines("one two three four", 10), 2); // "one two" | "three four"
        assert_eq!(wrap_lines("one two three four", 9), 3); // "one two" | "three" | "four"
        assert_eq!(wrap_lines("aaaaaaaaaa", 4), 3); // a long word splits
        // what the char-counting copy got wrong: 你好 is 4 columns, not 2
        assert_eq!(wrap_lines("你好 你好", 4), 2);
        assert_eq!(wrap_lines("one\ntwo", 40), 2);
        assert_eq!(wrap_lines("x", 0), 1, "a zero width counts as one column");
    }

    #[test]
    fn ellipsize_start_keeps_the_tail() {
        assert_eq!(ellipsize_start("pool/data/home", 20), "pool/data/home");
        assert!(matches!(ellipsize_start("fits", 4), Cow::Borrowed(_)));
        assert_eq!(ellipsize_start("pool/data/home", 8), "…ta/home");
        assert_eq!(ellipsize_start("pool", 1), "…");
        assert_eq!(ellipsize_start("pool", 0), "");
    }

    #[test]
    fn ellipsize_end_keeps_the_head() {
        assert_eq!(ellipsize_end("a rather long name", 8), "a rathe…");
        assert!(matches!(ellipsize_end("fits", 4), Cow::Borrowed(_)));
        assert_eq!(ellipsize_end("name", 1), "…");
        assert_eq!(ellipsize_end("name", 0), "");
    }

    #[test]
    fn ellipsizing_counts_display_columns() {
        // 4 wide chars = 8 columns; 5 columns hold … + two of them
        assert_eq!(ellipsize_start("日本語版", 5), "…語版");
        assert_eq!(ellipsize_end("日本語版", 5), "日本…");
        // 4 columns: … + one wide char (3), not over
        assert_eq!(ellipsize_start("日本語版", 4), "…版");
        assert_eq!(ellipsize_end("日本語版", 4), "日…");
        for w in 0..10 {
            assert!(ellipsize_start("日本語版", w).width() <= w);
            assert!(ellipsize_end("日本語版", w).width() <= w);
        }
    }

    #[test]
    fn skip_cols_keeps_the_styles_and_the_columns() {
        let red = Style::new().fg(Color::Red);
        let line = Line::from(vec![Span::raw("ab"), Span::styled("cdef", red), Span::raw("gh")]);
        let skipped = skip_cols(line.clone(), 3);
        assert_eq!(skipped.spans, [Span::styled("def", red), Span::raw("gh")]);
        assert!(matches!(skipped.spans[0].content, Cow::Borrowed(_)), "borrowed stays borrowed");
        assert_eq!(skip_cols(line.clone(), 0), line);
        assert_eq!(skip_cols(line.clone(), 8).width(), 0, "all of it");
        assert_eq!(skip_cols(line, 99).width(), 0, "past the end");
    }

    #[test]
    fn skip_cols_cuts_a_wide_character_to_spaces() {
        // 漢 and 字 take two columns each: skipping 1 leaves 漢's right half as a space
        let skipped = skip_cols(Line::from("漢字x"), 1);
        assert_eq!(skipped.to_string(), " 字x");
        assert_eq!(skipped.width(), 4);
        assert_eq!(skip_cols(Line::from("漢字x"), 2).to_string(), "字x");
        // a combining mark goes with its letter
        assert_eq!(skip_cols(Line::from("e\u{301}fg"), 1).to_string(), "fg");
    }
}
