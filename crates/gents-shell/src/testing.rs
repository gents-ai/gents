use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

use crate::app::layers::LayerKind;
use crate::app::session_rows;
use crate::app::{App, ChatLine};
use crate::draw;
use crate::sample::{self, Tone};

/// One token of the `press` grammar. SHIFT follows uppercase as crossterm reports it
/// (`crossterm-0.29.0/src/event/sys/unix/parse.rs:129-135`).
fn key(token: &str) -> KeyEvent {
    let named = match token {
        "esc" => Some(KeyCode::Esc),
        "enter" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "bs" => Some(KeyCode::Backspace),
        "del" => Some(KeyCode::Delete),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pgup" => Some(KeyCode::PageUp),
        "pgdn" => Some(KeyCode::PageDown),
        _ => None,
    };
    if let Some(code) = named {
        return KeyEvent::new(code, KeyModifiers::NONE);
    }
    let (modifiers, text) = match token.split_once('-') {
        Some(("ctrl", text)) if !text.is_empty() => (KeyModifiers::CONTROL, text),
        Some(("alt", text)) if !text.is_empty() => (KeyModifiers::ALT, text),
        _ => (KeyModifiers::NONE, token),
    };
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        panic!("unknown key token: {token}");
    };
    let shift = if c.is_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    KeyEvent::new(KeyCode::Char(c), modifiers | shift)
}

pub fn keys(tokens: &str) -> Vec<KeyEvent> {
    tokens.split_whitespace().map(key).collect()
}

pub fn press(app: &mut App, tokens: &str) {
    for key in keys(tokens) {
        app.handle(Event::Key(key));
    }
}

pub fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        let shift = if c.is_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        app.handle(Event::Key(KeyEvent::new(KeyCode::Char(c), shift)));
    }
}

pub fn buffer(app: &App) -> Buffer {
    let (width, height) = app.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| draw::draw(app, frame))
        .expect("draw to the test terminal");
    terminal.backend().buffer().clone()
}

pub fn rows(buffer: &Buffer) -> Vec<String> {
    let area = buffer.area;
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

pub fn screen(app: &App) -> Vec<String> {
    rows(&buffer(app))
}

/// The first match, as (row, column), counting columns in cells.
pub fn find(screen: &[String], text: &str) -> Option<(u16, u16)> {
    screen.iter().enumerate().find_map(|(row, line)| {
        let byte = line.find(text)?;
        Some((row as u16, line[..byte].chars().count() as u16))
    })
}

pub fn numbered(count: usize) -> Vec<ChatLine> {
    (0..count)
        .map(|n| ChatLine {
            text: format!("message {n}"),
            tone: Tone::Plain,
            item: None,
        })
        .collect()
}

pub fn session_row(text: &str) -> usize {
    session_rows(&sample::sessions())
        .iter()
        .position(|row| row.text.contains(text))
        .expect("a sample session")
}

/// The right pane starts closed. This opens it for tests about what it shows and where it sits.
pub fn with_right_pane(mut app: App) -> App {
    app.tabs[app.active].sidebar = true;
    app
}

pub fn kinds(app: &App) -> Vec<LayerKind> {
    app.layers.iter().map(|layer| layer.kind).collect()
}
