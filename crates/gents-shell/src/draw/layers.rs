use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};

use super::{
    draw_footer, draw_no_matches, draw_row, draw_rows, form, pane_box, sessions, setup, sidebar,
    theme,
};
use crate::app::input::LineInput;
use crate::app::layers::{Layer, LayerKind};
use crate::app::App;
use crate::jump;
use crate::keys::KeySet;
use crate::layout::{keys_rows_area, layer_rect, KEYS_FULL_W};
use crate::sample::{note, MenuId, SidebarRow, Tone, KEY_ROWS, PICKER_FOOTER};

const KEYS_FOOTNOTE: &str = "Sample bindings to react to. Switch sets from the jump bar.";
const KEYS_FOOTNOTE_VIM: &str = "Sample bindings to react to. Switch sets from the jump bar. \
    While writing, ctrl-w deletes a word.";
const PROMPT_W: u16 = 3;
const JUMP_FOOTER: &str = " enter jump · ctrl-n/ctrl-p move · esc back";

pub fn list_start(selected: usize, height: usize) -> usize {
    selected.saturating_sub(height.saturating_sub(1))
}

/// The rows of a menu, the picker or the jump bar: above the footer, and under the picker's and
/// the jump bar's prompt.
pub fn list_area(kind: LayerKind, rect: Rect) -> Rect {
    let area = inner(rect);
    let (top, bottom) = match kind {
        LayerKind::Picker => (2, 3),
        LayerKind::Jump => (2, 2),
        _ => (0, 3),
    };
    Rect {
        y: area.y + top,
        height: area.height - top - bottom,
        ..area
    }
}

pub fn draw(app: &App, area: Rect, buf: &mut Buffer) {
    for (depth, layer) in app.layers.iter().enumerate() {
        let focused = depth + 1 == app.layers.len();
        let rect = layer_rect(layer.kind, area, depth as u16, focused);
        Clear.render(rect, buf);
        buf.set_style(rect, Style::new().fg(theme::FG).bg(theme::BG));
        match layer.kind {
            LayerKind::Jump => draw_jump(app, layer, rect, focused, buf),
            LayerKind::Picker => draw_picker(app, layer, rect, focused, buf),
            LayerKind::Menu(id) => draw_menu(app, layer, id, rect, focused, buf),
            LayerKind::Keys => draw_keys(app, layer, rect, focused, buf),
            LayerKind::Sessions => sessions::draw(app, rect, focused, buf),
            LayerKind::Sidebar => sidebar::draw(app, rect, focused, buf),
            LayerKind::Setup => setup::draw(app, rect, focused, buf),
            LayerKind::Form => form::draw(app, rect, focused, buf),
        }
    }
}

fn inner(rect: Rect) -> Rect {
    rect.inner(Margin::new(1, 1))
}

fn draw_menu(app: &App, layer: &Layer, id: MenuId, rect: Rect, focused: bool, buf: &mut Buffer) {
    pane_box(buf, rect, app.menu_title(id), focused);
    let (rows, footer) = app.menu_rows(id);
    let area = inner(rect);
    let list = list_area(layer.kind, rect);
    let first = list_start(layer.selected, usize::from(list.height));
    draw_rows(buf, list, &rows[first..], Some(layer.selected - first));
    draw_footer(app, area, area.bottom() - 3, footer, buf);
}

fn draw_prompt(area: Rect, prompt: &str, filter: &LineInput, tone: Tone, buf: &mut Buffer) {
    buf.set_string(area.x, area.y, prompt, theme::tone(Tone::Accent));
    let room = area.width - PROMPT_W;
    let (query, _) = filter.view(room);
    buf.set_stringn(
        area.x + PROMPT_W,
        area.y,
        query,
        usize::from(room),
        theme::tone(tone),
    );
}

pub fn cursor(filter: &LineInput, rect: Rect) -> Position {
    let area = inner(rect);
    let (_, column) = filter.view(area.width - PROMPT_W);
    Position::new(area.x + PROMPT_W + column, area.y)
}

fn draw_picker(app: &App, layer: &Layer, rect: Rect, focused: bool, buf: &mut Buffer) {
    pane_box(buf, rect, "New tab: pick an agent", focused);
    let area = inner(rect);
    draw_prompt(area, " > ", &layer.filter, Tone::Plain, buf);

    let list = list_area(layer.kind, rect);
    let rows: Vec<SidebarRow> = app
        .picker_matches(layer.filter.text())
        .into_iter()
        .map(|agent| SidebarRow {
            text: agent.text.clone().into(),
            ..note("", agent.tone)
        })
        .collect();
    if rows.is_empty() {
        draw_no_matches(list, buf);
    } else {
        let first = list_start(layer.selected, usize::from(list.height));
        draw_rows(buf, list, &rows[first..], Some(layer.selected - first));
    }
    draw_footer(app, area, area.bottom() - 3, PICKER_FOOTER, buf);
}

/// Only the bar on top has a footer: the box beneath a menu is too narrow for it.
fn draw_jump(app: &App, layer: &Layer, rect: Rect, focused: bool, buf: &mut Buffer) {
    pane_box(buf, rect, "Jump to", focused);
    let area = inner(rect);
    draw_prompt(area, " : ", &layer.filter, Tone::Accent, buf);
    if focused {
        draw_footer(app, area, area.bottom() - 2, JUMP_FOOTER, buf);
    }

    let list = list_area(layer.kind, rect);
    let items = jump::items(app, layer.filter.text());
    if items.is_empty() {
        draw_no_matches(list, buf);
        return;
    }
    let (mut rows, mut selected) = (Vec::new(), 0);
    for (index, item) in items.iter().enumerate() {
        if index == 0 || items[index - 1].group != item.group {
            rows.push((format!(" {}", item.group.header()), Tone::Label));
        }
        if index == layer.selected {
            selected = rows.len();
        }
        rows.push((format!(" {:<38}{}", item.label, item.meta), Tone::Plain));
    }
    let first = list_start(selected, usize::from(list.height));
    for (index, y) in (first..rows.len()).zip(list.top()..list.bottom()) {
        let (text, tone) = &rows[index];
        draw_row(buf, list, y, text, *tone, index == selected);
    }
}

fn key_row(both: bool, set: KeySet, (name, vim, emacs): (&str, &str, &str)) -> String {
    if both {
        return format!("{name:<34}{vim:<22}{emacs}");
    }
    let active = match set {
        KeySet::Emacs if !emacs.is_empty() => emacs,
        _ => vim,
    };
    format!("{name:<28}{active}")
}

fn draw_keys(app: &App, layer: &Layer, rect: Rect, focused: bool, buf: &mut Buffer) {
    pane_box(buf, rect, "Keys", focused);
    let rows = keys_rows_area(rect);
    let (both, set) = (rect.width >= KEYS_FULL_W, app.keymap.set());
    let width = usize::from(rows.width);

    let header = key_row(both, set, ("", "vim", "emacs"));
    buf.set_stringn(rows.x, rows.y - 2, header, width, theme::tone(Tone::Label));

    let first = layer
        .selected
        .min(KEY_ROWS.len().saturating_sub(usize::from(rows.height)));
    for (&row, y) in KEY_ROWS[first..].iter().zip(rows.top()..rows.bottom()) {
        let text = key_row(both, set, row);
        buf.set_stringn(rows.x, y, text, width, theme::tone(Tone::Plain));
    }

    let footnote = Rect::new(rows.x, rows.bottom(), rows.width, 2);
    let text = match set {
        KeySet::Vim => KEYS_FOOTNOTE_VIM,
        KeySet::Emacs => KEYS_FOOTNOTE,
    };
    Paragraph::new(text)
        .style(theme::tone(Tone::Label))
        .wrap(Wrap { trim: true })
        .render(footnote, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeySet::Vim;
    use crate::sample::MenuId;
    use crate::testing::screen;

    #[test]
    fn list_start_keeps_the_selection_in_view() {
        assert_eq!(list_start(0, 5), 0);
        assert_eq!(list_start(4, 5), 0);
        assert_eq!(list_start(5, 5), 1);
        assert_eq!(list_start(9, 5), 5);
        assert_eq!(list_start(3, 0), 3);
    }

    #[test]
    fn a_deep_stack_draws_at_the_smallest_window() {
        let mut app = App::new(Vim, 60, 16);
        for _ in 0..40 {
            app.push_layer(LayerKind::Jump);
            app.push_layer(LayerKind::Menu(MenuId::Accounts));
        }
        app.push_layer(LayerKind::Keys);
        assert_eq!(app.layers.len(), 42);
        assert!(screen(&app).last().unwrap().contains("sample data"));
    }
}
