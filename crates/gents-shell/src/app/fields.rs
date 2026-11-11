use super::input::{LineInput, Lines};
use crate::keys::Action;
use crate::sample::Tone;
use crate::source::Connection;

/// How a field takes keys and shows what it holds.
pub enum Kind {
    Text,
    /// Shown, never focused.
    Fixed,
    /// A group's bold title, never focused.
    Heading,
    /// Its `(value, label)` pairs: `input` holds the value and the label is shown.
    Choice(Vec<(String, String)>),
    Lines(Lines),
}

/// Not `Debug`: an API key's field holds the key.
pub struct Field {
    pub label: String,
    pub placeholder: String,
    pub input: LineInput,
    /// Shown as one `•` per character.
    pub masked: bool,
    pub kind: Kind,
}

impl Field {
    pub fn new(label: &str, placeholder: &str, text: &str) -> Field {
        let mut input = LineInput::default();
        input.set(text);
        Field {
            label: label.to_string(),
            placeholder: placeholder.to_string(),
            input,
            masked: false,
            kind: Kind::Text,
        }
    }

    pub fn focusable(&self) -> bool {
        !matches!(self.kind, Kind::Fixed | Kind::Heading)
    }

    /// A Lines field's lines are joined with `\n`.
    pub fn value(&self) -> String {
        match &self.kind {
            Kind::Lines(lines) => lines.text(),
            _ => self.input.text().to_string(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.kind {
            Kind::Lines(lines) => lines.text().is_empty(),
            _ => self.input.is_empty(),
        }
    }

    /// The rows it is drawn in.
    pub fn rows(&self) -> usize {
        match &self.kind {
            Kind::Lines(lines) => lines.shown().len(),
            _ => 1,
        }
    }

    /// ← and → or Space cycle a choice, which takes no typing.
    pub fn edit(&mut self, action: Action) {
        match &mut self.kind {
            Kind::Text => self.input.edit(action),
            Kind::Lines(lines) => lines.edit(action),
            Kind::Choice(choices) => {
                let forward = match action {
                    Action::CursorRight | Action::Insert(' ') => true,
                    Action::CursorLeft => false,
                    _ => return,
                };
                let now = choices
                    .iter()
                    .position(|(value, _)| value == self.input.text());
                let next = cycle(now, choices.len(), forward);
                if let Some((value, _)) = choices.get(next) {
                    self.input.set(value);
                }
            }
            Kind::Fixed | Kind::Heading => {}
        }
    }

    /// A Lines field keeps the paste's line breaks; a text field takes it as one line.
    pub fn paste(&mut self, text: &str) {
        match &mut self.kind {
            Kind::Text => self.input.paste_inline(text),
            Kind::Lines(lines) => lines.paste(text),
            Kind::Fixed | Kind::Heading | Kind::Choice(_) => {}
        }
    }
}

/// A lost source reads no more intents, so a layer waiting on one says why in `status`.
pub fn status_says_lost(connection: &Connection, status: &mut Option<(String, Tone)>) -> bool {
    let Connection::Lost(why) = connection else {
        return false;
    };
    *status = Some((format!(" could not reach the runtime: {why}"), Tone::Warn));
    true
}

/// The position after `from`, or before it, that `focusable` takes, wrapping: `from` when no
/// other does.
pub fn next_focus<F>(from: usize, count: usize, forward: bool, focusable: F) -> usize
where
    F: Fn(usize) -> bool,
{
    (1..=count)
        .map(|offset| {
            if forward {
                (from + offset) % count
            } else {
                (from + count - offset % count) % count
            }
        })
        .find(|&index| focusable(index))
        .unwrap_or(from)
}

/// The choice after `current`, or before it, wrapping; the first when none is current.
pub fn cycle(current: Option<usize>, len: usize, forward: bool) -> usize {
    match current {
        Some(now) if forward => (now + 1) % len,
        Some(now) => (now + len - 1) % len,
        None => 0,
    }
}
