pub mod agent;
pub mod layers;
pub mod logo;
pub mod sessions;
pub mod sidebar;
pub mod status;
pub mod theme;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};
use ratatui::Frame;

use crate::app::search::{matches, Match, Search};
use crate::app::{App, Pane};
use crate::keys::{in_key_set, KeyCtx};
use crate::layout::{layout, Right, Screen};
use crate::sample::{SidebarRow, Tone};

const TOO_SMALL: &str = "Window too small: need at least 60 by 16";
const SEARCH_PREFIX: &str = "search: ";
const NO_MATCH: &str = " (no match)";

pub fn draw(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    buf.set_style(area, Style::new().fg(theme::FG).bg(theme::BG));
    let Screen::Shell(panes) = layout(area.width, area.height, app.wanted) else {
        Paragraph::new(TOO_SMALL)
            .wrap(Wrap { trim: true })
            .render(area, buf);
        return;
    };
    if let Some(rect) = panes.sessions {
        sessions::draw(app, rect, app.focus == Pane::Sessions, buf);
    }
    agent::draw(app, panes.agent, panes.sessions.is_some(), buf);
    match panes.right {
        Right::None => {}
        Right::Rail(rect) => sidebar::draw_rail(rect, buf),
        Right::Sidebar(rect) => sidebar::draw(app, rect, app.focus == Pane::Sidebar, buf),
    }
    if let Some(search) = &app.search {
        draw_search(app, search, buf);
    }
    if !app.layers.is_empty() {
        buf.set_style(area, Style::new().fg(theme::DIM).bg(theme::BG));
        layers::draw(app, panes.above_status(), buf);
    }
    status::draw(app, panes.status, buf);
    match (app.ctx(), &app.search) {
        (KeyCtx::Input, _) => {
            frame.set_cursor_position(agent::cursor(&app.tabs[app.active], panes.agent));
        }
        (KeyCtx::SearchLine, Some(search)) => {
            if let Some(rows) = app.pane_rows(search.pane) {
                frame.set_cursor_position(search_cursor(app, search, rows));
            }
        }
        (KeyCtx::Filter, _) => {
            if let (Some(top), Some(rect)) = (app.layers.last(), app.top_layer_rect()) {
                frame.set_cursor_position(layers::cursor(&top.filter, rect));
            }
        }
        _ => {}
    }
}

fn search_miss(app: &App, search: &Search) -> &'static str {
    let lines = app.search_lines(search.pane);
    if !search.query.is_empty() && matches(&lines, search.query.text()).is_empty() {
        NO_MATCH
    } else {
        ""
    }
}

fn search_room(rows: Rect, miss: &str) -> u16 {
    rows.width
        .saturating_sub((SEARCH_PREFIX.len() + miss.len()) as u16)
}

fn search_cursor(app: &App, search: &Search, rows: Rect) -> Position {
    let room = search_room(rows, search_miss(app, search));
    let (_, column) = search.query.view(room);
    let x = rows.x + SEARCH_PREFIX.len() as u16 + column;
    Position::new(x, rows.bottom() - 1)
}

/// Over rows already drawn: the matches in view, then the line in the last row.
fn draw_search(app: &App, search: &Search, buf: &mut Buffer) {
    let Some(rows) = app.pane_rows(search.pane) else {
        return;
    };
    // `rows` follows `app.size`; the buffer can already follow a resize whose event is queued.
    if rows.bottom() > buf.area.bottom() {
        return;
    }
    let lines = app.search_lines(search.pane);
    let found = matches(&lines, search.query.text());
    let visible = app.visible_rows(search.pane, rows);
    let first = match (search.pane, app.diff) {
        (Pane::Agent, _) => {
            agent::first_line(lines.len(), visible.height, app.tabs[app.active].scroll)
        }
        (Pane::Sidebar, Some(_)) => {
            sidebar::first_diff_row(app.diff_scroll, lines.len(), visible.height)
        }
        _ => 0,
    };
    for (index, found) in found.iter().enumerate() {
        if found.row >= first {
            let y = visible.y.saturating_add((found.row - first) as u16);
            let cells = match_cells(&lines[found.row], found, visible.x, y);
            let style = Style::new().fg(theme::BG).bg(theme::MATCH);
            let style = if index == search.current {
                style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                style
            };
            buf.set_style(cells.intersection(visible), style);
        }
    }

    let miss = search_miss(app, search);
    let (query, _) = search.query.view(search_room(rows, miss));
    buf.set_stringn(
        rows.x,
        rows.bottom() - 1,
        format!("{SEARCH_PREFIX}{query}{miss}"),
        usize::from(rows.width),
        theme::tone(Tone::Accent),
    );
}

fn match_cells(line: &str, found: &Match, x: u16, y: u16) -> Rect {
    let before: String = line.chars().take(found.start).collect();
    let text: String = line
        .chars()
        .skip(found.start)
        .take(found.end - found.start)
        .collect();
    let width = |text: &str| Span::raw(text.to_string()).width() as u16;
    Rect::new(x.saturating_add(width(&before)), y, width(&text), 1)
}

fn pane_box(buf: &mut Buffer, area: Rect, title: &str, focused: bool) {
    Block::bordered()
        .border_style(theme::edge(focused))
        .render(area, buf);
    if !title.is_empty() {
        let style = if focused {
            Style::new().fg(theme::BG).bg(theme::FOCUS)
        } else {
            Style::new().fg(theme::TITLE)
        };
        buf.set_string(area.x + 2, area.y, format!(" {title} "), style);
    }
}

fn draw_row(buf: &mut Buffer, area: Rect, y: u16, text: &str, tone: Tone, selected: bool) {
    let style = if selected {
        let row = Rect {
            y,
            height: 1,
            ..area
        };
        buf.set_style(row, theme::SELECTED);
        theme::SELECTED
    } else {
        theme::tone(tone)
    };
    buf.set_stringn(area.x, y, text, area.width as usize, style);
}

fn draw_rows(buf: &mut Buffer, area: Rect, rows: &[SidebarRow], selected: Option<usize>) {
    for (index, (row, y)) in rows.iter().zip(area.top()..area.bottom()).enumerate() {
        draw_row(buf, area, y, row.text, row.tone, selected == Some(index));
    }
}

fn draw_footer(app: &App, area: Rect, y: u16, text: &str, buf: &mut Buffer) {
    let label = theme::tone(Tone::Label);
    let text = in_key_set(app.keymap.set(), text);
    buf.set_stringn(area.x, y, text, area.width as usize, label);
}

fn draw_no_matches(area: Rect, buf: &mut Buffer) {
    let label = theme::tone(Tone::Label);
    buf.set_stringn(area.x, area.y, " no matches", area.width as usize, label);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::layers::LayerKind;
    use crate::app::{sample_chat, Mode};
    use crate::keys::KeySet::{Emacs, Vim};
    use crate::sample::{note, MenuId, View};
    use crate::testing::{buffer, find, press, rows, screen, type_text};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::Event;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    #[test]
    fn panes_at_each_width() {
        let s = screen(&App::new(Vim, 168, 48));
        assert_eq!(find(&s, "Sessions"), Some((0, 3)));
        assert_eq!(find(&s, "Changes"), Some((1, 127)));
        assert_eq!(find(&s, "sample data"), Some((47, 156)));
        let s = screen(&App::new(Vim, 132, 36));
        assert_eq!(find(&s, "Changes"), Some((1, 101)));
        assert_eq!(find(&s, "M cli/args.rs"), Some((5, 100)));
        assert_eq!(find(&s, "cli ux redesign"), Some((4, 4)));
        assert_eq!(find(&s, "INPUT"), Some((35, 1)));
        assert_eq!(find(&s, "sample data"), Some((35, 120)));
        let s = screen(&App::new(Vim, 104, 30));
        assert_eq!(find(&s, "+58"), Some((3, 99)));
        assert_eq!(find(&s, "Workflows"), None);
        let s = screen(&App::new(Vim, 70, 24));
        assert_eq!(find(&s, "Sessions"), None);
        assert_eq!(find(&s, "Changes"), None);
        assert_eq!(find(&s, "sample data"), Some((23, 58)));
        let s = screen(&App::new(Vim, 40, 24));
        assert!(find(&s, "Window too small: need at least 60 by 16").is_some());
        assert_eq!(find(&s, "sample data"), None);
    }

    #[test]
    fn resize_redraws() {
        let mut app = App::new(Vim, 132, 36);
        let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
        terminal.draw(|frame| draw(&app, frame)).unwrap();
        assert!(find(&rows(terminal.backend().buffer()), "Sessions").is_some());

        terminal.backend_mut().resize(70, 24);
        app.handle(Event::Resize(70, 24));
        terminal.draw(|frame| draw(&app, frame)).unwrap();
        let s = rows(terminal.backend().buffer());
        assert_eq!(find(&s, "Sessions"), None);
        assert_eq!(find(&s, "sample data"), Some((23, 58)));
    }

    #[test]
    fn draws_at_one_by_one() {
        screen(&App::new(Vim, 1, 1));
        screen(&App::new(Vim, 10, 3));
    }

    #[test]
    fn rows_clip_at_the_pane_border() {
        // The real panes are wider than their sample rows, so the sessions half draws narrower.
        let mut buf = Buffer::empty(Rect::new(0, 0, 14, 20));
        let app = App::new(Vim, 90, 30);
        sessions::draw(&app, Rect::new(0, 0, 12, 20), false, &mut buf);
        for y in 1..19 {
            assert_eq!(buf[(11, y)].symbol(), "│", "sessions row {y}");
            assert_eq!(buf[(12, y)].symbol(), " ", "sessions row {y}");
        }
        for view in [View::Changes, View::Workflows, View::Agents] {
            let mut app = App::new(Vim, 120, 30);
            app.view = view;
            let buf = buffer(&app);
            for y in 1..28 {
                assert_eq!(buf[(119, y)].symbol(), "│", "{view:?} row {y}");
            }
        }
    }

    #[test]
    fn long_rows_stop_at_the_width() {
        let long = [note("0123456789", Tone::Plain)];
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));
        draw_rows(&mut buf, Rect::new(1, 1, 4, 1), &long, Some(0));
        assert_eq!(rows(&buf)[1], " 0123     ");
        assert_eq!(buf[(4, 1)].bg, theme::SEL_BG);
        assert_ne!(buf[(5, 1)].bg, theme::SEL_BG);
    }

    #[test]
    fn focused_title_is_highlighted() {
        let mut app = App::new(Vim, 132, 36);
        assert_eq!(app.focus, Pane::Agent);
        assert_ne!(buffer(&app)[(3, 0)].bg, theme::FOCUS);
        app.focus = Pane::Sessions;
        let buf = buffer(&app);
        assert_eq!(buf[(3, 0)].bg, theme::FOCUS);
        assert_eq!(buf[(3, 0)].fg, theme::BG);
        assert_eq!(buf[(0, 1)].fg, theme::FOCUS);
    }

    #[test]
    fn each_view_shows_its_rows() {
        let mut app = App::new(Vim, 132, 36);
        app.view = View::Workflows;
        assert!(find(&screen(&app), "2 running").is_some());
        app.view = View::Agents;
        let s = screen(&app);
        assert!(find(&s, "3 background").is_some());
        assert!(find(&s, "finished 2m ago").is_some());
    }

    #[test]
    fn selected_rows_fill_their_width() {
        let buf = buffer(&App::new(Vim, 132, 36));
        for x in 1..27 {
            assert_eq!(buf[(x, 4)].bg, theme::SEL_BG, "sessions col {x}");
        }
        assert_eq!(buf[(4, 4)].fg, theme::SEL_FG);
        assert_eq!(buf[(1, 5)].bg, theme::BG);
        for x in 99..131 {
            assert_eq!(buf[(x, 5)].bg, theme::SEL_BG, "changes col {x}");
        }
    }

    #[test]
    fn active_view_has_its_own_chip_colour() {
        let buf = buffer(&App::new(Vim, 132, 36));
        assert_eq!(buf[(101, 1)].bg, theme::SEL_BG);
        assert_eq!(buf[(111, 1)].bg, theme::TAB_BG);
    }

    #[test]
    fn status_row_follows_the_mode() {
        let mut app = App::new(Vim, 132, 36);
        let hints = "esc normal mode · / commands · enter send";
        assert_eq!(find(&screen(&app), hints), Some((35, 8)));

        app.mode = Mode::Normal;
        let s = screen(&app);
        assert_eq!(find(&s, "NORMAL"), Some((35, 1)));
        assert!(find(&s, "ctrl-w h/j/k/l move · : jump · ? keys").is_some());

        let s = screen(&App::new(Emacs, 132, 36));
        assert_eq!(find(&s, "EMACS"), Some((35, 1)));
        let hints = "ctrl-x o next pane · ctrl-x x jump · ctrl-x ? keys";
        assert!(find(&s, hints).is_some());
        assert!(!s[35].contains("alt-x"));

        app.notice = Some("not wired yet".to_string());
        let s = screen(&app);
        assert!(find(&s, "not wired yet").is_some());
        assert_eq!(find(&s, "ctrl-w h/j/k/l"), None);
    }

    #[test]
    fn hints_clip_before_sample_data() {
        let s = screen(&App::new(Emacs, 60, 16));
        assert_eq!(find(&s, "sample data"), Some((15, 48)));
        assert!(find(&s, "ctrl-x o next pane · ctrl-x x jump").is_some());
        assert!(!s[15].contains("ctrl-x ?"));
        assert_eq!(s[15].chars().nth(47), Some(' '));

        let s = screen(&App::new(Vim, 60, 16));
        assert!(find(&s, "esc normal mode · / commands").is_some());
        assert!(!s[15].contains("enter"));
    }

    #[test]
    fn the_emacs_hint_holds_whatever_is_focused() {
        for focus in [Pane::Sessions, Pane::Agent, Pane::Sidebar] {
            let mut app = App::new(Emacs, 132, 36);
            app.focus = focus;
            let s = screen(&app);
            assert!(s[35].contains("ctrl-x o next pane"), "{focus:?}");
            assert!(!s[35].contains("enter send"), "{focus:?}");
        }
    }

    #[test]
    fn a_pending_chord_shows_in_the_status_row() {
        for (set, open, prefix, shown) in [
            (Vim, "esc", "ctrl-w", "ctrl-w ..."),
            (Vim, "esc", "g", "g ..."),
            (Emacs, "", "ctrl-x", "ctrl-x ..."),
        ] {
            let mut app = App::new(set, 132, 36);
            press(&mut app, open);
            assert_eq!(find(&screen(&app), shown), None);
            press(&mut app, prefix);
            assert_eq!(find(&screen(&app), shown).map(|(row, _)| row), Some(35));
            press(&mut app, "z");
            assert_eq!(find(&screen(&app), shown), None);
        }

        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Menu(MenuId::Accounts));
        press(&mut app, "ctrl-w");
        assert_eq!(
            find(&screen(&app), "ctrl-w ...").map(|(row, _)| row),
            Some(35)
        );
    }

    #[test]
    fn the_search_line_survives_a_buffer_behind_the_app_size() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc /");
        type_text(&mut app, "x");
        let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
        terminal.draw(|frame| draw(&app, frame)).unwrap();
        assert!(find(&rows(terminal.backend().buffer()), "sample data").is_some());
    }

    #[test]
    fn the_whole_frame_is_painted() {
        for (width, height) in [(132, 36), (40, 24)] {
            let buf = buffer(&App::new(Vim, width, height));
            assert_eq!(buf[(width - 1, height - 2)].bg, theme::BG);
            assert_eq!(buf[(1, height - 3)].bg, theme::BG);
        }
    }

    #[test]
    fn status_chip_and_right_side_use_their_colours() {
        let buf = buffer(&App::new(Vim, 132, 36));
        assert_eq!(buf[(1, 35)].bg, theme::ACCENT);
        assert_eq!(buf[(1, 35)].fg, theme::CHIP_FG);
        assert_eq!(buf[(120, 35)].fg, theme::ACCENT);
    }

    #[test]
    fn the_sidebar_header_fits_at_every_docked_width() {
        for width in 120..=132 {
            let buf = buffer(&App::new(Vim, width, 30));
            let header = &rows(&buf)[1];
            for chip in [" Changes ", " Workflows ", " Agents "] {
                assert!(header.contains(chip), "{width}: {header}");
            }
            assert_eq!(buf[(width - 1, 1)].symbol(), "│");
        }
    }

    #[test]
    fn draws_at_every_size_that_fits() {
        let kinds = [
            LayerKind::Jump,
            LayerKind::Picker,
            LayerKind::Menu(MenuId::Accounts),
            LayerKind::Keys,
            LayerKind::Sessions,
            LayerKind::Sidebar,
        ];
        for width in [
            60, 61, 89, 90, 91, 104, 119, 120, 121, 124, 125, 132, 168, 300,
        ] {
            for height in [16, 17, 18, 19, 20, 24, 30, 36, 48, 100] {
                let mut app = App::new(Vim, width, height);
                let size = format!("{width}x{height}");
                assert!(find(&screen(&app), "sample data").is_some(), "{size}");

                app.tabs[0].lines = sample_chat("Engineer");
                app.diff = Some(0);
                for focus in [Pane::Sessions, Pane::Agent, Pane::Sidebar] {
                    app.focus = focus;
                    assert!(find(&screen(&app), "sample data").is_some(), "{size}");
                }
                for kind in kinds {
                    app.push_layer(kind);
                    assert!(find(&screen(&app), "sample data").is_some(), "{size}");
                }
            }
        }
    }
}
