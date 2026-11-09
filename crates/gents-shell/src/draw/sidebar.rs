use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};

use super::{draw_rows, pane_box, theme};
use crate::app::changes::rail_rows;
use crate::app::{App, Pane};
use crate::layout::sidebar_rows_area;
use crate::sample::{Scope, View};

const SCOPES: [(Scope, &str); 2] = [(Scope::Session, "session"), (Scope::Tree, "working tree")];

/// Each chip's left column, two in from the pane's edge, a gap apart when the row has room.
fn chip_lefts(area: Rect) -> Vec<(u16, &'static str)> {
    let inner = area.inner(Margin::new(1, 1));
    let width: u16 = SCOPES.iter().map(|(_, name)| name.len() as u16 + 2).sum();
    let gap = u16::from(inner.width > width + 2);
    let mut left = area.x + 2;
    SCOPES
        .iter()
        .map(|&(_, name)| {
            let at = left;
            left += name.len() as u16 + 2 + gap;
            (at, name)
        })
        .collect()
}

fn draw_chips(buf: &mut Buffer, area: Rect, y: u16, shown: Scope) {
    let right = area.inner(Margin::new(1, 1)).right();
    for (&(scope, _), (x, name)) in SCOPES.iter().zip(chip_lefts(area)) {
        let style = if scope == shown {
            theme::SELECTED
        } else {
            theme::TAB_OFF
        };
        let room = right.saturating_sub(x) as usize;
        buf.set_stringn(x, y, format!(" {name} "), room, style);
    }
}

pub fn draw(app: &App, area: Rect, focused: bool, buf: &mut Buffer) {
    let title = match app.view {
        View::Diff => "Diff",
        View::Workflows => "Workflows",
        View::Agents => "Background agents",
    };
    pane_box(buf, area, title, focused);
    if app.view == View::Diff {
        let inner = area.inner(Margin::new(1, 1));
        draw_chips(buf, area, inner.y, app.scope);
        let rule = "─".repeat(inner.width as usize);
        buf.set_string(inner.x, inner.y + 1, rule, theme::edge(false));
    }

    let rows = sidebar_rows_area(area, app.view);
    if let Some(file) = app.diff {
        draw_diff(app, app.visible_rows(Pane::Sidebar, rows), file, buf);
    } else {
        let selected = app.view_sel[app.view as usize];
        draw_rows(buf, rows, &app.view_rows(app.view), Some(selected));
    }
}

/// The scope whose chip on the Diff view's scope line covers column `x`, where `draw_chips`
/// draws it.
pub fn scope_at(area: Rect, x: u16) -> Option<Scope> {
    let lefts = chip_lefts(area);
    let hit = lefts.iter().position(|&(left, name)| {
        let right = left + name.len() as u16 + 2;
        (left..right).contains(&x)
    });
    hit.map(|index| SCOPES[index].0)
}

pub fn first_diff_row(scroll: usize, total: usize, height: u16) -> usize {
    scroll.min(total.saturating_sub(usize::from(height)))
}

fn draw_diff(app: &App, area: Rect, file: usize, buf: &mut Buffer) {
    let rows = app.diff_rows(file);
    let first = first_diff_row(app.diff_scroll, rows.len(), area.height);
    for ((text, tone), y) in rows[first..].iter().zip(area.top()..area.bottom()) {
        let style = theme::tone(*tone);
        buf.set_stringn(area.x, y, text, usize::from(area.width), style);
    }
}

pub fn draw_rail(app: &App, area: Rect, buf: &mut Buffer) {
    pane_box(buf, area, "", false);
    let rows = rail_rows(&app.tabs[app.active].files());
    draw_rows(buf, area.inner(Margin::new(1, 1)), &rows, None);
}
