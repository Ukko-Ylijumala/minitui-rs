// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A header line: the app's name as a badge, then where the user is. nft-browser
and zfs-browser drew the same one apart from the name (a hand-counted `14`
for the badge and its gap), and both titled their list with the trail's
last segment; [`Breadcrumb`] does both.
*/

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::{ellipsize_start, pill};

/* ===== consts and statics ================================================= */

static SEPARATOR: &str = " / ";
/// Columns between the badge and the trail.
const BADGE_GAP: usize = 1;

/* ===== types ============================================================== */

/**
`[ app ] root / child / leaf` in one row: an optional badge, then the trail
cut from its start with an `…` when narrow, so its most specific end stays
in view. The trail is the app's own string, segments joined by
[`separator`](Breadcrumb::separator) (` / ` by default), which
[`leaf`](Breadcrumb::leaf) splits off the last of.

```no_run
# use ratatui::Frame;
# use ratatui::layout::Rect;
use minitui::Breadcrumb;

# fn draw(frame: &mut Frame, area: Rect, trail: &str) {
frame.render_widget(Breadcrumb::new(trail).badge("zfs-browser"), area);
// the list pane's title
let title = Breadcrumb::new(trail).leaf();
# }
```
*/
#[derive(Clone, Debug)]
pub struct Breadcrumb<'a> {
    trail: &'a str,
    badge: Option<&'a str>,
    separator: &'a str,
    style: Style,
    badge_style: Style,
}

/* ===== Breadcrumb ========================================================= */

impl<'a> Breadcrumb<'a> {
    /// The trail in cyan, no badge.
    pub fn new(trail: &'a str) -> Self {
        Self {
            trail,
            badge: None,
            separator: SEPARATOR,
            style: Style::new().fg(Color::Cyan),
            badge_style: Style::new().fg(Color::Black).bg(Color::Cyan).bold(),
        }
    }

    /// The app's name, ` name ` before the trail; black on cyan, bold.
    pub fn badge(mut self, name: &'a str) -> Self {
        self.badge = Some(name);
        self
    }

    /// What joins the trail's segments, for [`leaf`](Breadcrumb::leaf).
    pub fn separator(mut self, separator: &'a str) -> Self {
        self.separator = separator;
        self
    }

    /// The trail's style.
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// The badge's style.
    pub fn badge_style(mut self, style: impl Into<Style>) -> Self {
        self.badge_style = style.into();
        self
    }

    /// The trail's last segment (all of it without a separator): a title for the view.
    pub fn leaf(&self) -> &'a str {
        self.trail.rsplit(self.separator).next().unwrap_or_default()
    }

    /// The line for `width` columns, the trail cut from its start to fit.
    pub fn line(&self, width: usize) -> Line<'a> {
        let mut spans = Vec::new();
        let mut room = width;
        if let Some(name) = self.badge {
            let badge = pill(name, self.badge_style);
            room = room.saturating_sub(badge.width() + BADGE_GAP);
            spans.extend([badge, Span::raw(" ".repeat(BADGE_GAP))]);
        }
        spans.push(Span::styled(ellipsize_start(self.trail, room), self.style));
        Line::from(spans)
    }
}

impl Widget for Breadcrumb<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let row = Rect { height: 1, ..area };
        self.line(area.width.into()).render(row, buf);
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{buffer_text, render_buffer};

    static TRAIL: &str = "nftables / inet filter / input";

    fn screen(crumb: Breadcrumb, w: u16) -> String {
        buffer_text(&render_buffer(crumb, w, 1))
    }

    #[test]
    fn badge_then_trail() {
        let crumb = Breadcrumb::new(TRAIL).badge("nft-browser");
        assert_eq!(screen(crumb.clone(), 50), format!(" nft-browser  {TRAIL}      "));
        let row = render_buffer(crumb, 50, 1);
        assert_eq!(row[(1, 0)].bg, Color::Cyan, "badge");
        assert_eq!(row[(14, 0)].fg, Color::Cyan, "trail");
    }

    #[test]
    fn narrow_keeps_the_leaf_end() {
        let crumb = Breadcrumb::new(TRAIL).badge("nft-browser");
        assert_eq!(screen(crumb.clone(), 24), " nft-browser  …r / input");
        // narrower than the badge: no trail, the badge cut
        assert_eq!(screen(crumb, 8), " nft-bro");
        assert_eq!(screen(Breadcrumb::new(TRAIL), 10), "…r / input");
    }

    #[test]
    fn the_leaf() {
        assert_eq!(Breadcrumb::new(TRAIL).leaf(), "input");
        assert_eq!(Breadcrumb::new("tank").leaf(), "tank");
        assert_eq!(Breadcrumb::new("tank/data/home").separator("/").leaf(), "home");
        assert_eq!(Breadcrumb::new("").leaf(), "");
    }
}
