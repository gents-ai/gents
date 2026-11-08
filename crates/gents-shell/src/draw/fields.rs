use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::widgets::{Paragraph, Widget, Wrap};

use super::theme;
use crate::app::fields::{Field, Kind};
use crate::app::input::TAB;
use crate::sample::Tone;

/// Wide enough for the longest label and a space after it, and at least `least`.
pub fn label_width<'a, I>(fields: I, least: usize) -> u16
where
    I: IntoIterator<Item = &'a Field>,
{
    let longest = fields.into_iter().map(|field| field.label.len() + 1).max();
    longest.unwrap_or(0).max(least) as u16
}

/// Each field on its own row of `list`, after its label `width` wide.
pub fn draw_fields(fields: &[Field], width: u16, list: Rect, buf: &mut Buffer) {
    for (field, y) in fields.iter().zip(list.top()..list.bottom()) {
        draw_field(
            field,
            width,
            0,
            Rect {
                y,
                height: 1,
                ..list
            },
            buf,
        );
    }
}

/// `field`'s `row`th row on the row `line`: its label and value, then a Lines field's further
/// lines under its value.
pub fn draw_field(field: &Field, width: u16, row: usize, line: Rect, buf: &mut Buffer) {
    let label = if row == 0 { field.label.as_str() } else { "" };
    let style = match field.kind {
        Kind::Heading => theme::tone(Tone::Plain).add_modifier(Modifier::BOLD),
        _ => theme::tone(Tone::Label),
    };
    let label = format!(" {label:<w$}", w = usize::from(width));
    buf.set_stringn(line.x, line.y, label, usize::from(line.width), style);
    let room = line.width.saturating_sub(1 + width);
    let (text, tone) = value(field, row, room);
    let x = line.x + 1 + width;
    buf.set_stringn(x, line.y, text, usize::from(room), theme::tone(tone));
}

fn value(field: &Field, row: usize, room: u16) -> (String, Tone) {
    let input = match &field.kind {
        Kind::Heading => return (String::new(), Tone::Plain),
        Kind::Choice(choices) => {
            let value = field.input.text();
            let shown = choices.iter().find(|(choice, _)| choice == value);
            let label = shown.map_or(value, |(_, label)| label.as_str());
            return (label.to_string(), Tone::Plain);
        }
        Kind::Lines(lines) => match lines.lines().get(lines.shown().start + row) {
            Some(line) => line,
            None => return (String::new(), Tone::Plain),
        },
        Kind::Text | Kind::Fixed => &field.input,
    };
    let shown = input.view(room).0.replace('\t', TAB);
    if field.is_empty() {
        (field.placeholder.clone(), Tone::Label)
    } else if field.masked {
        ("•".repeat(shown.chars().count()), Tone::Plain)
    } else if matches!(field.kind, Kind::Fixed) {
        (shown, Tone::Label)
    } else {
        (shown, Tone::Plain)
    }
}

/// On `field` drawn at `line`: in a Lines field, `line` is its cursor's row.
// ponytail: a masked key's cursor takes one column per character, as API keys are ASCII.
pub fn cursor(field: &Field, width: u16, line: Rect) -> Position {
    let room = line.width.saturating_sub(1 + width);
    let column = match &field.kind {
        Kind::Lines(lines) => lines.current().view(room).1,
        Kind::Choice(_) => 0,
        _ => field.input.view(room).1,
    };
    Position::new(line.x + 1 + width + column, line.y)
}

/// The status over the footer, the last row of the area. Wrapped so a long cause shows whole,
/// it takes rows from what is above it, down to one.
pub struct Status<'a> {
    text: Option<Paragraph<'a>>,
    pub rows: u16,
}

impl<'a> Status<'a> {
    pub fn new(status: Option<&'a (String, Tone)>, area: Rect) -> Status<'a> {
        let text = status.map(|(text, tone)| {
            Paragraph::new(text.as_str())
                .style(theme::tone(*tone))
                .wrap(Wrap { trim: false })
        });
        let most = usize::from(area.height.saturating_sub(2)).max(1);
        let rows = text.as_ref().map_or(1, |text| text.line_count(area.width));
        Status {
            text,
            rows: rows.clamp(1, most) as u16,
        }
    }

    pub fn draw(self, area: Rect, buf: &mut Buffer) {
        if let Some(text) = self.text {
            let y = area.bottom().saturating_sub(1 + self.rows);
            let rect = Rect {
                y,
                height: self.rows,
                ..area
            };
            text.render(rect, buf);
        }
    }
}
