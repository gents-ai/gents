use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};

use super::{draw_rows, pane_box, theme};
use crate::app::{App, Pane};
use crate::layout::sidebar_rows_area;
use crate::sample::{View, RAIL};

const HEADER: [(View, &str); 3] = [
    (View::Changes, "Changes"),
    (View::Workflows, "Workflows"),
    (View::Agents, "Agents"),
];

pub fn draw(app: &App, area: Rect, focused: bool, buf: &mut Buffer) {
    pane_box(buf, area, "", focused);
    let inner = area.inner(Margin::new(1, 1));

    let chips: u16 = HEADER.iter().map(|(_, name)| name.len() as u16 + 2).sum();
    let gap = u16::from(inner.width > chips + 2);
    let mut x = area.x + 2;
    for (view, name) in HEADER {
        let style = if view == app.view {
            theme::SELECTED
        } else {
            theme::TAB_OFF
        };
        let room = inner.right().saturating_sub(x) as usize;
        let (end, _) = buf.set_stringn(x, inner.y, format!(" {name} "), room, style);
        x = end + gap;
    }
    let rule = "─".repeat(inner.width as usize);
    buf.set_string(inner.x, inner.y + 1, rule, theme::edge(false));

    let rows = sidebar_rows_area(area);
    if let Some(file) = app.diff {
        draw_diff(app, app.visible_rows(Pane::Sidebar, rows), file, buf);
    } else {
        let selected = app.view_sel[app.view as usize];
        draw_rows(buf, rows, &app.view_rows(app.view), Some(selected));
    }
}

/// The view whose header chip covers column `x`, following the positions `draw` uses.
pub fn view_at(area: Rect, x: u16) -> Option<View> {
    let inner = area.inner(Margin::new(1, 1));
    let chips: u16 = HEADER.iter().map(|(_, name)| name.len() as u16 + 2).sum();
    let gap = u16::from(inner.width > chips + 2);
    let mut left = area.x + 2;
    for (view, name) in HEADER {
        let right = left + name.len() as u16 + 2;
        if (left..right).contains(&x) {
            return Some(view);
        }
        left = right + gap;
    }
    None
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

pub fn draw_rail(area: Rect, buf: &mut Buffer) {
    pane_box(buf, area, "", false);
    draw_rows(buf, area.inner(Margin::new(1, 1)), RAIL, None);
}
