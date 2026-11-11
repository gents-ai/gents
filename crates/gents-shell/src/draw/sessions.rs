use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};

use super::{draw_rows, pane_box};
use crate::app::App;

pub fn draw(app: &App, area: Rect, focused: bool, buf: &mut Buffer) {
    pane_box(buf, area, "Sessions", focused);
    let rows = area.inner(Margin::new(1, 1));
    draw_rows(buf, rows, &app.session_rows(), Some(app.sessions_sel));
}
