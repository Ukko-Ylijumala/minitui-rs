// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Popups drawn over the UI, and what they share: sizing from a block's actual
chrome, centering, capping to the screen, clearing the cells beneath, the
default box. [`HelpPopup`] replaces the per-app help overlays:
the pre-formatted help text of zfs-browser, nft-browser and squawk, and the
key / description tables of collector-status and edge-xdp-flood. All of
them sized the box to the text, centered it, capped it to the terminal and
cleared the cells beneath; here that is done once, measuring display
columns, and sized from the block's actual chrome (borders, padding,
titles) instead of a hand-counted `+ 4`.
*/

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Padding, Paragraph, StatefulWidget, Widget};
use unicode_width::UnicodeWidthStr;

use crate::scroll::indicator_width;
use crate::{Scroll, TextView};

/* ===== consts and statics ================================================= */

static DEFAULT_TITLE: &str = "keys";
/// Columns between the key column and the descriptions of a row table.
const KEY_GAP: usize = 2;
/// An area big enough to measure any block's chrome against.
const PROBE: Rect = Rect::new(0, 0, 1000, 1000);

/* ===== types ============================================================== */

/**
A help overlay, centered over the area it is rendered into (normally the
whole frame) and sized to its content: the widest line plus the block's
chrome, all lines plus the block's chrome, capped to the area. The cells
beneath are cleared. Lines are not wrapped (help text is laid out in
columns); a text taller than the area scrolls: render it through a
[`Scroll`] (as a `StatefulWidget`) that the keys move, or give the offset
with [`HelpPopup::scroll`].

```no_run
# use ratatui::Frame;
# use ratatui::style::{Color, Style};
use minitui::HelpPopup;

static HELP_TEXT: &str = "  q    quit\n  h    this help";
static HELP_ROWS: &[(&str, &str)] = &[("q", "quit"), ("h / F1", "this help")];

# fn draw(frame: &mut Frame) {
// pre-formatted text, in the default "keys" box
frame.render_widget(HelpPopup::new(HELP_TEXT).style(Color::Cyan), frame.area());
// or a key / description table with styled keys
let help = HelpPopup::from_rows(HELP_ROWS, Style::new().bold()).title("Keys");
frame.render_widget(help, frame.area());
# }
```
*/
#[derive(Debug, Clone)]
#[must_use]
pub struct HelpPopup<'a> {
    text: Text<'a>,
    title: Line<'a>,
    style: Style,
    block: Option<Block<'a>>,
    scroll: u16,
}

/* ===== HelpPopup ========================================================== */

impl<'a> HelpPopup<'a> {
    /// Pre-formatted help text, shown as is (`\n` separates the lines).
    pub fn new(text: impl Into<Text<'a>>) -> Self {
        Self {
            text: text.into(),
            title: Line::from(DEFAULT_TITLE),
            style: Style::new(),
            block: None,
            scroll: 0,
        }
    }

    /**
    A table of `(key, description)` rows: the keys in `key_style`, padded to
    the widest key so the descriptions line up. A row with an empty key
    continues the description above it.
    */
    pub fn from_rows(rows: &[(&'a str, &'a str)], key_style: Style) -> Self {
        let key_w = rows.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
        let lines: Vec<Line> = rows
            .iter()
            .map(|&(key, desc)| {
                let pad = " ".repeat(key_w - key.width() + KEY_GAP);
                Line::from(vec![
                    Span::styled(key, key_style),
                    Span::raw(pad),
                    Span::raw(desc),
                ])
            })
            .collect();
        Self::new(lines)
    }

    /// Lines below the help, after a blank one: notes, a legend, "press any key".
    pub fn footer(mut self, footer: impl Into<Text<'a>>) -> Self {
        self.text.push_line(Line::default());
        self.text.extend(footer.into());
        self
    }

    /// The title of the default box (`keys`, shown padded with a space on either
    /// side like every popup title here); ignored with [`HelpPopup::block`].
    pub fn title(mut self, title: impl Into<Line<'a>>) -> Self {
        self.title = title.into();
        self
    }

    /// The popup's base style (borders and text), e.g. a color.
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /**
    Replace the default box (bordered, titled, one column of padding on
    either side) with `block`. The size is still right: it is measured
    from the block's own borders, padding and titles.
    */
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Start the text `rows` down; capped to [`HelpPopup::overflow`]. Not
    /// used when rendered through a [`Scroll`].
    pub fn scroll(mut self, rows: u16) -> Self {
        self.scroll = rows;
        self
    }

    /// Where the popup lands when rendered into `outer`.
    pub fn area(&self, outer: Rect) -> Rect {
        let (w, h) = self.size();
        centered(outer, w, h)
    }

    /**
    How many rows of the text don't fit when rendered into `outer`: the
    furthest it can scroll. 0 when it all fits.
    */
    pub fn overflow(&self, outer: Rect) -> u16 {
        self.size().1.saturating_sub(self.area(outer).height)
    }

    /// The size the popup wants: content plus chrome, uncapped.
    fn size(&self) -> (u16, u16) {
        let (chrome_w, chrome_h) = chrome(&self.make_block());
        (
            to_u16(self.text.width()).saturating_add(chrome_w),
            to_u16(self.text.height()).saturating_add(chrome_h),
        )
    }

    fn make_block(&self) -> Block<'a> {
        let block = self
            .block
            .clone()
            .unwrap_or_else(|| popup_block(self.title.clone()));
        block.style(self.style)
    }
}

impl Widget for HelpPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup = self.area(area);
        let scroll = self.scroll.min(self.overflow(area));
        let block = self.make_block();
        Clear.render(popup, buf);
        Paragraph::new(self.text)
            .block(block)
            .scroll((scroll, 0))
            .render(popup, buf);
    }
}

/**
Rendered through a [`Scroll`]: [`Scroll::key`] scrolls it, clamped so that
↑ moves at once after an overshoot, and when the text doesn't fit the
bottom border says which lines are in view (the popup widens for it, if
the text is narrower).
*/
impl StatefulWidget for HelpPopup<'_> {
    type State = Scroll;

    fn render(self, area: Rect, buf: &mut Buffer, scroll: &mut Scroll) {
        let mut popup = self.area(area);
        if self.overflow(area) > 0 {
            let w = to_u16(indicator_width(self.text.height())).max(popup.width);
            popup = centered(area, w, popup.height);
        }
        let block = self.make_block();
        Clear.render(popup, buf);
        TextView::new(self.text).block(block).render(popup, buf, scroll);
    }
}

/* ===== shared by the popups =============================================== */

/// The default popup box: bordered, `title` padded with a space on either side,
/// one column of padding inside.
pub(crate) fn popup_block<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::bordered()
        .title(padded(title.into()))
        .padding(Padding::horizontal(1))
}

/// Columns and rows `block` takes around its content: borders, padding, titles.
pub(crate) fn chrome(block: &Block) -> (u16, u16) {
    let inner = block.inner(PROBE);
    (PROBE.width - inner.width, PROBE.height - inner.height)
}

/// A `w` × `h` area centered in `outer`, capped to it.
pub(crate) fn centered(outer: Rect, w: u16, h: u16) -> Rect {
    outer.centered(
        Constraint::Length(w.min(outer.width)),
        Constraint::Length(h.min(outer.height)),
    )
}

/// A measured size (`usize`) as a `u16` dimension, saturating.
pub(crate) fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// ` title `: a title off the border corner and line; an empty one stays empty.
fn padded(mut title: Line<'_>) -> Line<'_> {
    if title.width() > 0 {
        title.spans.insert(0, Span::raw(" "));
        title.spans.push(Span::raw(" "));
    }
    title
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Cell;
    use ratatui::style::{Color, Stylize};

    static TEXT: &str = "  q    quit\n  h    this help";

    fn render(popup: HelpPopup, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::filled(area, Cell::new("x"));
        Widget::render(popup, area, &mut buf);
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    #[test]
    fn sized_to_the_text_and_centered() {
        // widest line 16 + borders 2 + padding 2; 2 lines + borders 2
        let popup = HelpPopup::new(TEXT);
        assert_eq!(popup.area(Rect::new(0, 0, 40, 10)), Rect::new(10, 3, 20, 4));
        let rows = render(popup, 40, 10);
        let side = "x".repeat(10);
        assert_eq!(rows[3], format!("{side}┌ keys ────────────┐{side}"));
        assert_eq!(rows[4], format!("{side}│   q    quit      │{side}"));
        assert_eq!(rows[5], format!("{side}│   h    this help │{side}"));
        assert_eq!(
            rows[2],
            "x".repeat(40),
            "nothing outside the popup is touched"
        );
    }

    #[test]
    fn the_cells_beneath_are_cleared() {
        // "a" is shorter than the popup: the rest of its row must be blank, not "x"
        let popup = HelpPopup::new("a\nbbbbbbbbbb");
        let outer = Rect::new(0, 0, 20, 6);
        let area = popup.area(outer);
        let rows = render(popup, 20, 6);
        let a_row: String = rows[usize::from(area.y) + 1]
            .chars()
            .skip(usize::from(area.x))
            .take(usize::from(area.width))
            .collect();
        assert_eq!(a_row, "│ a          │");
    }

    #[test]
    fn rows_line_up_on_the_widest_key() {
        let popup = HelpPopup::from_rows(
            &[("q", "quit"), ("F1 / h", "help"), ("", "  more")],
            Style::new(),
        );
        let lines: Vec<String> = popup.text.lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines, ["q       quit", "F1 / h  help", "          more"]);
    }

    #[test]
    fn keys_get_their_style() {
        let popup = HelpPopup::from_rows(&[("q", "quit")], Style::new().fg(Color::Green));
        assert_eq!(popup.text.lines[0].spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn a_footer_follows_a_blank_line() {
        let popup = HelpPopup::new("a").footer("press any key".dark_gray());
        let lines: Vec<String> = popup.text.lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(lines, ["a", "", "press any key"]);
    }

    #[test]
    fn a_custom_block_is_measured_not_assumed() {
        let bare = HelpPopup::new("abc").block(Block::new());
        assert_eq!(bare.area(Rect::new(0, 0, 10, 10)).width, 3);
        // 3 + borders 2 + padding 4 wide, 1 + 2 + 4 high
        let padded = HelpPopup::new("abc").block(Block::bordered().padding(Padding::uniform(2)));
        let area = padded.area(Rect::new(0, 0, 20, 20));
        assert_eq!((area.width, area.height), (9, 7));
    }

    #[test]
    fn capped_to_the_area_and_scrollable() {
        let text = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let popup = HelpPopup::new(text.clone());
        let outer = Rect::new(0, 0, 30, 10);
        assert_eq!(popup.area(outer).height, 10);
        assert_eq!(popup.overflow(outer), 12, "22 rows wanted, 10 shown");

        let rows = render(HelpPopup::new(text.clone()).scroll(5), 30, 10);
        assert!(rows[1].contains("line 6"));
        // scrolling past the end stops at the last full page
        let rows = render(HelpPopup::new(text).scroll(99), 30, 10);
        assert!(rows[8].contains("line 20"));
    }

    #[test]
    fn scrolled_through_a_scroll() {
        let text = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let area = Rect::new(0, 0, 30, 10);
        let mut buf = Buffer::empty(area);
        let mut scroll = Scroll::new();
        scroll.set_offset(99);
        StatefulWidget::render(HelpPopup::new(text), area, &mut buf, &mut scroll);
        assert_eq!(scroll.offset(), 12, "the last full page");
        let row = |y| (0..30).map(|x| buf[(x, y)].symbol()).collect::<String>();
        assert!(row(8).contains("line 20"));
        assert!(row(9).contains(" lines 13–20 of 20 "), "{}", row(9));
    }

    #[test]
    fn a_tiny_area_does_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (3, 2)] {
            render(HelpPopup::new(TEXT), w, h);
        }
    }
}
