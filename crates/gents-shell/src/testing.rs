use std::sync::mpsc::{channel, Receiver};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;

use crate::app::layers::LayerKind;
use crate::app::session_rows;
use crate::app::{App, ChatLine};
use crate::draw;
use crate::keys::KeySet;
use crate::layout::{layout, Right, Screen};
use crate::sample::{self, Tone};
use crate::source::Intent;

/// One token of the `press` grammar. SHIFT follows uppercase as crossterm reports it
/// (`crossterm-0.29.0/src/event/sys/unix/parse.rs:129-135`).
fn key(token: &str) -> KeyEvent {
    if token == "shift-tab" {
        return KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    }
    let (modifiers, text) = match token.split_once('-') {
        Some(("ctrl", text)) if !text.is_empty() => (KeyModifiers::CONTROL, text),
        Some(("alt", text)) if !text.is_empty() => (KeyModifiers::ALT, text),
        _ => (KeyModifiers::NONE, token),
    };
    let named = match text {
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
        return KeyEvent::new(code, modifiers);
    }
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

/// An app on a runtime, and what it sends there.
pub fn connected(set: KeySet, width: u16, height: u16) -> (App, Receiver<Intent>) {
    let (intents, intended) = channel();
    (App::connected(set, width, height, intents), intended)
}

/// Everything the app sent.
pub fn drain(intents: &Receiver<Intent>) -> Vec<Intent> {
    intents.try_iter().collect()
}

/// What the app sent, beside the skills it reads as the slash list opens.
pub fn sent(intents: &Receiver<Intent>) -> Vec<Intent> {
    let read = |intent: &Intent| matches!(intent, Intent::ReadSkills { .. });
    intents.try_iter().filter(|intent| !read(intent)).collect()
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

/// The docked right pane's rows, cut from the screen at its left edge; none while it is not docked.
pub fn right_pane(app: &App) -> Vec<String> {
    let (width, height) = app.size;
    let Screen::Shell(panes) = layout(width, height, app.wanted()) else {
        return Vec::new();
    };
    let Right::Sidebar(pane) = panes.right else {
        return Vec::new();
    };
    let left = usize::from(pane.x);
    screen(app)
        .iter()
        .map(|row| row.chars().skip(left).collect())
        .collect()
}

/// The first match, as (row, column), counting columns in cells.
pub fn find(screen: &[String], text: &str) -> Option<(u16, u16)> {
    screen.iter().enumerate().find_map(|(row, line)| {
        let byte = line.find(text)?;
        Some((row as u16, line[..byte].chars().count() as u16))
    })
}

pub fn click_text(app: &mut App, text: &str) {
    let (row, column) = find(&screen(app), text).expect(text);
    app.handle(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }));
}

pub fn numbered(count: usize) -> Vec<ChatLine> {
    (0..count)
        .map(|n| ChatLine {
            text: format!("message {n}"),
            tone: Tone::Plain,
            item: None,
            change: None,
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

/// Opens the sample's first session, whose turn changed three files.
pub fn with_sample_session(mut app: App) -> App {
    app.open_session(sample::START_SESSION);
    app
}

pub fn kinds(app: &App) -> Vec<LayerKind> {
    app.layers.iter().map(|layer| layer.kind).collect()
}
