// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Modal prompts: [`InputDialog`] (a text prompt around a [`LineEditor`]) and
[`ConfirmDialog`] (y / n), from the copies in zfs-browser and nft-browser.
Both are widgets over the app's own state: what the typed text or the `y`
means (validation, the action to run) stays with the app.

Sizing keeps both fixes those copies had between them, and adds wrapping:
- min width without `clamp`: nft-browser's `clamp(40, frame width)` panicked
  on a terminal under 40 columns (zfs-browser had fixed it);
- tall enough for every wrapped line, counted by ratatui's own wrapper
  ([`wrap_lines`]); the input prompt wraps too instead of being clipped
  when the terminal is narrower than it.
*/

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};

use crate::popup::{centered, chrome, popup_block, to_u16};
use crate::{LineEditor, wrap_lines};

/* ===== consts and statics ================================================= */

static INPUT_HINT: &str = "⏎ confirm · Esc cancel · ←→ Home/End move";
static INPUT_HINT_CHECKBOX: &str = "⏎ confirm · Tab toggle · Esc cancel · ←→ Home/End move";
static CONFIRM_HINT: &str = "y confirm · n / Esc cancel";
pub(crate) static CHECKED: &str = "[x] ";
pub(crate) static UNCHECKED: &str = "[ ] ";

/// Room to type in, even when the prompt is short.
pub(crate) const INPUT_MIN_W: u16 = 64;
const CONFIRM_MIN_W: u16 = 40;

pub(crate) const INPUT_COLOR: Color = Color::Cyan;
const CONFIRM_COLOR: Color = Color::Red;
const COMMAND_COLOR: Color = Color::Yellow;
pub(crate) const ERROR_COLOR: Color = Color::Red;
pub(crate) const HINT_COLOR: Color = Color::DarkGray;

/* ===== types ============================================================== */

/**
A text prompt: the prompt text (any number of lines), the editor, an
optional checkbox, and a footer that shows the validation error if there
is one, else a key hint. Sized to the prompt (at least 64 columns, room to
type), centered, capped to the area it is rendered into; the prompt wraps
when it has to.

The app keeps the editor and feeds it keys; Enter / Esc / Tab (the
checkbox) come back from [`LineEditor::handle_key`] as `Ignored` for the
app to act on.

```no_run
# use ratatui::Frame;
use minitui::{InputDialog, LineEditor};

# fn draw(frame: &mut Frame, editor: &LineEditor, error: Option<&str>) {
let dialog = InputDialog::new("snapshot", "name for the new snapshot of tank/data:", editor)
    .error(error);
frame.render_widget(dialog, frame.area());
# }
```
*/
#[derive(Debug, Clone)]
#[must_use]
pub struct InputDialog<'a> {
    title: Line<'a>,
    prompt: Text<'a>,
    editor: &'a LineEditor,
    error: Option<&'a str>,
    checkbox: Option<(&'a str, bool)>,
    hint: Option<Line<'a>>,
    style: Style,
    min_width: u16,
}

/**
A yes / no confirmation: the message (any number of lines, wrapped), the
equivalent command in yellow if there is one, and the `y` / `n` hint. Sized
to its longest line (at least 40 columns), centered, capped to the area it
is rendered into, and tall enough that nothing wrapped is clipped. Answer
keys: [`confirm_answer`].

```no_run
# use ratatui::Frame;
use minitui::ConfirmDialog;

# fn draw(frame: &mut Frame) {
let dialog = ConfirmDialog::new("destroy", "Destroy dataset 'tank/data'?")
    .command("zfs destroy tank/data");
frame.render_widget(dialog, frame.area());
# }
```
*/
#[derive(Debug, Clone)]
#[must_use]
pub struct ConfirmDialog<'a> {
    title: Line<'a>,
    message: Text<'a>,
    command: Option<Line<'a>>,
    hint: Line<'a>,
    style: Style,
    min_width: u16,
}

/// A confirmation's answer; see [`confirm_answer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
}

/* ===== InputDialog ======================================================== */

impl<'a> InputDialog<'a> {
    /// `title` (shown padded, ` title `), `prompt` above the editor.
    pub fn new(
        title: impl Into<Line<'a>>,
        prompt: impl Into<Text<'a>>,
        editor: &'a LineEditor,
    ) -> Self {
        Self {
            title: title.into(),
            prompt: prompt.into(),
            editor,
            error: None,
            checkbox: None,
            hint: None,
            style: Style::new().fg(INPUT_COLOR),
            min_width: INPUT_MIN_W,
        }
    }

    /// The last submit's validation error, shown in place of the hint.
    pub fn error(mut self, error: Option<&'a str>) -> Self {
        self.error = error;
        self
    }

    /// A boolean option below the editor, `[x] label`; the app toggles it (Tab).
    pub fn checkbox(mut self, label: &'a str, checked: bool) -> Self {
        self.checkbox = Some((label, checked));
        self
    }

    /// Replace the key hint in the footer.
    pub fn hint(mut self, hint: impl Into<Line<'a>>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The box's base style (borders and prompt); cyan by default.
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// The narrowest the box gets (64 by default), terminal permitting.
    pub fn min_width(mut self, width: u16) -> Self {
        self.min_width = width;
        self
    }

    /// Where the dialog lands when rendered into `outer`.
    pub fn area(&self, outer: Rect) -> Rect {
        let (chrome_w, chrome_h) = chrome(&popup_block(self.title.clone()));
        let content_w = [
            self.prompt.width(),
            self.checkbox
                .map_or(0, |(label, _)| Line::from(label).width() + CHECKED.len()),
            self.hint_line().width(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let width = to_u16(content_w)
            .saturating_add(chrome_w)
            .max(self.min_width)
            .min(outer.width);
        let inner_w = width.saturating_sub(chrome_w);
        let height = wrap_lines(self.body(inner_w), inner_w).saturating_add(chrome_h);
        centered(outer, width, height)
    }

    /// Prompt, editor, checkbox, footer: the dialog's lines for an inner width.
    fn body(&self, inner_w: u16) -> Text<'a> {
        let mut text = self.prompt.clone();
        text.push_line(Line::default());
        text.push_line(self.editor.line(inner_w));
        if let Some((label, checked)) = self.checkbox {
            let (mark, style) = match checked {
                true => (CHECKED, Style::new().fg(COMMAND_COLOR).bold()),
                false => (UNCHECKED, Style::new().fg(HINT_COLOR)),
            };
            text.push_line(Line::default());
            text.push_line(Line::from(vec![Span::raw(mark), Span::raw(label)]).style(style));
        }
        text.push_line(Line::default());
        text.push_line(match self.error {
            Some(error) => Line::from(error).style(Style::new().fg(ERROR_COLOR)),
            None => self.hint_line(),
        });
        text
    }

    fn hint_line(&self) -> Line<'a> {
        self.hint.clone().unwrap_or_else(|| {
            let hint = match self.checkbox {
                Some(_) => INPUT_HINT_CHECKBOX,
                None => INPUT_HINT,
            };
            Line::from(hint).style(Style::new().fg(HINT_COLOR))
        })
    }
}

impl Widget for InputDialog<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup = self.area(area);
        let block = popup_block(self.title.clone()).style(self.style);
        let body = self.body(block.inner(popup).width);
        render_popup(Paragraph::new(body).block(block), popup, buf);
    }
}

/* ===== ConfirmDialog ====================================================== */

impl<'a> ConfirmDialog<'a> {
    /// `title` (shown padded, ` title `) and the question.
    pub fn new(title: impl Into<Line<'a>>, message: impl Into<Text<'a>>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            command: None,
            hint: Line::from(CONFIRM_HINT).style(Style::new().fg(HINT_COLOR)),
            style: Style::new().fg(CONFIRM_COLOR),
            min_width: CONFIRM_MIN_W,
        }
    }

    /// What will run, e.g. the equivalent shell command; yellow, except spans
    /// that have their own style.
    pub fn command(mut self, command: impl Into<Line<'a>>) -> Self {
        let line: Line = command.into();
        self.command = Some(line.style(Style::new().fg(COMMAND_COLOR)));
        self
    }

    /// Replace the `y confirm · n / Esc cancel` hint.
    pub fn hint(mut self, hint: impl Into<Line<'a>>) -> Self {
        self.hint = hint.into();
        self
    }

    /// The box's base style (borders and message); red by default.
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// The narrowest the box gets (40 by default), terminal permitting.
    pub fn min_width(mut self, width: u16) -> Self {
        self.min_width = width;
        self
    }

    /// Where the dialog lands when rendered into `outer`.
    pub fn area(&self, outer: Rect) -> Rect {
        let (chrome_w, chrome_h) = chrome(&popup_block(self.title.clone()));
        let body = self.body();
        // max-then-min, not clamp: clamp(min, w) panics when w < min
        let width = to_u16(body.width())
            .saturating_add(chrome_w)
            .max(self.min_width)
            .min(outer.width);
        let inner_w = width.saturating_sub(chrome_w);
        let height = wrap_lines(body, inner_w).saturating_add(chrome_h);
        centered(outer, width, height)
    }

    /// Message, command, hint, a blank line between each.
    fn body(&self) -> Text<'a> {
        let mut text = self.message.clone();
        if let Some(command) = &self.command {
            text.push_line(Line::default());
            text.push_line(command.clone());
        }
        text.push_line(Line::default());
        text.push_line(self.hint.clone());
        text
    }
}

impl Widget for ConfirmDialog<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup = self.area(area);
        let block = popup_block(self.title.clone()).style(self.style);
        render_popup(Paragraph::new(self.body()).block(block), popup, buf);
    }
}

/**
The answer a key gives a confirmation: `y` yes; `n` or Esc no; anything
else (including a Ctrl or Alt chord, a key release or a held-down repeat)
none, so the dialog stays. Deliberately no Enter-to-confirm: Enter is too
easily already on its way from the prompt before.
*/
pub fn confirm_answer(key: KeyEvent) -> Option<Answer> {
    if key.kind != KeyEventKind::Press
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Char('y' | 'Y') => Some(Answer::Yes),
        KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(Answer::No),
        _ => None,
    }
}

/* ===== helpers ============================================================ */

fn render_popup(body: Paragraph, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);
    body.wrap(Wrap { trim: false }).render(area, buf);
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(widget: impl Widget, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn confirm_wraps_a_long_message_without_clipping() {
        // zfs-browser's case: a by-id device path longer than the box
        let dialog = ConfirmDialog::new(
            "expand",
            "Expand vdev '/dev/disk/by-id/very-long-disk-id-path-here-nothing-to-see-keep-going-part1' \
             into its grown disk? (relabel GPT + online -e)",
        )
        .command("zpool online -e tank <vdev>");
        let text = screen(dialog, 80, 24).replace('\n', "");
        assert!(text.contains("keep-going-part1"), "device path clipped");
        assert!(text.contains("online -e)"), "message tail clipped");
        assert!(text.contains("zpool online -e tank"), "command clipped");
        assert!(text.contains("y confirm"), "hint clipped");
    }

    #[test]
    fn confirm_survives_a_narrow_terminal() {
        // nft-browser's clamp(40, width) panicked under 40 columns
        for msg in ["Destroy tank/ds?".to_string(), "x".repeat(70_000)] {
            let dialog = ConfirmDialog::new("destroy", msg).command("zfs destroy");
            screen(dialog.clone(), 30, 12);
            assert!(dialog.area(Rect::new(0, 0, 30, 12)).width <= 30);
        }
        for (w, h) in [(0, 0), (1, 1), (5, 3)] {
            screen(ConfirmDialog::new("t", "m"), w, h);
        }
    }

    #[test]
    fn confirm_is_sized_to_its_lines() {
        let dialog = ConfirmDialog::new("t", "line one\nline two").command("cmd");
        // message 2 + blank + command + blank + hint = 6 rows, borders 2
        let area = dialog.area(Rect::new(0, 0, 100, 40));
        assert_eq!((area.width, area.height), (CONFIRM_MIN_W, 8));
        let no_command = ConfirmDialog::new("t", "m").area(Rect::new(0, 0, 100, 40));
        assert_eq!(no_command.height, 5);
    }

    #[test]
    fn input_fits_a_long_multiline_prompt() {
        let snap = "tank/some/deeply/nested/dataset@browser-snap_2026-08-18_12:34:56";
        let editor = LineEditor::new("tank/some/deeply/nested/dataset");
        let prompt = format!("replicate\n  {snap}\ninto dataset (or [user@]host:dataset):");
        let text = screen(
            InputDialog::new("send snapshot(s)", prompt, &editor),
            100,
            24,
        );
        assert!(text.contains(snap), "snapshot name truncated:\n{text}");
        assert!(
            text.contains("into dataset"),
            "trailing prompt line missing"
        );
        assert!(
            text.contains("> tank/some/deeply/nested/dataset"),
            "editor missing"
        );
        assert!(text.contains("⏎ confirm"), "footer clipped");
    }

    #[test]
    fn input_wraps_its_prompt_on_a_narrow_terminal() {
        let editor = LineEditor::new("x");
        let prompt = "a prompt much wider than the terminal it is shown on, end";
        let text = screen(InputDialog::new("t", prompt, &editor), 30, 20).replace('\n', "");
        assert!(text.contains("end"), "prompt tail clipped:\n{text}");
        assert!(text.contains("> x"));
    }

    #[test]
    fn the_error_replaces_the_hint() {
        let editor = LineEditor::new("bad name");
        let dialog = InputDialog::new("t", "name:", &editor);
        assert!(screen(dialog.clone(), 80, 12).contains("Esc cancel"));
        let text = screen(dialog.error(Some("name cannot contain spaces")), 80, 12);
        assert!(text.contains("name cannot contain spaces"));
        assert!(!text.contains("Esc cancel"));
    }

    #[test]
    fn a_checkbox_shows_its_state_and_its_key() {
        let editor = LineEditor::default();
        let off = screen(
            InputDialog::new("t", "p", &editor).checkbox("atomic", false),
            80,
            14,
        );
        assert!(off.contains("[ ] atomic") && off.contains("Tab toggle"));
        let on = screen(
            InputDialog::new("t", "p", &editor).checkbox("atomic", true),
            80,
            14,
        );
        assert!(on.contains("[x] atomic"));
    }

    #[test]
    fn the_editor_line_fits_the_box() {
        let editor = LineEditor::new("写".repeat(200));
        let dialog = InputDialog::new("t", "p", &editor);
        let area = dialog.area(Rect::new(0, 0, 80, 20));
        assert_eq!(area.width, INPUT_MIN_W);
        // the editor line wrapping would add a row: prompt, blank, editor, blank, hint
        assert_eq!(area.height, 5 + 2);
        for (w, h) in [(0, 0), (2, 2), (10, 5)] {
            screen(InputDialog::new("t", "p", &editor), w, h);
        }
    }

    #[test]
    fn titles_are_padded() {
        let editor = LineEditor::default();
        assert!(screen(InputDialog::new("rename", "p", &editor), 80, 10).contains("┌ rename ─"));
        assert!(screen(ConfirmDialog::new("destroy", "m"), 80, 10).contains("┌ destroy ─"));
    }

    #[test]
    fn answers_are_y_and_n_or_esc_only() {
        assert_eq!(confirm_answer(key(KeyCode::Char('y'))), Some(Answer::Yes));
        assert_eq!(confirm_answer(key(KeyCode::Char('Y'))), Some(Answer::Yes));
        assert_eq!(confirm_answer(key(KeyCode::Char('n'))), Some(Answer::No));
        assert_eq!(confirm_answer(key(KeyCode::Esc)), Some(Answer::No));
        assert_eq!(
            confirm_answer(key(KeyCode::Enter)),
            None,
            "no Enter-to-confirm"
        );
        let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(confirm_answer(ctrl_y), None);
        let mut repeat = key(KeyCode::Char('y'));
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(
            confirm_answer(repeat),
            None,
            "a held-down y doesn't confirm"
        );
    }
}
