// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A modal form: [`Form`], a list of labeled fields (text, checkbox, choice)
with focus, optional Submit / Cancel buttons, a note and the validation
error. Generalized from wiremeup's field-list `Form`, which already served
all its dialogs, and the hand-built ones it was modelled on: mping-rs's
add-target dialog (two text fields, a checkbox, buttons, a feedback area)
and edge-xdp-flood's F2 config dialog (14 text fields, 2 checkboxes).

Fixes over the copies, which all edited through tui-input:
- the text fields are [`LineEditor`]s: they scroll to keep the cursor in
  view (flood's and wiremeup's were clipped, the cursor left behind at the
  edge) and place the cursor by display width (wiremeup's took a column
  count for a char index, off after any wide character);
- labels line up by display width, not bytes;
- Enter submits from every field, a checkbox too, as flood learned the
  hard way: its F2 opened on a checkbox, so "open, glance, Enter" flipped
  an auto-action instead of closing.
*/

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};

use crate::dialog::{CHECKED, ERROR_COLOR, HINT_COLOR, INPUT_COLOR, INPUT_MIN_W, UNCHECKED};
use crate::popup::{centered, chrome, popup_block, to_u16};
use crate::{EditOutcome, LineEditor, wrap_lines};

/* ===== consts and statics ================================================= */

static HINT_BASE: &str = "⏎ submit · Esc cancel · Tab/↑↓ field";
static HINT_CHECKBOX: &str = " · Space toggle";
static HINT_CHOICE: &str = " · ←→ choose";
static MARK_FOCUS: &str = "▶ ";
static MARK_IDLE: &str = "  ";
static CHOICE_OPEN: &str = "‹ ";
static CHOICE_CLOSE: &str = " ›";

/// Columns of the focus mark before a label.
const MARK_W: usize = 2;
/// Columns between the label column and the values.
const LABEL_GAP: usize = 2;
/// Columns between the two buttons.
const BUTTON_GAP: usize = 2;
/// A text field's room beyond its text: the `> ` prefix and the cursor cell.
const TEXT_EXTRA_W: usize = 3;

const FOCUS_COLOR: Color = Color::Cyan;
const IDLE_CHOICE_COLOR: Color = Color::Gray;
const SUBMIT_COLOR: Color = Color::Green;
const CANCEL_COLOR: Color = Color::Red;

/* ===== types ============================================================== */

/**
A modal form: a title, labeled fields, optional Submit / Cancel buttons, a
note (the app's own text: "working…", the result of the last submit) and
the validation error, shown in place of the key hint. Unlike the other
dialogs, the form is state: it owns its fields, the app keeps it and feeds
it keys ([`Form::handle_key`]), and renders `&form`.

Fields are addressed by index, in the order given; an app usually names
them with an enum (`Field::Name as usize`). The accessors panic on a field
of another kind: the form's shape is the app's own, fixed in code.

Keys:
- Tab / ↓ and Shift-Tab / ↑ move the focus (the buttons included), wrapping
- Enter submits, from any field; on the Cancel button it cancels
- Esc cancels
- Space toggles a checkbox; Space / → and ← cycle a choice
- a text field takes the [`LineEditor`] keys
- Space on a button presses it

Anything else (Ctrl-C, F-keys) comes back [`FormAction::Ignored`] for the
app. Sized to its fields (at least 64 columns), centered and capped to the
area it is rendered into; when the fields don't fit the height, they show
a page at a time around the focus.

```no_run
# use ratatui::Frame;
use minitui::{Field, Form, FormAction};

# fn demo(frame: &mut Frame, key: crossterm::event::KeyEvent) {
const NAME: usize = 0;
const HUB: usize = 1;
let mut form = Form::new(
    "network",
    vec![
        Field::text("name", "wg0"),
        Field::checkbox("hub", false),
        Field::choice("topology", ["star", "mesh"], 0),
    ],
)
.buttons("apply", "cancel");

match form.handle_key(key) {
    FormAction::Submit if form.text(NAME).trim().is_empty() => {
        form.fail(NAME, "name: required")
    }
    FormAction::Submit => { /* apply form.text(NAME), form.checked(HUB) */ }
    FormAction::Cancel => { /* close it */ }
    _ => {}
}
frame.render_widget(&form, frame.area());
# }
```
*/
#[derive(Debug, Clone)]
#[must_use]
pub struct Form {
    title: Line<'static>,
    fields: Vec<Field>,
    /// Index into the focus ring: the fields, then Submit and Cancel.
    focus: usize,
    buttons: Option<(String, String)>,
    error: Option<String>,
    note: Option<Text<'static>>,
    hint: Option<Line<'static>>,
    style: Style,
    min_width: u16,
}

/// A labeled form field; see the constructors.
#[derive(Debug, Clone)]
pub struct Field {
    label: String,
    kind: FieldKind,
}

#[derive(Debug, Clone)]
enum FieldKind {
    Text(LineEditor),
    Checkbox(bool),
    /// `selected` is always an index into a non-empty `options`.
    Choice {
        options: Vec<String>,
        selected: usize,
    },
}

/// What has the focus in a [`Form`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The field at this index.
    Field(usize),
    Submit,
    Cancel,
}

/// What a key did to a [`Form`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormAction {
    /// A value changed (the validation error is cleared).
    Changed,
    /// A key the form used without changing a value: focus, a cursor move.
    Handled,
    /// Not the form's: Ctrl-C, F-keys, a key release.
    Ignored,
    /// Enter (or the Submit button): read the fields and act, or [`Form::fail`].
    Submit,
    /// Esc (or the Cancel button).
    Cancel,
}

/// The rows below the fields, measured for the inner width they render in.
struct Tail {
    text: Text<'static>,
    height: u16,
}

/* ===== Field ============================================================== */

impl Field {
    /// A text field holding `value`, the cursor at its end.
    pub fn text(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self::editor(label, LineEditor::new(value))
    }

    /// A text field around an editor of the app's making: a secret, a style.
    pub fn editor(label: impl Into<String>, editor: LineEditor) -> Self {
        Self {
            label: label.into(),
            kind: FieldKind::Text(editor),
        }
    }

    pub fn checkbox(label: impl Into<String>, checked: bool) -> Self {
        Self {
            label: label.into(),
            kind: FieldKind::Checkbox(checked),
        }
    }

    /**
    One of `options`, `selected` first (clamped to them), cycled in place.
    An empty list gets a single empty option, so the field always has a
    value to show.
    */
    pub fn choice<S: Into<String>>(
        label: impl Into<String>,
        options: impl IntoIterator<Item = S>,
        selected: usize,
    ) -> Self {
        let mut options: Vec<String> = options.into_iter().map(Into::into).collect();
        if options.is_empty() {
            options.push(String::new());
        }
        let selected = selected.min(options.len() - 1);
        Self {
            label: label.into(),
            kind: FieldKind::Choice { options, selected },
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// The value's natural width: what it takes to show it whole.
    fn value_width(&self) -> usize {
        match &self.kind {
            FieldKind::Text(editor) => Line::from(editor.text()).width() + TEXT_EXTRA_W,
            FieldKind::Checkbox(_) => CHECKED.len(),
            FieldKind::Choice { options, .. } => {
                let widest = options.iter().map(|o| Line::from(o.as_str()).width());
                widest.max().unwrap_or(0) + CHOICE_OPEN.len() + CHOICE_CLOSE.len()
            }
        }
    }

    /// The value as shown in `width` columns.
    fn value_line(&self, width: u16, focused: bool) -> Line<'static> {
        let focus_style = Style::new().fg(FOCUS_COLOR);
        match &self.kind {
            FieldKind::Text(editor) if focused => editor.line(width),
            FieldKind::Text(editor) => editor.idle_line(width),
            FieldKind::Checkbox(checked) => {
                let mark = if *checked { CHECKED } else { UNCHECKED };
                let style = if focused { focus_style } else { Style::new() };
                Line::styled(mark, style)
            }
            FieldKind::Choice { options, selected } => {
                let style = match focused {
                    true => focus_style,
                    false => Style::new().fg(IDLE_CHOICE_COLOR),
                };
                Line::styled(format!("{CHOICE_OPEN}{}{CHOICE_CLOSE}", options[*selected]), style)
            }
        }
    }
}

/* ===== Form =============================================================== */

impl Form {
    /// `title` (shown padded, ` title `) over `fields`; the first one has the focus.
    pub fn new(title: impl Into<Line<'static>>, fields: Vec<Field>) -> Self {
        Self {
            title: title.into(),
            fields,
            focus: 0,
            buttons: None,
            error: None,
            note: None,
            hint: None,
            style: Style::new().fg(INPUT_COLOR),
            min_width: INPUT_MIN_W,
        }
    }

    /// Submit and Cancel buttons below the fields, in the focus ring after them.
    pub fn buttons(mut self, submit: impl Into<String>, cancel: impl Into<String>) -> Self {
        self.buttons = Some((submit.into(), cancel.into()));
        self
    }

    /// Replace the key hint in the footer.
    pub fn hint(mut self, hint: impl Into<Line<'static>>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The box's base style (borders and labels); cyan by default.
    pub fn style(mut self, style: impl Into<Style>) -> Self {
        self.style = style.into();
        self
    }

    /// The narrowest the box gets (64 by default), terminal permitting.
    pub fn min_width(mut self, width: u16) -> Self {
        self.min_width = width;
        self
    }

    /* ----- keys ----- */

    /// Apply one key; see the type docs for the bindings.
    pub fn handle_key(&mut self, key: KeyEvent) -> FormAction {
        if key.kind == KeyEventKind::Release {
            return FormAction::Ignored;
        }
        let press = key.kind == KeyEventKind::Press;
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            // a held-down Enter or Esc must not run on into the next screen
            KeyCode::Esc if press => return FormAction::Cancel,
            KeyCode::Enter if press && self.focus() == Focus::Cancel => {
                return FormAction::Cancel;
            }
            KeyCode::Enter if press => return FormAction::Submit,
            KeyCode::Esc | KeyCode::Enter => return FormAction::Ignored,
            KeyCode::Tab | KeyCode::Down if plain => return self.move_focus(1),
            KeyCode::BackTab | KeyCode::Up if plain => {
                return self.move_focus(self.ring_len() - 1);
            }
            _ => {}
        }

        let space = key.code == KeyCode::Char(' ') && plain;
        let action = match self.focus() {
            Focus::Submit if space && press => FormAction::Submit,
            Focus::Cancel if space && press => FormAction::Cancel,
            Focus::Submit | Focus::Cancel => FormAction::Ignored,
            Focus::Field(i) => match &mut self.fields[i].kind {
                FieldKind::Text(editor) => outcome(editor.handle_key(key)),
                FieldKind::Checkbox(checked) if space => {
                    *checked = !*checked;
                    FormAction::Changed
                }
                FieldKind::Checkbox(_) => FormAction::Ignored,
                FieldKind::Choice { options, selected } => {
                    let n = options.len();
                    match key.code {
                        KeyCode::Right if plain => *selected = (*selected + 1) % n,
                        KeyCode::Char(' ') if plain => *selected = (*selected + 1) % n,
                        KeyCode::Left if plain => *selected = (*selected + n - 1) % n,
                        _ => return FormAction::Ignored,
                    }
                    FormAction::Changed
                }
            },
        };
        if action == FormAction::Changed {
            self.error = None;
        }
        action
    }

    /// Insert `s` (a bracketed paste) into the focused text field, if one is.
    pub fn insert(&mut self, s: &str) -> FormAction {
        let Focus::Field(i) = self.focus() else {
            return FormAction::Ignored;
        };
        let action = match &mut self.fields[i].kind {
            FieldKind::Text(editor) => outcome(editor.insert(s)),
            _ => FormAction::Ignored,
        };
        if action == FormAction::Changed {
            self.error = None;
        }
        action
    }

    /* ----- focus, error, note ----- */

    pub fn focus(&self) -> Focus {
        let n = self.fields.len();
        match self.focus {
            i if i < n => Focus::Field(i),
            i if i == n => Focus::Submit,
            _ => Focus::Cancel,
        }
    }

    /// Move the focus; a field out of range, or a button the form doesn't
    /// have, leaves it where it is.
    pub fn set_focus(&mut self, focus: Focus) {
        let n = self.fields.len();
        let at = match focus {
            Focus::Field(i) => i,
            Focus::Submit => n,
            Focus::Cancel => n + 1,
        };
        if at < self.ring_len() && (at < n || self.buttons.is_some()) {
            self.focus = at;
        }
    }

    /// The validation error shown in place of the hint; cleared by the next change.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    /// A submit refused because of field `field`: show `error` and focus it.
    pub fn fail(&mut self, field: usize, error: impl Into<String>) {
        self.set_focus(Focus::Field(field));
        self.error = Some(error.into());
    }

    /**
    The app's own text between the fields and the footer, wrapped: what a
    submit is doing or did ("resolving…", "added 3, skipped 1"). It stays
    until replaced; `None` removes it.
    */
    pub fn set_note(&mut self, note: Option<Text<'static>>) {
        self.note = note;
    }

    /* ----- values ----- */

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// The text of text field `field`, as typed (untrimmed).
    pub fn text(&self, field: usize) -> &str {
        self.editor(field).text()
    }

    pub fn editor(&self, field: usize) -> &LineEditor {
        match &self.fields[field].kind {
            FieldKind::Text(editor) => editor,
            other => panic!("form field {field} is not text: {other:?}"),
        }
    }

    /// The editor of text field `field`: to reset it after a submit, say.
    pub fn editor_mut(&mut self, field: usize) -> &mut LineEditor {
        match &mut self.fields[field].kind {
            FieldKind::Text(editor) => editor,
            other => panic!("form field {field} is not text: {other:?}"),
        }
    }

    pub fn checked(&self, field: usize) -> bool {
        match &self.fields[field].kind {
            FieldKind::Checkbox(checked) => *checked,
            other => panic!("form field {field} is not a checkbox: {other:?}"),
        }
    }

    pub fn set_checked(&mut self, field: usize, value: bool) {
        match &mut self.fields[field].kind {
            FieldKind::Checkbox(checked) => *checked = value,
            other => panic!("form field {field} is not a checkbox: {other:?}"),
        }
    }

    /// The index of choice field `field`'s selected option.
    pub fn selected(&self, field: usize) -> usize {
        match &self.fields[field].kind {
            FieldKind::Choice { selected, .. } => *selected,
            other => panic!("form field {field} is not a choice: {other:?}"),
        }
    }

    /// Select option `index` of choice field `field`, clamped to the options.
    pub fn set_selected(&mut self, field: usize, index: usize) {
        match &mut self.fields[field].kind {
            FieldKind::Choice { options, selected } => *selected = index.min(options.len() - 1),
            other => panic!("form field {field} is not a choice: {other:?}"),
        }
    }

    /* ----- layout ----- */

    /// Where the form lands when rendered into `outer`.
    pub fn area(&self, outer: Rect) -> Rect {
        let (width, inner_w, chrome_h) = self.width(outer);
        let tail = self.tail(inner_w);
        let rows = to_u16(self.fields.len()).min(self.field_rows(outer, chrome_h, &tail));
        let height = rows.saturating_add(tail.height).saturating_add(chrome_h);
        centered(outer, width, height)
    }

    /// Box width, inner width and chrome height for `outer`.
    fn width(&self, outer: Rect) -> (u16, u16, u16) {
        let (chrome_w, chrome_h) = chrome(&popup_block(self.title.clone()));
        let fields_w = self
            .fields
            .iter()
            .map(Field::value_width)
            .max()
            .map_or(0, |w| MARK_W + self.label_width() + LABEL_GAP + w);
        let content_w = [
            fields_w,
            self.buttons_line().width(),
            self.hint_line().width(),
            self.note.as_ref().map_or(0, Text::width),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        // max-then-min, not clamp: clamp(min, w) panics when w < min
        let width = to_u16(content_w)
            .saturating_add(chrome_w)
            .max(self.min_width)
            .min(outer.width);
        (width, width.saturating_sub(chrome_w), chrome_h)
    }

    /// The rows the fields get in `outer`: what the chrome and the tail leave, at least one.
    fn field_rows(&self, outer: Rect, chrome_h: u16, tail: &Tail) -> u16 {
        outer
            .height
            .saturating_sub(chrome_h)
            .saturating_sub(tail.height)
            .max(1)
    }

    /**
    The first field shown when `rows` of them fit: pages of `rows` fields,
    the one holding the focus (the last when a button has it), the last
    page kept full.
    */
    fn first_shown(&self, rows: usize) -> usize {
        let n = self.fields.len();
        if n <= rows {
            return 0;
        }
        let focus = match self.focus() {
            Focus::Field(i) => i,
            Focus::Submit | Focus::Cancel => n - 1,
        };
        (focus / rows * rows).min(n - rows)
    }

    fn label_width(&self) -> usize {
        let widths = self.fields.iter().map(|f| Line::from(f.label.as_str()).width());
        widths.max().unwrap_or(0)
    }

    /// Below the fields: a blank, the note and a blank, the buttons and a
    /// blank, the error or the hint.
    fn tail(&self, inner_w: u16) -> Tail {
        let mut text = Text::default();
        text.push_line(Line::default());
        if let Some(note) = &self.note {
            text.extend(note.clone());
            text.push_line(Line::default());
        }
        if self.buttons.is_some() {
            text.push_line(self.buttons_line().right_aligned());
            text.push_line(Line::default());
        }
        text.push_line(match &self.error {
            Some(error) => Line::from(error.clone()).style(Style::new().fg(ERROR_COLOR)),
            None => self.hint_line(),
        });
        let height = wrap_lines(text.clone(), inner_w);
        Tail { text, height }
    }

    fn buttons_line(&self) -> Line<'static> {
        let Some((submit, cancel)) = &self.buttons else {
            return Line::default();
        };
        let button = |label: &str, color: Color, focused: bool| {
            let style = Style::new().fg(color).bold();
            let style = if focused { style.reversed() } else { style };
            Span::styled(format!("[ {label} ]"), style)
        };
        Line::from(vec![
            button(submit, SUBMIT_COLOR, self.focus() == Focus::Submit),
            Span::raw(" ".repeat(BUTTON_GAP)),
            button(cancel, CANCEL_COLOR, self.focus() == Focus::Cancel),
        ])
    }

    /// The default hint names only the keys this form's fields take.
    fn hint_line(&self) -> Line<'static> {
        if let Some(hint) = &self.hint {
            return hint.clone();
        }
        let mut hint = HINT_BASE.to_string();
        let has = |f: fn(&FieldKind) -> bool| self.fields.iter().any(|field| f(&field.kind));
        if has(|k| matches!(k, FieldKind::Checkbox(_))) {
            hint.push_str(HINT_CHECKBOX);
        }
        if has(|k| matches!(k, FieldKind::Choice { .. })) {
            hint.push_str(HINT_CHOICE);
        }
        Line::from(hint).style(Style::new().fg(HINT_COLOR))
    }

    /// One field's row: focus mark, label padded to the label column, value.
    fn field_line(&self, index: usize, label_w: usize, inner_w: u16) -> Line<'static> {
        let field = &self.fields[index];
        let focused = self.focus() == Focus::Field(index);
        let (mark, label_style) = match focused {
            true => (MARK_FOCUS, Style::new().fg(FOCUS_COLOR).bold()),
            false => (MARK_IDLE, Style::new()),
        };
        let pad = label_w - Line::from(field.label.as_str()).width() + LABEL_GAP;
        let value_w = inner_w.saturating_sub(to_u16(MARK_W + label_w + LABEL_GAP));
        let mut spans = vec![
            Span::styled(mark, label_style),
            Span::styled(field.label.clone(), label_style),
            Span::raw(" ".repeat(pad)),
        ];
        spans.extend(field.value_line(value_w, focused).spans);
        Line::from(spans)
    }

    fn move_focus(&mut self, by: usize) -> FormAction {
        self.focus = (self.focus + by) % self.ring_len();
        FormAction::Handled
    }

    /// The fields and the buttons; at least 1, so the ring arithmetic holds.
    fn ring_len(&self) -> usize {
        let buttons = if self.buttons.is_some() { 2 } else { 0 };
        (self.fields.len() + buttons).max(1)
    }
}

/**
The form centered in the area (normally the whole frame), cells beneath
cleared. When not every field fits, the bottom border tells which are shown.
*/
impl Widget for &Form {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (width, inner_w, chrome_h) = self.width(area);
        let tail = self.tail(inner_w);
        let n = self.fields.len();
        let rows = usize::from(self.field_rows(area, chrome_h, &tail)).min(n);
        let first = self.first_shown(rows);

        let mut block = popup_block(self.title.clone()).style(self.style);
        if rows < n {
            let shown = format!(" fields {}–{} of {n} ", first + 1, first + rows);
            block = block.title_bottom(Line::from(shown).right_aligned());
        }
        let height = to_u16(rows)
            .saturating_add(tail.height)
            .saturating_add(chrome_h);
        let popup = centered(area, width, height);
        let inner = block.inner(popup);
        Clear.render(popup, buf);
        block.render(popup, buf);

        // the fields unwrapped (one row each, as the paging counts them), the tail wrapped
        let label_w = self.label_width();
        let lines: Vec<Line> = (first..first + rows)
            .map(|i| self.field_line(i, label_w, inner.width))
            .collect();
        let fields_h = to_u16(rows).min(inner.height);
        Paragraph::new(lines).render(Rect { height: fields_h, ..inner }, buf);
        let tail_area = Rect {
            y: inner.y + fields_h,
            height: inner.height - fields_h,
            ..inner
        };
        Paragraph::new(tail.text)
            .wrap(Wrap { trim: false })
            .render(tail_area, buf);
    }
}

/* ===== helpers ============================================================ */

fn outcome(edit: EditOutcome) -> FormAction {
    match edit {
        EditOutcome::Changed => FormAction::Changed,
        EditOutcome::Handled => FormAction::Handled,
        EditOutcome::Ignored => FormAction::Ignored,
    }
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: usize = 0;
    const HUB: usize = 1;
    const TOPO: usize = 2;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn form() -> Form {
        Form::new(
            "network",
            vec![
                Field::text("name", "wg0"),
                Field::checkbox("hub", false),
                Field::choice("topology", ["star", "mesh"], 0),
            ],
        )
    }

    fn screen(form: &Form, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        form.render(area, &mut buf);
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn focus_cycles_both_ways_through_the_buttons() {
        let mut f = form().buttons("apply", "cancel");
        assert_eq!(f.handle_key(key(KeyCode::Tab)), FormAction::Handled);
        assert_eq!(f.focus(), Focus::Field(HUB));
        f.handle_key(key(KeyCode::Down));
        f.handle_key(key(KeyCode::Down));
        assert_eq!(f.focus(), Focus::Submit);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.focus(), Focus::Cancel);
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.focus(), Focus::Field(NAME));
        f.handle_key(key(KeyCode::BackTab));
        assert_eq!(f.focus(), Focus::Cancel);
        f.handle_key(key(KeyCode::Up));
        assert_eq!(f.focus(), Focus::Submit);
        // without buttons the ring is just the fields
        let mut f = form();
        f.handle_key(key(KeyCode::Up));
        assert_eq!(f.focus(), Focus::Field(TOPO));
    }

    #[test]
    fn enter_submits_from_anywhere_but_cancel() {
        let mut f = form().buttons("apply", "cancel");
        for focus in [Focus::Field(NAME), Focus::Field(HUB), Focus::Field(TOPO), Focus::Submit] {
            f.set_focus(focus);
            assert_eq!(f.handle_key(key(KeyCode::Enter)), FormAction::Submit, "{focus:?}");
        }
        assert!(!f.checked(HUB), "Enter on a checkbox doesn't toggle it");
        f.set_focus(Focus::Cancel);
        assert_eq!(f.handle_key(key(KeyCode::Enter)), FormAction::Cancel);
        assert_eq!(f.handle_key(key(KeyCode::Char(' '))), FormAction::Cancel);
        f.set_focus(Focus::Submit);
        assert_eq!(f.handle_key(key(KeyCode::Char(' '))), FormAction::Submit);
        assert_eq!(f.handle_key(key(KeyCode::Esc)), FormAction::Cancel);
        let mut held = key(KeyCode::Enter);
        held.kind = KeyEventKind::Repeat;
        assert_eq!(f.handle_key(held), FormAction::Ignored, "a held-down Enter");
    }

    #[test]
    fn values_edit_in_place() {
        let mut f = form();
        assert_eq!(f.handle_key(key(KeyCode::Char('1'))), FormAction::Changed);
        assert_eq!(f.text(NAME), "wg01");
        assert_eq!(f.handle_key(key(KeyCode::Left)), FormAction::Handled);
        f.set_focus(Focus::Field(HUB));
        assert_eq!(f.handle_key(key(KeyCode::Char(' '))), FormAction::Changed);
        assert!(f.checked(HUB));
        assert_eq!(f.handle_key(key(KeyCode::Char('x'))), FormAction::Ignored);
        f.set_focus(Focus::Field(TOPO));
        f.handle_key(key(KeyCode::Right));
        assert_eq!(f.selected(TOPO), 1);
        f.handle_key(key(KeyCode::Char(' '))); // wraps
        assert_eq!(f.selected(TOPO), 0);
        f.handle_key(key(KeyCode::Left));
        assert_eq!(f.selected(TOPO), 1);
    }

    #[test]
    fn the_apps_keys_are_left_to_the_app() {
        let mut f = form();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(f.handle_key(ctrl_c), FormAction::Ignored);
        assert_eq!(f.handle_key(key(KeyCode::F(2))), FormAction::Ignored);
        let mut release = key(KeyCode::Tab);
        release.kind = KeyEventKind::Release;
        assert_eq!(f.handle_key(release), FormAction::Ignored);
        assert_eq!(f.focus(), Focus::Field(NAME));
    }

    #[test]
    fn a_failed_submit_focuses_the_field_until_the_next_change() {
        let mut f = form();
        f.set_focus(Focus::Field(TOPO));
        f.fail(NAME, "name: taken");
        assert_eq!(f.focus(), Focus::Field(NAME));
        assert_eq!(f.error(), Some("name: taken"));
        assert!(screen(&f, 80, 20).contains("name: taken"));
        f.handle_key(key(KeyCode::Left));
        assert!(f.error().is_some(), "a cursor move isn't a change");
        f.handle_key(key(KeyCode::Char('2')));
        assert_eq!(f.error(), None);
    }

    #[test]
    fn a_paste_goes_to_the_focused_text_field() {
        let mut f = form();
        assert_eq!(f.insert("-a\nb"), FormAction::Changed);
        assert_eq!(f.text(NAME), "wg0-ab");
        f.set_focus(Focus::Field(HUB));
        assert_eq!(f.insert("x"), FormAction::Ignored);
    }

    #[test]
    fn set_focus_ignores_what_isnt_there() {
        let mut f = form();
        f.set_focus(Focus::Submit);
        assert_eq!(f.focus(), Focus::Field(NAME), "no buttons");
        f.set_focus(Focus::Field(9));
        assert_eq!(f.focus(), Focus::Field(NAME));
    }

    #[test]
    fn choices_are_never_empty_or_out_of_range() {
        let mut f = Form::new(
            "t",
            vec![
                Field::choice("a", Vec::<String>::new(), 3),
                Field::choice("b", ["x", "y"], 9),
            ],
        );
        assert_eq!(f.selected(0), 0);
        assert_eq!(f.selected(1), 1);
        f.set_selected(1, 7);
        assert_eq!(f.selected(1), 1);
        assert_eq!(f.handle_key(key(KeyCode::Right)), FormAction::Changed);
        assert_eq!(f.selected(0), 0);
    }

    #[test]
    #[should_panic(expected = "not a checkbox")]
    fn the_wrong_kind_panics() {
        form().checked(NAME);
    }

    #[test]
    fn renders_aligned_fields_and_the_hint() {
        let mut f = form().buttons("apply", "cancel");
        f.set_checked(HUB, true);
        let text = screen(&f, 80, 20);
        assert!(text.contains("┌ network ─"), "{text}");
        assert!(text.contains("▶ name      > wg0"), "{text}");
        assert!(text.contains("  hub       [x]"), "{text}");
        assert!(text.contains("  topology  ‹ star ›"), "{text}");
        assert!(text.contains("[ apply ]  [ cancel ]"), "{text}");
        assert!(text.contains("Space toggle · ←→ choose"), "{text}");
        let plain = Form::new("t", vec![Field::text("a", "")]);
        assert!(!screen(&plain, 80, 10).contains("Space toggle"));
    }

    #[test]
    fn labels_line_up_by_display_width() {
        let f = Form::new("t", vec![Field::text("名前", "x"), Field::text("id", "y")]);
        let area = Rect::new(0, 0, 80, 10);
        let mut buf = Buffer::empty(area);
        f.render(area, &mut buf);
        // the buffer column of the value's first cell (a wide char's
        // trailing cell is blank, so the screen text can't say it)
        let col = |value: &str| {
            let cells = (0..10).flat_map(|y| (0..79).map(move |x| (x, y)));
            let found = cells
                .filter(|&(x, y)| buf[(x, y)].symbol() == ">" && buf[(x + 2, y)].symbol() == value);
            found.map(|(x, _)| x).next().unwrap()
        };
        assert_eq!(col("x"), col("y"));
    }

    #[test]
    fn a_note_shows_between_fields_and_footer() {
        let mut f = form();
        f.set_note(Some(Text::from("resolving 3 names…")));
        let area = f.area(Rect::new(0, 0, 80, 30));
        let without = form().area(Rect::new(0, 0, 80, 30));
        assert_eq!(area.height, without.height + 2, "the note and its blank");
        assert!(screen(&f, 80, 30).contains("resolving 3 names…"));
    }

    #[test]
    fn a_tall_form_pages_around_the_focus() {
        let fields = (0..16).map(|i| Field::text(format!("f{i:02}"), format!("v{i}")));
        let mut f = Form::new("config", fields.collect()).buttons("save", "cancel");
        // 12 rows: borders 2, blank + buttons + blank + hint 4: 6 fields a page
        let text = screen(&f, 80, 12);
        assert!(text.contains("f00") && text.contains("f05") && !text.contains("f06"));
        assert!(text.contains("fields 1–6 of 16"), "{text}");
        assert!(text.contains("[ save ]"), "the tail always shows");
        f.set_focus(Focus::Field(7));
        let text = screen(&f, 80, 12);
        assert!(text.contains("f06") && text.contains("▶ f07") && !text.contains("f05"));
        // the last page is full, a button shows it
        f.set_focus(Focus::Cancel);
        let text = screen(&f, 80, 12);
        assert!(text.contains("f10") && text.contains("f15"), "{text}");
        assert!(text.contains("fields 11–16 of 16"));
    }

    #[test]
    fn a_long_value_scrolls_in_its_field() {
        let long = "x".repeat(200) + "END";
        let mut f = Form::new("t", vec![Field::text("addr", long.clone()), Field::text("b", "")]);
        let text = screen(&f, 80, 10);
        assert!(text.contains("END"), "the focused field shows the cursor end");
        f.set_focus(Focus::Field(1));
        let text = screen(&f, 80, 10);
        assert!(text.contains("> xxx") && text.contains('…') && !text.contains("END"));
    }

    #[test]
    fn a_tiny_area_does_not_panic() {
        let f = form().buttons("a", "b");
        for (w, h) in [(0, 0), (1, 1), (5, 3), (20, 4)] {
            screen(&f, w, h);
            let area = f.area(Rect::new(0, 0, w, h));
            assert!(area.width <= w && area.height <= h);
        }
        screen(&Form::new("empty", Vec::new()), 40, 10);
    }
}
