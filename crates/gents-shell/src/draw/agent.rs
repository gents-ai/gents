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
use crate::layout::running_area;
use crate::sample::{Opens, RunState, Tone, SUBAGENT_MARK};

const WORD: &str = "g e n t s";
const HINT_SESSIONS_DOCKED: &str = "Pick a session on the left, or just start typing";
const HINT: &str = "Just start typing";
const HINT_NORMAL_SESSIONS_DOCKED: &str = "Pick a session on the left, or press i to start typing";
const HINT_NORMAL: &str = "Press i to start typing";
const TEXT_X: u16 = 4;
const SLASH_H: u16 = 13;
const SLASH_FOOTER: &str = " tab complete · enter run · esc close";
const RUNNING_STATUS_X: u16 = 32;
const RUNNING_HINT_MIN_W: u16 = 64;
const READ_ONLY: &str = "subagents take input from their parent";

pub fn draw(app: &App, area: Rect, sessions_docked: bool, buf: &mut Buffer) {
    let focused = app.focus == Pane::Agent;
    let (x, y, w) = (area.x, area.y, area.width);
    pane_box(buf, area, "", focused);
    tab_chips(app, Rect::new(x + 1, y + 1, w - 2, 1), buf);
    let rule = "─".repeat((w - 2) as usize);
    buf.set_string(x + 1, y + 2, rule, theme::edge(false));

    let tab = &app.tabs[app.active];
    if tab.lines.is_empty() && tab.running.is_empty() {
        let typing = app.keymap.set() == KeySet::Emacs || app.mode == Mode::Input;
        startup(area, sessions_docked, typing, buf);
    } else {
        let rows = app.visible_rows(Pane::Agent, app.conversation_rect(area));
        conversation(tab, rows, buf);
    }
    running_rows(app, tab, area, buf);
    message_box(tab, message_box_area(area), focused, buf);
    if let Some(selected) = app.slash {
        slash_list(app, selected, area, buf);
    }
}

pub fn message_box_area(pane: Rect) -> Rect {
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

/// Each tab chip's label and width, and the first chip that keeps the active one in `room`.
fn chips_in(app: &App, room: u16) -> (Vec<String>, Vec<usize>, usize) {
    let chips: Vec<String> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| match &tab.parent {
            Some(parent) => format!(
                " {} {} · {} › {} ",
                index + 1,
                parent.name,
                parent.agent,
                tab.agent
            ),
            None => format!(" {} {} · {} ", index + 1, tab.name, tab.agent),
        })
        .collect();
    let widths: Vec<usize> = chips
        .iter()
        .map(|chip| Span::raw(chip.as_str()).width())
        .collect();
    let mut first = 0;
    while first < app.active && chips_width(&widths[first..=app.active]) > room as usize {
        first += 1;
    }
    (chips, widths, first)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipHit {
    Tab(usize),
    New,
}

pub fn chip_at(app: &App, pane: Rect, x: u16) -> Option<ChipHit> {
    let (_, widths, first) = chips_in(app, pane.width - 2);
    let mut left = pane.x + 1;
    for (index, width) in widths.iter().enumerate().skip(first) {
        let right = left + *width as u16;
        if (left..right).contains(&x) {
            return Some(ChipHit::Tab(index));
        }
        left = right + 1;
    }
    (left..left + 3).contains(&x).then_some(ChipHit::New)
}

fn tab_chips(app: &App, area: Rect, buf: &mut Buffer) {
    let (chips, _, first) = chips_in(app, area.width);
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
        draw_row(buf, area, y, text, *tone, *tone == Tone::Reply);
        if *tone == Tone::You {
            let left = (text.len() - text.trim_start().len()).saturating_sub(1) as u16;
            let block = Rect::new(area.x + left, y, area.width - left, 1);
            buf.set_style(block, Style::new().bg(theme::YOU_BG));
        }
        if *tone == Tone::Reply && text.starts_with(SUBAGENT_MARK) {
            let mark = Rect::new(area.x + 1, y, 1, 1);
            buf.set_style(mark, theme::tone(Tone::Accent));
        }
    }
}

fn running_rows(app: &App, tab: &Tab, pane: Rect, buf: &mut Buffer) {
    if tab.running.is_empty() {
        return;
    }
    let area = running_area(pane, tab.running.len() as u16);
    let rows = tab.running.iter().zip(area.top()..area.bottom());
    for (index, (running, y)) in rows.enumerate() {
        let selected = tab.running_sel == Some(index);
        let (mark, tone) = match (&running.opens, running.state) {
            (Opens::Workflow(_), _) => ("▸", Tone::Plain),
            (Opens::Agent(_), RunState::Working) => ("◆", Tone::Plain),
            (Opens::Agent(_), RunState::Done) => ("◇", Tone::Label),
            (Opens::Agent(_), RunState::Failed) => ("✗", Tone::Warn),
        };
        let head = format!(" {mark} {} · {}", running.name, running.job);
        draw_row(buf, area, y, &head, tone, selected);
        if !selected && running.state == RunState::Working {
            let cell = Rect::new(area.x + 1, y, 1, 1);
            buf.set_style(cell, theme::tone(Tone::Accent));
        }
        let style = if selected {
            theme::SELECTED
        } else {
            theme::tone(Tone::Label)
        };
        let room = usize::from(area.width.saturating_sub(RUNNING_STATUS_X));
        buf.set_stringn(area.x + RUNNING_STATUS_X, y, &running.status, room, style);
    }
    let hint = if app.can_fall_into_running() {
        Some("j select ")
    } else if app.can_enter_running() {
        Some("↑ select ")
    } else {
        None
    };
    if let (Some(hint), None, true) = (hint, tab.running_sel, area.width >= RUNNING_HINT_MIN_W) {
        let x = area.right() - Span::raw(hint).width() as u16;
        buf.set_string(x, area.bottom() - 1, hint, theme::tone(Tone::Label));
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
    if tab.parent.is_some() {
        buf.set_stringn(text_x, y, READ_ONLY, room as usize, label);
        return;
    }
    let placeholder = format!("Message {}", tab.agent);
    buf.set_stringn(text_x, y, placeholder, room as usize, label);
    if area.width > 40 {
        buf.set_string(area.right() - 12, y, "enter send", label);
    }
}

pub fn slash_area(pane: Rect) -> Rect {
    let message = message_box_area(pane);
    let top = message.y.saturating_sub(SLASH_H).max(pane.y + 3);
    Rect::new(message.x, top, message.width, message.y - top)
}

/// The list's rows inside `slash_area`, above its footer.
pub fn slash_rows(rect: Rect) -> Rect {
    let area = rect.inner(Margin::new(1, 1));
    Rect {
        height: area.height - 2,
        ..area
    }
}

fn slash_list(app: &App, selected: usize, pane: Rect, buf: &mut Buffer) {
    let rect = slash_area(pane);
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
    use crate::source::{Agent, Update};
    use crate::testing::{buffer, find, press, screen, with_right_pane};

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
                item: None,
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
        let s = screen(&with_right_pane(App::new(Vim, 132, 36)));
        assert_eq!(find(&s, "1 new session · Engineer"), Some((1, 30)));
        assert_eq!(find(&s, "g e n t s"), Some((25, 58)));
        let hint = "Pick a session on the left, or just start typing";
        assert_eq!(find(&s, hint), Some((27, 39)));
        assert_eq!(find(&s, "Message Engineer"), Some((32, 34)));
        assert_eq!(find(&s, "enter send"), Some((32, 84)));
        assert!(find(
            &screen(&with_right_pane(App::new(Vim, 70, 24))),
            "Just start typing"
        )
        .is_some());
    }

    #[test]
    fn the_startup_line_fits_the_mode() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc");
        let s = screen(&app);
        let docked = "Pick a session on the left, or press i to start typing";
        assert!(find(&s, docked).is_some());
        assert_eq!(find(&s, "just start typing"), None);
        press(&mut app, "i");
        assert!(find(&screen(&app), "just start typing").is_some());

        let mut app = with_right_pane(App::new(Vim, 70, 24));
        press(&mut app, "esc");
        let s = screen(&app);
        assert!(find(&s, "Press i to start typing").is_some());
        assert_eq!(find(&s, "Just start typing"), None);

        let mut app = with_right_pane(App::new(Vim, 90, 24));
        press(&mut app, "esc");
        let s = screen(&app);
        let (row, column) = find(&s, docked).expect("the line inside the narrowest pane");
        assert!(column > 26 && column + 54 < 83, "{column}");
        assert_eq!(row, 15);

        let mut app = with_right_pane(App::new(Emacs, 132, 36));
        press(&mut app, "esc");
        assert!(find(&screen(&app), "just start typing").is_some());
    }

    #[test]
    fn logo_sits_where_the_pictures_put_it() {
        let buf = buffer(&with_right_pane(App::new(Vim, 132, 36)));
        assert_eq!(buf[(60, 6)].bg, theme::LOGO);
        assert_ne!(buf[(59, 6)].bg, theme::LOGO);
        assert_eq!(buf[(42, 9)].bg, theme::LOGO);
        assert_ne!(buf[(41, 9)].bg, theme::LOGO);
    }

    fn chat_app() -> App {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        app.tabs[0].lines = sample_chat("Engineer");
        app
    }

    #[test]
    fn your_message_sits_right_on_a_tinted_block() {
        let app = chat_app();
        let text = "Group the sign-in commands under one parent.";
        let (row, column) = find(&screen(&app), text).expect("the message");
        assert_eq!(column + text.len() as u16, 95);
        let buf = buffer(&app);
        assert_eq!(buf[(column - 1, row)].bg, theme::YOU_BG);
        assert_eq!(buf[(95, row)].bg, theme::YOU_BG);
        assert_ne!(buf[(column - 2, row)].bg, theme::YOU_BG);
    }

    #[test]
    fn a_reply_is_highlighted_across_the_row() {
        let app = chat_app();
        let s = screen(&app);
        let buf = buffer(&app);
        let (reply, column) = find(&s, "Moving them").expect("the reply");
        assert_eq!(column, 31);
        assert_eq!(buf[(30, reply)].bg, theme::SEL_BG);
        assert_eq!(buf[(95, reply)].bg, theme::SEL_BG);
        let (tool, _) = find(&s, "✓ read").expect("a tool row");
        assert_ne!(buf[(30, tool)].bg, theme::SEL_BG);
        let (prompt, _) = find(&s, "! Allow").expect("the prompt");
        assert_ne!(buf[(30, prompt)].bg, theme::SEL_BG);
    }

    #[test]
    fn no_name_labels_in_the_conversation() {
        let mut app = chat_app();
        crate::testing::type_text(&mut app, "hi");
        press(&mut app, "i enter");
        for line in &app.tabs[0].lines {
            assert_ne!(line.text, "you");
            assert_ne!(line.text, "Engineer");
        }
    }

    #[test]
    fn a_long_message_keeps_one_left_edge() {
        let mut tab = Tab::new_session();
        tab.lines = vec![ChatLine {
            text: "hello 世界 wörld 🦀 ".repeat(8),
            tone: Tone::You,
            item: None,
        }];
        let rows = rows_at(&tab, 56);
        assert!(rows.len() > 1);
        let left = |row: &String| row.len() - row.trim_start().len();
        for row in &rows {
            assert_eq!(left(row), left(&rows[0]), "{rows:?}");
            assert!(Span::raw(row.as_str()).width() <= 55, "{row:?}");
        }
        assert!(left(&rows[0]) >= 1);
    }

    #[test]
    fn no_logo_with_messages() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = sample_chat("Engineer");
        let s = screen(&app);
        assert_eq!(find(&s, "g e n t s"), None);
        assert_eq!(find(&s, "Group the sign-in").map(|at| at.0), Some(4));
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
        let mut app = with_right_pane(App::new(Vim, 132, 36));
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
    fn a_multi_line_reply_wraps_each_line_and_keeps_its_tone() {
        let mut tab = Tab::new_session();
        let line = |text: &str, tone| ChatLine {
            text: text.to_string(),
            tone,
            item: None,
        };
        tab.lines = vec![
            line("one\ntwo three", Tone::Reply),
            line("ok\n", Tone::Label),
        ];
        assert_eq!(
            tab.wrapped_rows(5),
            [
                ("one".to_string(), Tone::Reply),
                ("two".to_string(), Tone::Reply),
                ("three".to_string(), Tone::Reply),
                ("ok".to_string(), Tone::Label),
                (String::new(), Tone::Label),
            ]
        );

        tab.lines = vec![line("a\nlonger", Tone::You)];
        let rows = rows_at(&tab, 20);
        assert_eq!(rows.len(), 2);
        let left = |row: &String| row.len() - row.trim_start().len();
        assert_eq!(left(&rows[0]), left(&rows[1]), "{rows:?}");
    }

    #[test]
    fn the_first_tab_takes_the_default_agent() {
        let (intents, _receiver) = std::sync::mpsc::channel();
        let mut app = App::connected(Vim, 132, 36, intents);
        crate::testing::type_text(&mut app, "hi");
        press(&mut app, "enter");
        assert_eq!(
            app.notice.as_deref(),
            Some("still connecting to the runtime")
        );
        assert_eq!(app.tabs[0].input.text(), "hi");

        let agent = |id: &str, name: &str| Agent {
            id: id.to_string(),
            name: name.to_string(),
            text: format!(" {name} agent"),
            tone: Tone::Plain,
        };
        app.apply(Update::Agents(vec![
            agent("local:default", "default"),
            agent("local:reviewer", "reviewer"),
        ]));
        assert_eq!(app.tabs[0].agent_id, "local:default");
        press(&mut app, "ctrl-c");
        let s = screen(&app);
        assert!(find(&s, "Message default").is_some());
        assert!(find(&s, "1 new session · default").is_some());

        app.push_layer(crate::app::layers::LayerKind::Picker);
        let s = screen(&app);
        assert!(find(&s, "default agent").is_some());
        assert!(find(&s, "reviewer agent").is_some());
    }

    #[test]
    fn centering_counts_columns_not_bytes() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        centered(&mut buf, Rect::new(0, 0, 10, 1), 0, "日本", Tone::Plain);
        assert_eq!(buf[(3, 0)].symbol(), "日");
    }
}
