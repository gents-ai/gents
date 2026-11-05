use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use super::theme;
use crate::app::layers::LayerKind;
use crate::app::{App, Mode};
use crate::keys::KeySet;
use crate::sample::Tone;

const RIGHT: &str = "sample data ";

fn layer_hints(kind: LayerKind, set: KeySet) -> &'static str {
    match (kind, set) {
        (LayerKind::Jump, _) => "type to filter tabs, sessions, agents, menus and commands",
        (LayerKind::Picker, _) => "each tab keeps its own agent, side by side",
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
    if app.slash.is_some() {
        return match app.keymap.set() {
            KeySet::Vim => "/ at the start of a message opens commands · esc closes the list",
            KeySet::Emacs => "tab complete · enter run · ctrl-g close",
        };
    }
    match (app.keymap.set(), app.mode) {
        (KeySet::Vim, Mode::Input) => "esc normal mode · / commands · enter send",
        (KeySet::Vim, Mode::Normal) => "ctrl-w h/j/k/l move · : jump · ? keys",
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

    let right_x = area.right().saturating_sub(RIGHT.len() as u16);
    let hints_x = area.x + chip.len() as u16 + 1;
    let room = right_x.saturating_sub(hints_x + 1) as usize;
    let text = app
        .keymap
        .pending()
        .or(app.notice.as_deref())
        .unwrap_or_else(|| whole_hints(hints(app), room));
    buf.set_stringn(hints_x, area.y, text, room, theme::tone(Tone::Label));

    buf.set_string(right_x, area.y, RIGHT, theme::tone(Tone::Accent));
}
