use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};

use super::{draw_rows, pane_box, theme};
use crate::app::changes::rail_rows;
use crate::app::{App, Pane};
use crate::layout::sidebar_rows_area;
use crate::sample::{Scope, View};

const SCOPES: [(Scope, &str); 2] = [(Scope::Session, "session"), (Scope::Tree, "working tree")];

/// Each chip's left column, two in from the pane's edge, a gap apart when the row has room.
fn chip_lefts<T>(area: Rect, chips: &[(T, &'static str)]) -> Vec<(u16, &'static str)> {
    let inner = area.inner(Margin::new(1, 1));
    let width: u16 = chips.iter().map(|(_, name)| name.len() as u16 + 2).sum();
    let gap = u16::from(inner.width > width + 2);
    let mut left = area.x + 2;
    chips
        .iter()
        .map(|&(_, name)| {
            let at = left;
            left += name.len() as u16 + 2 + gap;
            (at, name)
        })
        .collect()
}

fn draw_chips<T>(buf: &mut Buffer, area: Rect, y: u16, chips: &[(T, &'static str)], shown: T)
where
    T: Copy + PartialEq,
{
    let right = area.inner(Margin::new(1, 1)).right();
    for (&(value, _), (x, name)) in chips.iter().zip(chip_lefts(area, chips)) {
        let style = if value == shown {
            theme::SELECTED
        } else {
            theme::TAB_OFF
        };
        let room = right.saturating_sub(x) as usize;
        buf.set_stringn(x, y, format!(" {name} "), room, style);
    }
}

/// The chip covering column `x`, following the positions `draw_chips` uses.
fn chip_at<T>(area: Rect, chips: &[(T, &'static str)], x: u16) -> Option<T>
where
    T: Copy,
{
    let lefts = chip_lefts(area, chips);
    let hit = lefts.iter().position(|&(left, name)| {
        let right = left + name.len() as u16 + 2;
        (left..right).contains(&x)
    });
    hit.map(|index| chips[index].0)
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
        draw_chips(buf, area, inner.y, &SCOPES, app.scope);
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

/// The scope whose chip on the Diff view's scope line covers column `x`.
pub fn scope_at(area: Rect, x: u16) -> Option<Scope> {
    chip_at(area, &SCOPES, x)
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
