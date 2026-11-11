use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use super::fields::{self, draw_field, label_width, Status};
use super::{draw_footer, draw_row, pane_box, theme};
use crate::app::fields::Kind;
use crate::app::form::FormState;
use crate::app::App;
use crate::sample::Tone;
use crate::source::FormRef;

const LABEL_W: usize = 22;
const FOOTER: &str = " tab next · ctrl-s save · esc cancel";
const CHOOSE: &str = " · ←→ choose";
/// The delete confirmation saves nothing.
const DELETE_FOOTER: &str = " enter delete · esc cancel";

/// One row of the part that scrolls.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line<'a> {
    /// A field's row: its label and value, then a Lines field's further lines.
    Field(usize, usize),
    /// A row of a field's help or error, wrapped under its value.
    Under(usize, &'a str, Tone, u16),
    Gap,
    Action(usize),
}

impl Line<'_> {
    /// The focus position whose row it is.
    fn owner(self, fields: usize) -> Option<usize> {
        match self {
            Line::Field(index, _) | Line::Under(index, ..) => Some(index),
            Line::Action(index) => Some(fields + index),
            Line::Gap => None,
        }
    }
}

/// What the fields and actions scroll in, their label width, their rows, and the first shown.
struct Body<'a> {
    rect: Rect,
    width: u16,
    lines: Vec<Line<'a>>,
    first: usize,
}

fn note(form: &FormState) -> Option<Paragraph<'_>> {
    (!form.note.is_empty()).then(|| {
        Paragraph::new(form.note.join("\n"))
            .style(theme::tone(Tone::Label))
            .wrap(Wrap { trim: false })
    })
}

/// Under the note and a gap after it, above the status.
fn body<'a>(form: &'a FormState, area: Rect, status: &Status) -> Body<'a> {
    let note = note(form).map_or(0, |note| note.line_count(area.width) + 1) as u16;
    let top = area.y.saturating_add(note).min(area.bottom());
    let bottom = area.bottom().saturating_sub(2 + status.rows).max(top);
    let rect = Rect {
        y: top,
        height: bottom - top,
        ..area
    };
    let width = label_width(form.fields.iter().map(|entry| &entry.field), LABEL_W);
    let room = rect.width.saturating_sub(1 + width);
    let lines = lines(form, room);
    let first = first_line(&lines, form, usize::from(rect.height));
    Body {
        rect,
        width,
        lines,
        first,
    }
}

fn lines(form: &FormState, room: u16) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    for (index, entry) in form.fields.iter().enumerate() {
        if !form.shows(entry) {
            continue;
        }
        lines.extend((0..entry.field.rows()).map(|row| Line::Field(index, row)));
        let help = entry.help.as_deref().map(|text| (text, Tone::Label));
        let error = entry.error.as_deref().map(|text| (text, Tone::Warn));
        for (text, tone) in help.into_iter().chain(error) {
            let rows = Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .line_count(room);
            lines.extend((0..rows as u16).map(|row| Line::Under(index, text, tone, row)));
        }
    }
    if !form.actions.is_empty() {
        lines.push(Line::Gap);
    }
    lines.extend((0..form.actions.len()).map(Line::Action));
    lines
}

/// From the top until the focused rows would pass the bottom, then so they end there.
fn first_line(lines: &[Line], form: &FormState, height: usize) -> usize {
    let mine = |line: &Line| line.owner(form.fields.len()) == Some(form.focus);
    let (Some(first), Some(last)) = (lines.iter().position(mine), lines.iter().rposition(mine))
    else {
        return 0;
    };
    (last + 1).saturating_sub(height).min(first)
}

pub fn draw(app: &App, rect: Rect, focused: bool, buf: &mut Buffer) {
    let Some(form) = &app.form else {
        return;
    };
    pane_box(buf, rect, &form.title, focused);
    let area = rect.inner(Margin::new(1, 1));
    let status = Status::new(form.status.as_ref(), area);
    let body = body(form, area, &status);
    if let Some(note) = note(form) {
        let height = body.rect.y - area.y;
        note.render(Rect { height, ..area }, buf);
    }
    let value_x = body.rect.x + 1 + body.width;
    let room = body.rect.width.saturating_sub(1 + body.width);
    let rows = body.rect.top()..body.rect.bottom();
    for (line, y) in body.lines[body.first..].iter().zip(rows) {
        let row = Rect {
            y,
            height: 1,
            ..body.rect
        };
        match *line {
            Line::Field(index, at) => {
                draw_field(&form.fields[index].field, body.width, at, row, buf)
            }
            Line::Under(_, text, tone, at) => {
                let under = Rect {
                    x: value_x,
                    width: room,
                    ..row
                };
                Paragraph::new(text)
                    .style(theme::tone(tone))
                    .wrap(Wrap { trim: false })
                    .scroll((at, 0))
                    .render(under, buf);
            }
            Line::Gap => {}
            Line::Action(index) => {
                let label = format!(" {}", form.actions[index].label);
                let on = form.focus == form.fields.len() + index;
                draw_row(buf, body.rect, y, &label, Tone::Accent, on);
            }
        }
    }
    status.draw(area, buf);
    let footer = match form.form {
        FormRef::Delete(..) => DELETE_FOOTER.to_string(),
        _ if form.on_choice() => format!("{FOOTER}{CHOOSE}"),
        _ => FOOTER.to_string(),
    };
    draw_footer(app, area, area.bottom().saturating_sub(1), &footer, buf);
}

/// On the focused field, while it is in view.
pub fn cursor(form: &FormState, rect: Rect) -> Option<Position> {
    let field = form.focused()?;
    let area = rect.inner(Margin::new(1, 1));
    let body = body(form, area, &Status::new(form.status.as_ref(), area));
    let row = match &field.kind {
        Kind::Lines(lines) => lines.row() - lines.shown().start,
        _ => 0,
    };
    let at = body
        .lines
        .iter()
        .position(|line| *line == Line::Field(form.focus, row))?;
    let y = body.rect.y + u16::try_from(at.checked_sub(body.first)?).ok()?;
    let line = Rect {
        y,
        height: 1,
        ..body.rect
    };
    (y < body.rect.bottom()).then(|| fields::cursor(field, body.width, line))
}
