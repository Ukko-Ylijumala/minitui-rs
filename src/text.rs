// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Text measuring and fitting, in terminal display columns: a CJK character or
an emoji takes two cells, a combining mark none. The per-app copies counted
`char`s, which let wide text overflow the space it was measured for.
*/

use std::borrow::Cow;

use ratatui::text::Text;
use ratatui::widgets::{Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/* ===== consts and statics ================================================= */

static ELLIPSIS: &str = "…";
/// Display width of [`ELLIPSIS`].
const ELLIPSIS_W: usize = 1;

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
Walk `chars` (in either direction) while their widths still fit in `room`;
the byte offset of the last char taken, `None` if not even the first fits.
A wide char that would overflow stops the walk, so the result may come out
a column short rather than over.
*/
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
}
