// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Reading a rendered buffer back as text, for tests: nft-browser had a
`buffer_text` for its `TestBackend`, zfs-browser the same loop six times
inline, and this crate's own tests one per module. They read every cell,
so a wide character came back followed by the blank cell it hides
(`日 本 `); here those cells are skipped, as `TestBackend` does when it
prints a buffer, and a row reads the way it shows.

```
use minitui::{buffer_text, render_buffer};
use ratatui::text::Line;

let buf = render_buffer(Line::from("日本 ok"), 8, 2);
assert_eq!(buffer_text(&buf), "日本 ok \n        ");
```

With a `Terminal<TestBackend>`: `buffer_text(term.backend().buffer())`.
*/

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

/* ===== code =============================================================== */

/// `widget` rendered on an empty `width` × `height` buffer.
pub fn render_buffer(widget: impl Widget, width: u16, height: u16) -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    widget.render(buf.area, &mut buf);
    buf
}

/**
The buffer's rows as text, a wide character's hidden cells skipped (so a
row is as wide as the buffer in columns, not always in chars), trailing
blanks kept.
*/
pub fn buffer_rows(buf: &Buffer) -> Vec<String> {
    let width = usize::from(buf.area.width).max(1);
    buf.content
        .chunks(width)
        .map(|cells| {
            let mut row = String::with_capacity(cells.len());
            let mut hidden = 0;
            for cell in cells {
                let symbol = cell.symbol();
                if hidden == 0 {
                    row.push_str(symbol);
                }
                hidden = hidden.max(symbol.width()).saturating_sub(1);
            }
            row
        })
        .collect()
}

/// The buffer as text: [`buffer_rows`] joined by newlines, for `contains`.
pub fn buffer_text(buf: &Buffer) -> String {
    buffer_rows(buf).join("\n")
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::text::Line;

    #[test]
    fn rows_skip_the_cells_under_a_wide_character() {
        let buf = render_buffer(Line::from("a日b"), 5, 2);
        assert_eq!(buffer_rows(&buf), ["a日b ", "     "]);
    }

    #[test]
    fn an_empty_buffer_has_no_rows() {
        assert!(buffer_rows(&Buffer::empty(Rect::ZERO)).is_empty());
        assert_eq!(buffer_text(&render_buffer(Line::from("x"), 0, 0)), "");
    }
}
