use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Clear, Widget};

use super::layers::list_start;
use super::{draw_footer, draw_no_matches, draw_row, logo, pane_box, theme};
use crate::app::{App, Mode, Pane, Tab};
use crate::commands::COMMANDS;
use crate::keys::KeySet;
use crate::layout::conversation_area;
use crate::sample::Tone;

const WORD: &str = "g e n t s";
const HINT_SESSIONS_DOCKED: &str = "Pick a session on the left, or just start typing";
const HINT: &str = "Just start typing";
const HINT_NORMAL_SESSIONS_DOCKED: &str = "Pick a session on the left, or press i to start typing";
const HINT_NORMAL: &str = "Press i to start typing";
const TEXT_X: u16 = 4;
const SLASH_H: u16 = 13;
const SLASH_FOOTER: &str = " tab complete · enter run · esc close";

pub fn draw(app: &App, area: Rect, sessions_docked: bool, buf: &mut Buffer) {
    let focused = app.focus == Pane::Agent;
    let (x, y, w) = (area.x, area.y, area.width);
    pane_box(buf, area, "", focused);
    tab_chips(app, Rect::new(x + 1, y + 1, w - 2, 1), buf);
    let rule = "─".repeat((w - 2) as usize);
    buf.set_string(x + 1, y + 2, rule, theme::edge(false));

    let tab = &app.tabs[app.active];
    if tab.lines.is_empty() {
        let typing = app.keymap.set() == KeySet::Emacs || app.mode == Mode::Input;
        startup(area, sessions_docked, typing, buf);
    } else {
        let rows = app.visible_rows(Pane::Agent, conversation_area(area));
        conversation(tab, rows, buf);
    }
    message_box(tab, message_box_area(area), focused, buf);
    if let Some(selected) = app.slash {
        slash_list(app, selected, area, buf);
    }
}

fn message_box_area(pane: Rect) -> Rect {
    Rect::new(pane.x + 2, pane.y + pane.height - 4, pane.width - 4, 3)
}

fn text_room(message_box: Rect) -> u16 {
    message_box.width.saturating_sub(TEXT_X + 2)
}

pub fn cursor(tab: &Tab, pane: Rect) -> Position {
    let area = message_box_area(pane);
    let (_, column) = tab.input.view(text_room(area));
    Position::new(area.x + TEXT_X + column, area.y + 1)
}

fn tab_chips(app: &App, area: Rect, buf: &mut Buffer) {
    let chips: Vec<String> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| format!(" {} {} · {} ", index + 1, tab.name, tab.agent))
        .collect();
    let widths: Vec<usize> = chips
        .iter()
        .map(|chip| Span::raw(chip.as_str()).width())
        .collect();
    let mut first = 0;
    while first < app.active && chips_width(&widths[first..=app.active]) > area.width as usize {
        first += 1;
    }

    let mut x = area.x;
    for (index, chip) in chips.iter().enumerate().skip(first) {
        let style = if index == app.active {
            theme::SELECTED
        } else {
            theme::TAB_OFF
        };
        let room = area.right().saturating_sub(x) as usize;
        x = buf.set_stringn(x, area.y, chip, room, style).0 + 1;
    }
    let room = area.right().saturating_sub(x) as usize;
    buf.set_stringn(x, area.y, " + ", room, theme::TAB_OFF);
}

fn chips_width(widths: &[usize]) -> usize {
    widths.iter().sum::<usize>() + widths.len() - 1
}

fn startup(pane: Rect, sessions_docked: bool, typing: bool, buf: &mut Buffer) {
    let (x, y, w, h) = (pane.x, pane.y, pane.width, pane.height);
    logo::draw(buf, Rect::new(x + 1, y + 3, w - 2, h - 13));
    let hint = match (sessions_docked, typing) {
        (true, true) => HINT_SESSIONS_DOCKED,
        (false, true) => HINT,
        (true, false) => HINT_NORMAL_SESSIONS_DOCKED,
        (false, false) => HINT_NORMAL,
    };
    centered(buf, pane, y + h - 10, WORD, Tone::Accent);
    centered(buf, pane, y + h - 8, hint, Tone::Label);
}

fn centered(buf: &mut Buffer, pane: Rect, y: u16, text: &str, tone: Tone) {
    let x = pane.x + pane.width.saturating_sub(Span::raw(text).width() as u16) / 2;
    buf.set_string(x, y, text, theme::tone(tone));
}

pub fn first_line(total: usize, height: u16, scroll: usize) -> usize {
    total.saturating_sub(usize::from(height) + scroll)
}

fn conversation(tab: &Tab, area: Rect, buf: &mut Buffer) {
    let rows = tab.wrapped_rows(area.width);
    let first = first_line(rows.len(), area.height, tab.scroll);
    for ((text, tone), y) in rows[first..].iter().zip(area.top()..area.bottom()) {
        let style = theme::tone(*tone);
        buf.set_stringn(area.x, y, text, usize::from(area.width), style);
    }
}

fn message_box(tab: &Tab, area: Rect, focused: bool, buf: &mut Buffer) {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::edge(focused))
        .render(area, buf);
    let y = area.y + 1;
    let label = theme::tone(Tone::Label);
    buf.set_string(area.x + 2, y, "> ", theme::tone(Tone::Accent));
    let room = text_room(area);
    let text_x = area.x + TEXT_X;
    if !tab.input.is_empty() {
        let (text, _) = tab.input.view(room);
        buf.set_stringn(text_x, y, text, room as usize, theme::tone(Tone::Plain));
        return;
    }
    let placeholder = format!("Message {}", tab.agent);
    buf.set_stringn(text_x, y, placeholder, room as usize, label);
    if area.width > 40 {
        buf.set_string(area.right() - 12, y, "enter send", label);
    }
}

fn slash_list(app: &App, selected: usize, pane: Rect, buf: &mut Buffer) {
    let message = message_box_area(pane);
    let top = message.y.saturating_sub(SLASH_H).max(pane.y + 3);
    let rect = Rect::new(message.x, top, message.width, message.y - top);
    Clear.render(rect, buf);
    buf.set_style(rect, Style::new().fg(theme::FG).bg(theme::BG));
    pane_box(buf, rect, "Commands", false);

    let area = rect.inner(Margin::new(1, 1));
    draw_footer(app, area, area.bottom() - 2, SLASH_FOOTER, buf);
    let list = Rect {
        height: area.height - 2,
        ..area
    };
    let matches = app.slash_matches();
    if matches.is_empty() {
        draw_no_matches(list, buf);
        return;
    }
    let first = list_start(selected, usize::from(list.height));
    for (row, y) in (first..matches.len()).zip(list.top()..list.bottom()) {
        let command = &COMMANDS[matches[row]];
        let name = command.slash.unwrap_or_default();
        let text = format!(" /{name:<10}{}", command.about);
        draw_row(buf, list, y, &text, Tone::Plain, row == selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{sample_chat, ChatLine, Tab};
    use crate::keys::KeySet::{Emacs, Vim};
    use crate::sample::Tone;
    use crate::testing::{buffer, find, press, screen};

    fn no_logo(app: &App) -> bool {
        buffer(app).content().iter().all(|c| c.bg != theme::LOGO)
    }

    fn tab_of(texts: &[&str]) -> Tab {
        let mut tab = Tab::new_session();
        tab.lines = texts
            .iter()
            .map(|text| ChatLine {
                text: text.to_string(),
                tone: Tone::Plain,
            })
            .collect();
        tab
    }

    fn rows_at(tab: &Tab, width: u16) -> Vec<String> {
        tab.wrapped_rows(width)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    #[test]
    fn startup_pane() {
        let s = screen(&App::new(Vim, 132, 36));
        assert_eq!(find(&s, "1 new session · Engineer"), Some((1, 30)));
        assert_eq!(find(&s, "g e n t s"), Some((25, 58)));
        let hint = "Pick a session on the left, or just start typing";
        assert_eq!(find(&s, hint), Some((27, 39)));
        assert_eq!(find(&s, "Message Engineer"), Some((32, 34)));
        assert_eq!(find(&s, "enter send"), Some((32, 84)));
        assert!(find(&screen(&App::new(Vim, 70, 24)), "Just start typing").is_some());
    }

    #[test]
    fn the_startup_line_fits_the_mode() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc");
        let s = screen(&app);
        let docked = "Pick a session on the left, or press i to start typing";
        assert!(find(&s, docked).is_some());
        assert_eq!(find(&s, "just start typing"), None);
        press(&mut app, "i");
        assert!(find(&screen(&app), "just start typing").is_some());

        let mut app = App::new(Vim, 70, 24);
        press(&mut app, "esc");
        let s = screen(&app);
        assert!(find(&s, "Press i to start typing").is_some());
        assert_eq!(find(&s, "Just start typing"), None);

        let mut app = App::new(Vim, 90, 24);
        press(&mut app, "esc");
        let s = screen(&app);
        let (row, column) = find(&s, docked).expect("the line inside the narrowest pane");
        assert!(column > 26 && column + 54 < 83, "{column}");
        assert_eq!(row, 15);

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "esc");
        assert!(find(&screen(&app), "just start typing").is_some());
    }

    #[test]
    fn logo_sits_where_the_pictures_put_it() {
        let buf = buffer(&App::new(Vim, 132, 36));
        assert_eq!(buf[(60, 6)].bg, theme::LOGO);
        assert_ne!(buf[(59, 6)].bg, theme::LOGO);
        assert_eq!(buf[(42, 9)].bg, theme::LOGO);
        assert_ne!(buf[(41, 9)].bg, theme::LOGO);
    }

    #[test]
    fn no_logo_with_messages() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = sample_chat("Engineer");
        let s = screen(&app);
        assert_eq!(find(&s, "g e n t s"), None);
        assert_eq!(find(&s, "you"), Some((4, 30)));
        assert!(no_logo(&app));
        assert_eq!(find(&s, "Message Engineer"), Some((32, 34)));
    }

    #[test]
    fn chat_uses_the_agent_of_the_tab() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].agent = "Reviewer".to_string();
        app.tabs[0].lines = sample_chat("Reviewer");
        let s = screen(&app);
        assert_eq!(find(&s, "Reviewer"), Some((1, 46)));
        assert!(find(&s, "! Allow Reviewer to run").is_some());
        assert_eq!(find(&s, "Engineer"), None);
    }

    #[test]
    fn short_window_has_no_logo_and_no_overlap() {
        let app = App::new(Vim, 60, 16);
        let s = screen(&app);
        assert!(no_logo(&app));
        assert_eq!(find(&s, "g e n t s").map(|(row, _)| row), Some(5));
        assert_eq!(find(&s, "Just start typing").map(|(row, _)| row), Some(7));
        assert_eq!(find(&s, "Message Engineer").map(|(row, _)| row), Some(12));
    }

    #[test]
    fn active_tab_chip_stays_visible() {
        let mut app = App::new(Vim, 70, 24);
        app.tabs = (0..12).map(|_| Tab::new_session()).collect();
        app.active = 11;
        assert!(find(&screen(&app), "12 new session").is_some());
        app.active = 5;
        assert!(find(&screen(&app), "6 new session").is_some());
        app.active = 0;
        assert_eq!(find(&screen(&app), "1 new session"), Some((1, 2)));
    }

    #[test]
    fn chips_stay_inside_the_pane() {
        let mut app = App::new(Vim, 70, 24);
        app.tabs = (0..12).map(|_| Tab::new_session()).collect();
        app.active = 11;
        assert_eq!(buffer(&app)[(69, 1)].symbol(), "│");
    }

    #[test]
    fn chips_use_their_colours_then_the_new_tab_chip() {
        let mut app = App::new(Vim, 132, 36);
        let buf = buffer(&app);
        assert_eq!(buf[(30, 1)].bg, theme::SEL_BG);
        assert_eq!(buf[(30, 1)].fg, theme::SEL_FG);
        assert_eq!(buf[(57, 1)].symbol(), "+");
        assert_eq!(buf[(57, 1)].bg, theme::TAB_BG);

        app.tabs.push(Tab::new_session());
        let buf = buffer(&app);
        assert_eq!(buf[(57, 1)].symbol(), "2");
        assert_eq!(buf[(57, 1)].bg, theme::TAB_BG);
        assert_eq!(buf[(57, 1)].fg, theme::TITLE);
    }

    #[test]
    fn message_box_follows_pane_focus() {
        let mut app = App::new(Vim, 132, 36);
        let buf = buffer(&app);
        assert_eq!(buf[(30, 31)].symbol(), "╭");
        assert_eq!(buf[(95, 33)].symbol(), "╯");
        assert_eq!(buf[(30, 31)].fg, theme::FOCUS);
        assert_eq!(buf[(28, 0)].fg, theme::FOCUS);

        app.focus = crate::app::Pane::Sessions;
        let buf = buffer(&app);
        assert_eq!(buf[(30, 31)].fg, theme::EDGE);
        assert_eq!(buf[(28, 0)].fg, theme::EDGE);
    }

    #[test]
    fn lines_wrap_by_display_width() {
        let tab = tab_of(&["abcdefgh", "", "日本語"]);
        let rows = rows_at(&tab, 5);
        assert_eq!(rows, ["abcde", "fgh", "", "日本", "語"]);
    }

    #[test]
    fn a_character_wider_than_the_width_gets_its_own_row() {
        let tab = tab_of(&["日本"]);
        let rows = rows_at(&tab, 1);
        assert_eq!(rows, ["日", "本"]);
    }

    #[test]
    fn conversation_fills_upward_from_the_bottom_when_it_overflows() {
        let mut app = App::new(Vim, 60, 16);
        let texts: Vec<String> = (0..20).map(|n| format!("line {n}")).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        app.tabs[0] = tab_of(&texts);
        let s = screen(&app);
        assert_eq!(find(&s, "line 19"), Some((10, 2)));
        assert_eq!(find(&s, "line 13"), Some((4, 2)));
        assert_eq!(find(&s, "line 12"), None);

        app.tabs[0].scroll = 2;
        let s = screen(&app);
        assert_eq!(find(&s, "line 17"), Some((10, 2)));
        assert_eq!(find(&s, "line 18"), None);

        app.tabs[0].scroll = 500;
        assert_eq!(find(&screen(&app), "line 0"), Some((4, 2)));
    }

    #[test]
    fn long_lines_stay_inside_the_pane() {
        let mut app = App::new(Vim, 70, 24);
        app.tabs[0] = tab_of(&[&"x".repeat(200)]);
        let buf = buffer(&app);
        assert_eq!(buf[(69, 5)].symbol(), "│");
        assert_eq!(buf[(68, 5)].symbol(), " ");
        assert_eq!(buf[(67, 5)].symbol(), "x");
        assert_eq!(buf[(2, 6)].symbol(), "x");
    }

    #[test]
    fn a_variation_selector_wraps_with_its_character() {
        let tab = tab_of(&["a✔\u{fe0f}"]);
        let rows = rows_at(&tab, 2);
        assert_eq!(rows, ["a", "✔\u{fe0f}"]);
    }

    #[test]
    fn a_joined_emoji_is_never_split() {
        let tab = tab_of(&["👩\u{200d}💻"]);
        let rows = rows_at(&tab, 3);
        assert_eq!(rows, ["👩\u{200d}💻"]);
    }

    #[test]
    fn wrapped_emoji_stays_inside_the_pane() {
        let mut app = App::new(Vim, 70, 24);
        app.tabs[0] = tab_of(&[&"✔\u{fe0f}".repeat(60)]);
        let buf = buffer(&app);
        for y in 4..19 {
            assert_eq!(buf[(69, y)].symbol(), "│", "row {y}");
            assert_eq!(buf[(68, y)].symbol(), " ", "row {y}");
        }
        let drawn = buf.content().iter().filter(|c| c.symbol().starts_with('✔'));
        assert_eq!(drawn.count(), 60);
    }

    #[test]
    fn centering_counts_columns_not_bytes() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        centered(&mut buf, Rect::new(0, 0, 10, 1), 0, "日本", Tone::Plain);
        assert_eq!(buf[(3, 0)].symbol(), "日");
    }
}
