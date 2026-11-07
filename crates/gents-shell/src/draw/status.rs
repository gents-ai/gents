use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use super::theme;
use crate::app::layers::LayerKind;
use crate::app::{App, Mode};
use crate::keys::KeySet;
use crate::sample::Tone;
use crate::source::Connection;

fn right(connection: &Connection) -> &'static str {
    match connection {
        Connection::Sample => "sample data ",
        Connection::Connecting => "connecting ",
        Connection::Ready => "runtime ready ",
        Connection::Lost(_) => "runtime lost ",
    }
}

fn layer_hints(kind: LayerKind, set: KeySet) -> &'static str {
    match (kind, set) {
        (LayerKind::Jump, _) => "type to filter tabs, sessions, agents, menus and commands",
        (LayerKind::Picker, _) => "each tab keeps its own agent, side by side",
        (LayerKind::Setup, _) => "a provider, its connection and a model make one backend",
        (LayerKind::Form, _) => "shift-tab back a field · enter on an action runs it",
        (LayerKind::Menu(_), KeySet::Vim) => "esc back one layer · ctrl-w h/j/k/l cycles layers",
        (LayerKind::Menu(_), KeySet::Emacs) => "ctrl-g back one layer · ctrl-x o cycles layers",
        (LayerKind::Keys, KeySet::Vim) => "j/k scroll · esc close",
        (LayerKind::Keys, KeySet::Emacs) => "ctrl-n/ctrl-p scroll · ctrl-g close",
        (LayerKind::Sessions | LayerKind::Sidebar, KeySet::Vim) => {
            "esc close · j/k move · enter open"
        }
        (LayerKind::Sessions | LayerKind::Sidebar, KeySet::Emacs) => {
            "ctrl-g close · ctrl-n/ctrl-p move · enter open"
        }
    }
}

fn hints(app: &App) -> &'static str {
    if let Some(top) = app.layers.last() {
        return layer_hints(top.kind, app.keymap.set());
    }
    if let Some(search) = &app.search {
        return match (app.keymap.set(), search.committed) {
            (KeySet::Vim, false) => "enter keep · esc cancel",
            (KeySet::Vim, true) => "n next · N previous · esc clear",
            (KeySet::Emacs, _) => "ctrl-s next · ctrl-r previous · enter end · ctrl-g cancel",
        };
    }
    if app.in_running() {
        return match app.keymap.set() {
            KeySet::Vim => "enter open · j/k move · esc back",
            KeySet::Emacs => "enter open · ctrl-n/ctrl-p move · ctrl-g back",
        };
    }
    let stepped_in = app.tabs[app.active].parent.is_some();
    if stepped_in && (app.mode == Mode::Normal || app.keymap.set() == KeySet::Emacs) {
        return match app.keymap.set() {
            KeySet::Vim => "esc step out · : jump · ? keys",
            KeySet::Emacs => "ctrl-g step out · ctrl-x x jump · ctrl-x ? keys",
        };
    }
    if app.slash.is_some() {
        return match app.keymap.set() {
            KeySet::Vim => "/ at the start of a message opens commands · esc closes the list",
            KeySet::Emacs => "tab complete · enter run · ctrl-g close",
        };
    }
    match (app.keymap.set(), app.mode) {
        (KeySet::Vim, Mode::Input) if stepped_in => "esc normal mode · / commands",
        (KeySet::Vim, Mode::Input) => "esc normal mode · / commands · enter send",
        (KeySet::Vim, Mode::Normal) => "h/l move · [ ] sidebars · : jump · ? keys",
        (KeySet::Emacs, _) => "ctrl-x o next pane · ctrl-x x jump · ctrl-x ? keys",
    }
}

/// Drops whole hints from the end, because a key cut in half reads as another key.
fn whole_hints(mut text: &str, room: usize) -> &str {
    while Span::raw(text).width() > room {
        let Some((kept, _)) = text.rsplit_once(" · ") else {
            break;
        };
        text = kept;
    }
    text
}

pub fn draw(app: &App, area: Rect, buf: &mut Buffer) {
    let chip = format!(" {} ", app.chip());
    let chip_style = Style::new().fg(theme::CHIP_FG).bg(theme::ACCENT);
    buf.set_string(area.x, area.y, &chip, chip_style);

    let right = right(&app.connection);
    let right_x = area.right().saturating_sub(right.len() as u16);
    let hints_x = area.x + chip.len() as u16 + 1;
    let room = right_x.saturating_sub(hints_x + 1) as usize;
    let lost = match &app.connection {
        Connection::Lost(message) => Some(message.as_str()),
        _ => None,
    };
    let message = app.keymap.pending().or(app.notice.as_deref()).or(lost);
    let wide = message.is_some_and(|message| Span::raw(message).width() > room);
    let room = if wide {
        area.right().saturating_sub(hints_x) as usize
    } else {
        room
    };
    let text = message.unwrap_or_else(|| whole_hints(hints(app), room));
    buf.set_stringn(hints_x, area.y, text, room, theme::tone(Tone::Label));

    if !wide {
        buf.set_string(right_x, area.y, right, theme::tone(Tone::Accent));
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::keys::KeySet::Vim;
    use crate::source::{Connection, Update};
    use crate::testing::{find, screen};

    #[test]
    fn the_status_bar_shows_the_connection() {
        for (width, height) in [(132, 36), (60, 16)] {
            let last = height - 1;
            let at = |text: &str| Some((last, width - text.len() as u16 - 1));
            assert_eq!(
                find(&screen(&App::new(Vim, width, height)), "sample data"),
                at("sample data")
            );

            let (intents, _receiver) = std::sync::mpsc::channel();
            let mut app = App::connected(Vim, width, height, intents);
            assert_eq!(find(&screen(&app), "connecting"), at("connecting"));
            app.apply(Update::Connection(Connection::Ready));
            let s = screen(&app);
            assert_eq!(find(&s, "runtime ready"), at("runtime ready"));
            assert!(s[usize::from(last)].contains("esc normal mode"), "{width}");

            let lost = "check gents status";
            app.apply(Update::Connection(Connection::Lost(lost.to_string())));
            let s = screen(&app);
            assert_eq!(find(&s, "runtime lost"), at("runtime lost"));
            assert_eq!(find(&s, lost).map(|(row, _)| row), Some(last), "{width}");
            assert!(!s[usize::from(last)].contains("esc normal mode"), "{width}");

            app.apply(Update::Notice("a notice".to_string()));
            assert!(find(&screen(&app), "a notice").is_some(), "{width}");
        }
    }
}
