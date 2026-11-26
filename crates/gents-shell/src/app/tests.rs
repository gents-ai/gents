use super::transcript::{blank, sample_chat, wrap};
use super::*;
use crate::draw;
use crate::draw::theme;
use crate::keys::KeySet::{Emacs, Regular, Vim};
use crate::sample::{RunState, Running};
use crate::testing::{
    buffer, find, keys, kinds, numbered, press, right_pane, screen, sent, session_row, type_text,
    with_right_pane, with_sample_session,
};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;

fn alt(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT))
}

#[test]
fn ctrl_x_ctrl_c_quits_in_emacs() {
    let mut app = App::new(Emacs, 80, 24);
    press(&mut app, "ctrl-x");
    assert!(!app.should_quit());
    press(&mut app, "ctrl-c");
    assert!(app.should_quit());
}

#[test]
fn ctrl_x_ctrl_c_does_not_quit_in_vim() {
    let mut app = App::new(Vim, 80, 24);
    press(&mut app, "ctrl-x ctrl-c");
    assert!(!app.should_quit());
    assert_eq!(app.notice.as_deref(), Some("ctrl-c again to quit"));
}

#[test]
fn a_key_between_cancels_the_chord() {
    let mut app = App::new(Emacs, 80, 24);
    press(&mut app, "ctrl-x q ctrl-c");
    assert!(!app.should_quit());
}

#[test]
fn resize_updates_size() {
    let mut app = App::new(Vim, 132, 36);
    app.handle(Event::Resize(70, 24));
    assert_eq!(app.size, (70, 24));
}

#[test]
fn selections_start_on_a_selectable_row() {
    let app = with_sample_session(App::new(Vim, 80, 24));
    assert!(app.session_rows()[app.sessions_sel]
        .text
        .contains("cli ux redesign"));
    // The Background view has no selectable row.
    for (view, selected) in [View::Diff, View::Workflows].into_iter().zip(app.view_sel) {
        assert!(app.view_rows(view)[selected].selectable(), "{view:?}");
    }
}

#[test]
fn release_events_are_ignored() {
    let mut app = App::new(Emacs, 80, 24);
    let mut release = keys("ctrl-x")[0];
    release.kind = KeyEventKind::Release;
    app.handle(Event::Key(release));
    press(&mut app, "ctrl-c");
    assert!(!app.should_quit());
}

#[test]
fn empty_tab_starts_in_input_and_esc_i_round_trips() {
    let mut app = App::new(Vim, 132, 36);
    assert_eq!(app.mode, Mode::Input);
    press(&mut app, "esc");
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.ctx(), KeyCtx::Normal);
    press(&mut app, "i");
    assert_eq!(app.mode, Mode::Input);
    assert_eq!(app.ctx(), KeyCtx::Input);

    let mut app = App::new(Emacs, 132, 36);
    assert_eq!(app.chip(), "EMACS");
    assert_eq!(app.ctx(), KeyCtx::Input);
    press(&mut app, "esc");
    assert_eq!(app.ctx(), KeyCtx::Input);
    assert_eq!(app.chip(), "EMACS");
}

#[test]
fn enter_sends_and_appends_a_sample_reply() {
    for set in [Vim, Emacs] {
        let mut app = with_right_pane(App::new(set, 132, 36));
        type_text(&mut app, "hi");
        press(&mut app, "enter");
        let s = screen(&app);
        assert_eq!(find(&s, "hi "), Some((4, 93)));
        assert_eq!(find(&s, "Engineer"), Some((1, 46)));
        let reply = "sample reply: nothing is wired to an agent yet.";
        assert_eq!(find(&s, reply), Some((6, 31)));
        assert!(app.tabs[0].input.is_empty());
        assert!(buffer(&app).content().iter().all(|c| c.bg != theme::LOGO));

        let tones: Vec<Tone> = app.tabs[0].lines.iter().map(|l| l.tone).collect();
        use Tone::{Plain, Reply, You};
        assert_eq!(tones, [You, Plain, Reply, Plain]);
    }
}

#[test]
fn enter_sends_a_draft_with_text_from_the_box_as_typed() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "   ");
    press(&mut app, "enter");
    assert!(app.tabs[0].lines.is_empty(), "whitespace only is not sent");
    assert_eq!(app.tabs[0].input.text(), "   ");

    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "hi");
    press(&mut app, "esc enter");
    assert!(app.tabs[0].lines.is_empty(), "not from normal mode");
    assert_eq!(app.tabs[0].input.text(), "hi");

    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, " hi ");
    press(&mut app, "enter");
    assert_eq!(app.tabs[0].lines[0].text, " hi ");
}

#[test]
fn ctrl_c_clears_then_quits_when_pressed_twice() {
    let hint = "ctrl-c again to quit";
    for set in [Regular, Vim, Emacs] {
        let mut app = App::new(set, 132, 36);
        type_text(&mut app, "hi");
        press(&mut app, "ctrl-c");
        assert!(app.tabs[0].input.is_empty(), "{set:?}");
        assert_eq!(app.notice, None, "{set:?}");

        press(&mut app, "ctrl-c");
        assert_eq!(app.notice.as_deref(), Some(hint), "{set:?}");
        assert!(find(&screen(&app), hint).is_some(), "{set:?}");
        assert!(!app.should_quit(), "{set:?}");

        press(&mut app, "esc");
        assert_eq!(app.notice, None, "{set:?}");
        press(&mut app, "ctrl-c");
        assert!(!app.should_quit(), "a key between starts over: {set:?}");

        app.handle(Event::Paste("x".to_string()));
        press(&mut app, "ctrl-c");
        assert!(!app.should_quit(), "so does a paste: {set:?}");
        assert!(app.tabs[0].input.is_empty(), "{set:?}");

        press(&mut app, "ctrl-c ctrl-c");
        assert!(app.should_quit(), "{set:?}");
    }
}

#[test]
fn alt_char_in_vim_acts_as_esc_then_char() {
    let mut app = App::new(Vim, 132, 36);
    app.tabs[0].lines = numbered(40);
    app.tabs[0].scroll = 3;
    app.handle(alt('j'));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.tabs[0].scroll, 2);

    let mut app = App::new(Emacs, 132, 36);
    app.tabs[0].lines = numbered(40);
    app.tabs[0].scroll = 3;
    app.handle(alt('j'));
    assert_eq!(app.tabs[0].scroll, 3);
    assert!(app.tabs[0].input.is_empty());
}

#[test]
fn ctrl_u_clears_the_message_box_a_jump_filter_and_the_search_line() {
    for (set, jump, search) in [(Vim, "esc :", "esc /"), (Emacs, "alt-x", "ctrl-s")] {
        let mut app = App::new(set, 132, 36);
        type_text(&mut app, "hello there");
        press(&mut app, "ctrl-u");
        assert!(app.tabs[0].input.is_empty(), "{set:?}");

        press(&mut app, jump);
        type_text(&mut app, "acc");
        press(&mut app, "ctrl-u");
        assert_eq!(top_layer(&app), LayerKind::Jump, "{set:?}");
        assert!(app.layers[0].filter.is_empty(), "{set:?}");
        press(&mut app, "esc");

        press(&mut app, search);
        type_text(&mut app, "abc");
        press(&mut app, "ctrl-u");
        assert!(app.search.as_ref().unwrap().query.is_empty(), "{set:?}");
    }
}

fn top_layer(app: &App) -> LayerKind {
    app.layers.last().expect("a layer").kind
}

/// Option-left and option-right send `esc b` and `esc f`, which arrive as alt-b and alt-f.
#[test]
fn alt_b_and_alt_f_move_by_word_in_a_vim_line_and_other_alt_keys_still_split() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "one two");
    app.handle(alt('b'));
    type_text(&mut app, "x");
    app.handle(alt('f'));
    type_text(&mut app, "!");
    assert_eq!(app.tabs[0].input.text(), "one xtwo!");
    assert_eq!(app.mode, Mode::Input);

    app.handle(alt('x'));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.tabs[0].input.text(), "one xtwo!");

    press(&mut app, ":");
    type_text(&mut app, "ab cd");
    app.handle(alt('b'));
    assert_eq!(top_layer(&app), LayerKind::Jump);
    type_text(&mut app, "x");
    assert_eq!(app.layers[0].filter.text(), "ab xcd");
}

#[test]
fn alt_i_in_vim_stays_in_input() {
    let mut app = App::new(Vim, 132, 36);
    app.handle(alt('i'));
    assert_eq!(app.mode, Mode::Input);
    assert!(app.tabs[0].input.is_empty());
}

#[test]
fn a_paste_sends_as_one_message_with_its_lines() {
    let (mut app, intents) = connected();
    app.handle(Event::Paste("line one\n\n    line two".to_string()));
    assert_eq!(app.tabs[0].input.text(), "line one\n\n    line two");
    press(&mut app, "enter");
    let sent = intents.try_recv().expect("one send");
    assert!(
        matches!(&sent, Intent::Send { text, .. } if text == "line one\n\n    line two"),
        "{sent:?}"
    );
    assert!(intents.try_recv().is_err(), "one message, not one a line");
}

const SIX_LINES: &str = "1\n2\n3\n4\n5\n6";
const BADGE: &str = "[Pasted text #1 +6 lines]";

/// `a [badge] b` after `keys`, with `|` typed where the cursor is, and what it sends.
fn around_a_badge(keys: &str) -> (String, String) {
    let mut app = App::new(Regular, 132, 36);
    type_text(&mut app, "a ");
    app.handle(Event::Paste(SIX_LINES.to_string()));
    type_text(&mut app, " b");
    press(&mut app, keys);
    type_text(&mut app, "|");
    let input = &app.tabs[0].input;
    let held = input.badges().count();
    assert_eq!(held, usize::from(input.text().contains(BADGE)), "{keys}");
    (input.text().to_string(), input.sent())
}

#[test]
fn every_move_and_delete_takes_a_badge_whole() {
    let kept = |at: &str| {
        let text = at.replace('B', BADGE);
        let sent = at.replace('B', SIX_LINES);
        (text, sent)
    };
    let gone = |at: &str| (at.to_string(), at.to_string());
    let cases = [
        ("left left left", kept("a |B b")),
        ("home right right right", kept("a B| b")),
        ("left left alt-b", kept("a |B b")),
        ("home alt-f alt-f", kept("a B| b")),
        ("home right right del", gone("a | b")),
        ("left left bs", gone("a | b")),
        ("left left ctrl-w", gone("a | b")),
        ("left left ctrl-u", gone("| b")),
    ];
    for (keys, want) in cases {
        assert_eq!(around_a_badge(keys), want, "{keys}");
    }
}

#[test]
fn a_badges_text_pasted_again_right_after_it_is_typed_in_to_edit() {
    let (mut app, intents) = connected();
    app.handle(Event::Paste(SIX_LINES.to_string()));
    assert_eq!(app.tabs[0].input.text(), BADGE);
    app.handle(Event::Paste(SIX_LINES.to_string()));
    assert_eq!(app.tabs[0].input.text(), SIX_LINES);
    assert_eq!(app.tabs[0].input.badges().count(), 0);
    press(&mut app, "up");
    type_text(&mut app, "x");
    press(&mut app, "enter");
    let sends = sent(&intents);
    let edited = "1\n2\n3\n4\n5x\n6";
    let whole = matches!(&sends[..], [Intent::Send { text, .. }] if text == edited);
    assert!(whole, "{sends:?}");
}

#[test]
fn a_paste_of_up_to_five_lines_is_typed_into_the_box_and_a_field_gets_one_line() {
    let (mut app, _intents) = connected();
    app.handle(Event::Paste("1\n2\n3\n4\n5".to_string()));
    assert_eq!(app.tabs[0].input.text(), "1\n2\n3\n4\n5");
    press(&mut app, "esc /");
    app.handle(Event::Paste(SIX_LINES.to_string()));
    let query = app.search.as_ref().expect("the search line").query.text();
    assert_eq!(query, "1 2 3 4 5 6");
}

fn ready(set: KeySet) -> (App, std::sync::mpsc::Receiver<Intent>) {
    let (mut app, intents) = crate::testing::connected(set, 132, 36);
    app.agents = crate::sample::agents();
    (app, intents)
}

#[test]
fn modified_enter_and_ctrl_j_break_the_line_and_enter_sends_every_line() {
    for set in [Regular, Vim, Emacs] {
        for newline in ["shift-enter", "alt-enter", "ctrl-enter", "ctrl-j"] {
            let (mut app, intents) = ready(set);
            type_text(&mut app, "one");
            press(&mut app, newline);
            type_text(&mut app, "two");
            assert_eq!(app.tabs[0].input.text(), "one\ntwo", "{set:?} {newline}");
            press(&mut app, "enter");
            let sends = sent(&intents);
            let whole = matches!(&sends[..], [Intent::Send { text, .. }] if text == "one\ntwo");
            assert!(whole, "{set:?} {newline}: {sends:?}");
        }
    }
}

#[test]
fn up_and_down_cross_the_lines_then_do_what_a_one_line_box_does() {
    for (set, up, down) in [
        (Regular, "up", "down"),
        (Vim, "up", "down"),
        (Emacs, "ctrl-p", "ctrl-n"),
    ] {
        let mut app = App::new(set, 132, 36);
        app.tabs[0].lines = numbered(80);
        type_text(&mut app, "one");
        press(&mut app, "shift-enter");
        type_text(&mut app, "two");
        press(&mut app, up);
        type_text(&mut app, "!");
        assert_eq!(app.tabs[0].input.text(), "one!\ntwo", "{set:?}");
        assert_eq!(app.tabs[0].scroll, 0, "{set:?}");
        press(&mut app, up);
        assert_eq!(
            app.tabs[0].scroll, 1,
            "{set:?}: up from the first line scrolls"
        );
        press(&mut app, down);
        type_text(&mut app, "?");
        assert_eq!(app.tabs[0].input.text(), "one!\ntwo?", "{set:?}");
        assert_eq!(app.tabs[0].scroll, 1, "{set:?}");
        press(&mut app, down);
        assert_eq!(
            app.tabs[0].scroll, 0,
            "{set:?}: down from the last line scrolls"
        );
    }
}

#[test]
fn the_box_grows_to_six_rows_then_scrolls_and_the_conversation_gives_way() {
    let mut app = App::new(Regular, 132, 36);
    app.tabs[0].lines = numbered(80);
    let row_of = |app: &App, text: &str| find(&screen(app), text).map(|(row, _)| row);
    let end = row_of(&app, "message 79").expect("the conversation's end");
    type_text(&mut app, "line 1");
    for n in 2..=8 {
        press(&mut app, "shift-enter");
        type_text(&mut app, &format!("line {n}"));
    }
    assert_eq!(row_of(&app, "message 79"), Some(end - 5));
    let rows = |app: &App| -> Vec<Option<u16>> {
        (1..=8).map(|n| row_of(app, &format!("line {n}"))).collect()
    };
    // Under the conversation's new last row and the box's top edge.
    let top = end - 3;
    let shown = |first: u16| (1..=8).map(move |n| (first..first + 6).contains(&n));
    let at = |first: u16| -> Vec<Option<u16>> {
        shown(first)
            .zip(1..)
            .map(|(on, n)| on.then_some(top + n - first))
            .collect()
    };
    assert_eq!(rows(&app), at(3));
    let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
    terminal.draw(|frame| draw::draw(&app, frame)).unwrap();
    terminal.backend_mut().assert_cursor_position((40, top + 5));
    press(&mut app, "up up up up up up up");
    assert_eq!(rows(&app), at(1));
    terminal.draw(|frame| draw::draw(&app, frame)).unwrap();
    terminal.backend_mut().assert_cursor_position((40, top));
}

#[test]
fn in_the_smallest_window_the_box_grows_only_while_the_conversation_keeps_a_row() {
    let mut app = App::new(Regular, 60, 16);
    app.tabs[0].lines = numbered(80);
    app.tabs[0].running = crate::sample::running();
    let rows = app.tabs[0].running.len();
    for n in 1..=8 {
        type_text(&mut app, &format!("line {n}"));
        press(&mut app, "shift-enter");
    }
    let s = screen(&app);
    assert!(find(&s, "message 79").is_some(), "{s:#?}");
    let shown = (1..=8).filter(|n| find(&s, &format!("line {n}")).is_some());
    assert_eq!(shown.count(), 7 - rows - 1, "{s:#?}");
}

/// `/goal` and `more` lines under it, with the slash list open.
fn goal_and_lines(width: u16, height: u16, more: usize) -> App {
    let mut app = App::new(Regular, width, height);
    type_text(&mut app, "/goal");
    for n in 1..=more {
        press(&mut app, "shift-enter");
        type_text(&mut app, &format!("step {n}"));
    }
    assert!(app.slash.is_some());
    app
}

#[test]
fn the_slash_list_keeps_a_row_over_a_grown_box_in_the_smallest_windows() {
    const GOAL_ROW: &str = "/goal      set a goal";
    for height in [16, 17] {
        let mut app = goal_and_lines(60, height, 5);
        let s = screen(&app);
        let (row, column) = find(&s, GOAL_ROW).expect("the list's row");
        assert!(find(&s, "step 5").is_some(), "{height}: {s:#?}");
        app.handle(Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: ratatui::crossterm::event::MouseEventKind::Down(
                ratatui::crossterm::event::MouseButton::Left,
            ),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.slash, None, "{height}: the click ran the row");
    }
    let app = goal_and_lines(132, 36, 5);
    assert_eq!(
        find(&screen(&app), "step 5").map(|(row, _)| row),
        Some(32),
        "where there is room the box keeps its six rows"
    );
}

#[test]
fn many_running_rows_in_the_smallest_window_leave_the_box_drawn() {
    let mut app = App::new(Regular, 60, 16);
    app.tabs[0].lines = numbered(3);
    app.tabs[0].running = (0..3).flat_map(|_| crate::sample::running()).collect();
    assert!(app.tabs[0].running.len() >= 8);
    type_text(&mut app, "hi");
    press(&mut app, "esc ctrl-f");
    let s = screen(&app);
    assert!(find(&s, "> hi").is_some(), "{s:#?}");
}

#[test]
fn the_startup_hint_gives_way_to_a_grown_box() {
    let mut app = App::new(Regular, 132, 36);
    type_text(&mut app, "a");
    for line in ["b", "c", "d", "e", "f"] {
        press(&mut app, "shift-enter");
        type_text(&mut app, line);
    }
    let s = screen(&app);
    let (row, _) = find(&s, "> a").expect("the box's first line");
    let after = s[usize::from(row)].split_once("> a").expect("the line").1;
    assert!(after.trim_start().starts_with('│'), "{after:?}");
    assert_eq!(find(&s, "start typing"), None, "{s:#?}");
    assert!(find(&s, "g e n t s").is_some());
}

#[test]
fn a_click_on_the_grown_box_starts_typing_in_it() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "one");
    press(&mut app, "shift-enter");
    type_text(&mut app, "two");
    press(&mut app, "esc");
    let (row, column) = find(&screen(&app), "one").expect("the box's first line");
    app.handle(Event::Mouse(ratatui::crossterm::event::MouseEvent {
        kind: ratatui::crossterm::event::MouseEventKind::Down(
            ratatui::crossterm::event::MouseButton::Left,
        ),
        column,
        row: row - 1,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(app.mode, Mode::Input);
}

#[test]
fn line_keys_take_the_cursors_line_in_a_box_of_lines() {
    let cases = [
        ("home", "one\n|two"),
        ("super-left", "one\n|two"),
        ("ctrl-a", "one\n|two"),
        ("home end", "one\ntwo|"),
        ("super-right", "one\ntwo|"),
        ("ctrl-u", "one\n|"),
        ("super-bs", "one\n|"),
        ("alt-bs", "one\n|"),
        ("ctrl-bs", "one\n|"),
        ("alt-left", "one\n|two"),
        ("ctrl-left", "one\n|two"),
        ("home alt-right", "one\ntwo|"),
        ("home ctrl-right", "one\ntwo|"),
    ];
    for set in [Regular, Vim, Emacs] {
        for (keys, want) in cases {
            let mut app = App::new(set, 132, 36);
            type_text(&mut app, "one");
            press(&mut app, "shift-enter");
            type_text(&mut app, "two");
            press(&mut app, keys);
            type_text(&mut app, "|");
            assert_eq!(app.tabs[0].input.text(), want, "{set:?} {keys}");
        }
    }
}

#[test]
fn a_paste_in_normal_mode_goes_into_the_box() {
    let (mut app, _intents) = connected();
    press(&mut app, "esc");
    assert_eq!(app.mode, Mode::Normal);
    app.handle(Event::Paste("hi".to_string()));
    assert_eq!(app.mode, Mode::Input);
    assert_eq!(app.tabs[0].input.text(), "hi");
}

#[test]
fn a_paste_into_the_jump_bar_stays_on_one_line() {
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc");
    press(&mut app, ":");
    app.handle(Event::Paste("tab\nclose".to_string()));
    assert_eq!(
        app.layers.last().expect("the bar").filter.text(),
        "tab close"
    );
    assert!(app.tabs[0].input.is_empty());
}

#[test]
fn a_sent_message_keeps_its_blank_lines_and_indent() {
    let mut tab = Tab::new_session();
    tab.lines = vec![ChatLine {
        text: "one\n\n    two".to_string(),
        tone: Tone::You,
        item: None,
        change: None,
        under: None,
    }];
    let rows: Vec<String> = tab
        .wrapped_rows(40)
        .into_iter()
        .map(|(row, _)| row)
        .collect();
    let edge = " ".repeat(32);
    assert_eq!(
        rows,
        [format!("{edge}one"), edge.clone(), format!("{edge}    two")]
    );
}

#[test]
fn pasted_newline_sends_that_line() {
    let mut app = App::new(Vim, 132, 36);
    for code in [KeyCode::Char('a'), KeyCode::Enter, KeyCode::Char('b')] {
        app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }
    assert_eq!(app.tabs[0].lines[0].text, "a");
    assert_eq!(
        app.tabs[0]
            .lines
            .iter()
            .filter(|l| l.tone == Tone::You)
            .count(),
        1
    );
    assert_eq!(app.tabs[0].input.text(), "b");
}

#[test]
fn scroll_never_runs_past_the_content() {
    let mut app = App::new(Vim, 132, 36);
    app.tabs[0].lines = numbered(40);
    press(&mut app, "esc");
    for _ in 0..100 {
        press(&mut app, "k");
    }
    let s = screen(&app);
    assert_eq!(find(&s, "message 0 "), Some((4, 30)));
    let top = app.tabs[0].scroll;
    assert_eq!(top, 40 - 27);

    press(&mut app, "j");
    assert_eq!(app.tabs[0].scroll, top - 1);
    assert_eq!(find(&screen(&app), "message 1 "), Some((4, 30)));

    press(&mut app, "i");
    type_text(&mut app, "hi");
    press(&mut app, "enter");
    assert_eq!(app.tabs[0].scroll, 0);
}

#[test]
fn gg_and_shift_g_or_alt_angles_go_to_the_top_and_bottom() {
    for (set, top, bottom) in [(Vim, "esc g g", "G"), (Emacs, "alt-<", "alt->")] {
        let mut app = App::new(set, 132, 36);
        app.tabs[0].lines = numbered(40);
        press(&mut app, top);
        assert_eq!(app.tabs[0].scroll, 40 - 27, "{set:?}");
        assert_eq!(find(&screen(&app), "message 0 "), Some((4, 30)), "{set:?}");
        press(&mut app, bottom);
        assert_eq!(app.tabs[0].scroll, 0, "{set:?}");
    }

    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc h G");
    assert_eq!(app.focus, Pane::Sessions);
    let rows = app.session_rows();
    let last = rows.iter().rposition(SidebarRow::selectable).unwrap();
    assert!(last > 0);
    assert_eq!(app.sessions_sel, last);
    press(&mut app, "g g");
    assert_eq!(app.sessions_sel, first_selectable(&rows));
}

#[test]
fn scroll_comes_back_at_once_after_the_window_grows() {
    let mut app = App::new(Vim, 70, 24);
    app.tabs[0].lines = numbered(40);
    press(&mut app, "esc");
    for _ in 0..100 {
        press(&mut app, "k");
    }
    app.handle(Event::Resize(132, 36));
    press(&mut app, "j");
    assert_eq!(app.tabs[0].scroll, 40 - 27 - 1);
}

#[test]
fn scrolling_at_too_small_does_nothing() {
    let mut app = App::new(Vim, 132, 36);
    app.tabs[0].lines = numbered(40);
    press(&mut app, "esc");
    app.handle(Event::Resize(40, 10));
    press(&mut app, "k");
    assert_eq!(app.tabs[0].scroll, 0);
}

#[test]
fn cursor_sits_at_the_text_end() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "hello");
    let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
    terminal.draw(|frame| draw::draw(&app, frame)).unwrap();
    terminal.backend_mut().assert_cursor_position((39, 32));
    assert_eq!(find(&screen(&app), "hello"), Some((32, 34)));
}

#[test]
fn the_cursor_is_hidden_unless_typing_goes_to_the_box() {
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc");
    let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
    terminal.draw(|frame| draw::draw(&app, frame)).unwrap();
    assert!(!terminal.backend().cursor_visible());

    press(&mut app, "i");
    terminal.draw(|frame| draw::draw(&app, frame)).unwrap();
    assert!(terminal.backend().cursor_visible());
}

#[test]
fn long_and_wide_text_keeps_the_cursor_in_the_box() {
    let mut app = App::new(Vim, 70, 24);
    type_text(&mut app, &"日".repeat(100));
    let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
    terminal.draw(|frame| draw::draw(&app, frame)).unwrap();
    let position = terminal.backend().cursor_position();
    assert_eq!(position.y, 20);
    assert!(position.x >= 6 && position.x < 66, "{position:?}");
    let buf = terminal.backend().buffer();
    assert_eq!(buf[(2, 20)].symbol(), "│");
    assert_eq!(buf[(67, 20)].symbol(), "│");
    press(&mut app, "bs");
    assert_eq!(app.tabs[0].input.text().chars().count(), 99);
}

#[test]
fn hints_give_way_to_text() {
    let mut app = App::new(Vim, 132, 36);
    assert!(screen(&app)[32].contains("enter send"));
    type_text(&mut app, "hi");
    let s = screen(&app);
    assert!(!s[32].contains("enter send"));
    assert_eq!(find(&s, "Message Engineer"), None);
}

#[test]
fn each_tab_keeps_its_own_draft() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "one");
    app.tabs.push(Tab::new_session());
    app.active = 1;
    assert!(find(&screen(&app), "one").is_none());
    type_text(&mut app, "two");
    app.active = 0;
    assert_eq!(app.tabs[0].input.text(), "one");
}

#[test]
fn wrap_breaks_at_spaces_and_between_graphemes_by_display_width() {
    for (text, width, rows) in [
        ("alpha beta gamma", 10, &["alpha beta", "gamma"][..]),
        ("alpha beta", 7, &["alpha", "beta"]),
        ("aaaaaaaaaaaa", 5, &["aaaaa", "aaaaa", "aa"]),
        ("ab cccccccc", 5, &["ab", "ccccc", "ccc"]),
        ("abcdefghij  klmnopqrst", 10, &["abcdefghij", "klmnopqrst"]),
        ("one  two", 4, &["one", "two"]),
        ("", 5, &[""]),
        ("世界世界", 3, &["世", "界", "世", "界"]),
        ("日本語", 5, &["日本", "語"]),
        ("日本", 1, &["日", "本"]),
        ("a✔\u{fe0f}", 2, &["a", "✔\u{fe0f}"]),
        ("👩\u{200d}💻", 3, &["👩\u{200d}💻"]),
    ] {
        assert_eq!(wrap(text, width), rows, "{text:?} at {width}");
    }
}

fn session_app() -> App {
    let mut app = App::new(Vim, 132, 36);
    enter_on_session(&mut app, "pack registry");
    assert_eq!(app.mode, Mode::Normal);
    app
}

#[test]
fn rows_sit_directly_above_the_message_box() {
    let s = screen(&session_app());
    assert_eq!(find(&s, "◆ Reviewer · auth grouping"), Some((27, 31)));
    assert_eq!(find(&s, "◆ Researcher · registry login"), Some((28, 31)));
    assert_eq!(find(&s, "◇ Triage · inbox"), Some((29, 31)));
    assert_eq!(find(&s, "▸ change unit · cli ux"), Some((30, 31)));
    assert_eq!(find(&s, "reading crates/gents-cli"), Some((27, 62)));
    assert_eq!(find(&s, "╭").map(|at| at.0), Some(31));
}

#[test]
fn a_long_running_row_keeps_its_status_apart_from_its_title() {
    let mut app = session_app();
    let row = &mut app.tabs[0].running[1];
    row.name = "wf-read-only".to_string();
    row.job = "Disk scan: modules summary.".to_string();
    row.status = "finished".to_string();
    row.state = RunState::Done;
    let s = screen(&app);
    let title = "◇ wf-read-only · Disk scan: modules summary.";
    let (y, x) = find(&s, title).expect("the whole title");
    let after = &s[usize::from(y)]
        .chars()
        .skip(usize::from(x))
        .collect::<String>();
    let status = after[title.len()..].trim_start();
    assert!(status.starts_with("finished"), "{after:?}");
    assert_eq!(
        find(&s, "reading crates/gents-cli").map(|at| at.1),
        find(&s, "finished").map(|at| at.1),
        "the statuses stay in one column"
    );

    let mut app = session_app();
    app.handle(Event::Resize(60, 36));
    app.tabs[0].running[1].job = "a title much longer than the row has room for".into();
    let s = screen(&app);
    let (y, _) = find(&s, "Researcher · a title").expect("the row");
    let shown = &s[usize::from(y)];
    let cut = shown.find('…').expect("the title is cut");
    let status = shown.find("running for 4m").expect("the status whole");
    assert!(cut < status, "{shown:?}");
}

#[test]
fn a_tab_with_nothing_running_has_no_rows() {
    let s = screen(&App::new(Vim, 132, 36));
    assert_eq!(find(&s, "◆"), None);
    assert_eq!(find(&s, "select"), None);
}

#[test]
fn the_conversation_gives_up_a_row_for_each() {
    let mut app = session_app();
    app.tabs[0].lines = numbered(100);
    app.tabs[0].prompts.clear();
    assert_eq!(find(&screen(&app), "message 99").map(|at| at.0), Some(26));
}

#[test]
fn the_running_rows_are_entered_and_left_at_their_edges() {
    let mut app = session_app();
    press(&mut app, "i up");
    assert_eq!(app.tabs[0].running_sel, Some(3));
    assert_eq!(app.chip(), "AGENTS");
    let s = screen(&app);
    assert!(find(&s, "enter open · j/k move · esc back").is_some());
    press(&mut app, "down");
    assert_eq!(app.tabs[0].running_sel, None);
    assert_eq!(app.ctx(), KeyCtx::Input);
    type_text(&mut app, "x");
    press(&mut app, "up");
    assert_eq!(app.tabs[0].running_sel, None, "not with text in the box");

    let mut app = session_app();
    press(&mut app, "i up up up up up");
    assert_eq!(app.tabs[0].running_sel, None);
    assert_eq!(app.mode, Mode::Normal);

    let mut app = session_app();
    press(&mut app, "j");
    assert_eq!(app.tabs[0].running_sel, Some(0));
    press(&mut app, "k");
    assert_eq!(app.tabs[0].running_sel, None);
    assert_eq!(app.ctx(), KeyCtx::Normal);
    press(&mut app, "j j j j j j j");
    assert_eq!(app.tabs[0].running_sel, Some(3));
    press(&mut app, "ctrl-w h");
    assert_eq!(app.focus, Pane::Sessions);
    assert_eq!(app.tabs[0].running_sel, None);

    let mut app = session_app();
    app.tabs[0].lines = numbered(100);
    press(&mut app, "k k j");
    assert_eq!((app.tabs[0].scroll, app.tabs[0].running_sel), (1, None));
    press(&mut app, "j");
    assert_eq!((app.tabs[0].scroll, app.tabs[0].running_sel), (0, None));
    press(&mut app, "j");
    assert_eq!(app.tabs[0].running_sel, Some(0));

    let mut app = session_app();
    press(&mut app, "ctrl-w j");
    assert_eq!(app.tabs[0].running_sel, Some(0));
    press(&mut app, "ctrl-w k");
    assert_eq!(app.tabs[0].running_sel, None);
}

/// The sidebar shows the subagent and takes the keys where it docks; a narrow window opens it
/// as a layer. The tab, and its box, stay on the parent.
#[test]
fn enter_on_a_subagent_shows_it_in_the_sidebar() {
    let mut app = session_app();
    let before = app.tabs[0].clone();
    press(&mut app, "j enter");
    let opened = Tab {
        sidebar: true,
        ..before
    };
    assert_eq!(app.tabs[0], opened, "the tab still shows the parent");
    assert_eq!((app.focus, app.ctx()), (Pane::Sidebar, KeyCtx::Normal));
    let pane = right_pane(&app);
    for text in ["Subagent: Reviewer", "Checking the new"] {
        assert!(find(&pane, text).is_some(), "{text}: {pane:#?}");
    }
    let s = screen(&app);
    for text in ["1 packs · Engineer", "Moving them", "Message Engineer"] {
        assert!(find(&s, text).is_some(), "{text}");
    }

    let mut narrow = App::new(Vim, 70, 24);
    narrow.open_session(session_row("pack registry"));
    press(&mut narrow, "j enter");
    assert_eq!(kinds(&narrow), [LayerKind::Sidebar]);
    assert!(find(&screen(&narrow), "Subagent: Reviewer").is_some());
    press(&mut narrow, "esc");
    assert!(narrow.layers.is_empty(), "esc closes it as other views");
}

/// Picking an open session again reads it again, unless its tab follows a turn it sent: a
/// resume under a streaming turn could repeat or drop its text. Another client's turn is not
/// streamed here, so it does not block the reread, and ctrl-c still stops it.
#[test]
fn picking_an_open_session_again_rereads_it_unless_it_follows_its_own_turn() {
    let (mut app, intents) = with_runtime_agents();
    let listed = Session {
        agent: "local:default".to_string(),
        ..listed("s1", Day::Today, SessionState::Idle)
    };
    app.apply(Update::Sessions(vec![listed]));
    let row = app
        .session_rows()
        .iter()
        .position(|row| row.text.contains("s1"))
        .expect("the session's row");
    let open = Intent::Open {
        session: "s1".to_string(),
        agent: "local:default".to_string(),
    };
    app.open_session(row);
    assert_eq!(intents.try_recv(), Ok(open.clone()));

    app.open_session(row);
    assert_eq!(intents.try_recv(), Ok(open.clone()), "picked again");
    assert_eq!(app.tabs.len(), 1);

    app.apply(turn("t1"));
    app.open_session(row);
    assert_eq!(intents.try_recv(), Ok(open.clone()), "a foreign turn");
    press(&mut app, "ctrl-c");
    let stop = Intent::Interrupt {
        session: "s1".to_string(),
        agent: "local:default".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(intents.try_recv(), Ok(stop), "ctrl-c still stops it");

    type_text(&mut app, "hi");
    press(&mut app, "enter");
    assert!(matches!(intents.try_recv(), Ok(Intent::Send { .. })));
    app.apply(turn("t2"));
    app.open_session(row);
    assert!(intents.try_recv().is_err(), "its own turn is left alone");
    assert_eq!((app.tabs.len(), app.active), (1, 0));
    app.apply(ended("t2"));
    app.open_session(row);
    assert_eq!(intents.try_recv(), Ok(open), "once it ends");

    let mut app = App::new(Vim, 132, 36);
    enter_on_session(&mut app, "flaky");
    press(&mut app, "i");
    type_text(&mut app, "hi");
    press(&mut app, "enter");
    let lines = app.tabs[0].lines.clone();
    enter_on_session(&mut app, "flaky");
    assert_eq!(
        app.tabs[0].lines, lines,
        "the sample keeps nothing to reread"
    );
}

fn subagent(name: &str, state: RunState, session: &str) -> Running {
    Running {
        name: name.to_string(),
        job: format!("{name} job"),
        status: "working".to_string(),
        state,
        opens: Opens::Agent(session.to_string()),
    }
}

fn running(session: &str, rows: Vec<Running>) -> Update {
    Update::Running {
        session: session.to_string(),
        rows,
    }
}

/// A connected tab on session `s1` of agent `local:default`, with two subagents.
fn with_subagents() -> (App, std::sync::mpsc::Receiver<Intent>) {
    let (mut app, intents) = with_runtime_agents();
    app.tabs[0].session = Some("s1".to_string());
    app.tabs[0].name = "fix login".to_string();
    app.apply(running(
        "s1",
        vec![
            subagent("researcher", RunState::Working, "c1"),
            subagent("triage", RunState::Done, "c2"),
        ],
    ));
    (app, intents)
}

/// The subagent's session opens on the parent's connection. Its updates reach the sidebar,
/// drawn as the tab draws a conversation, and the parent's reach the tab, whose box still
/// messages the parent. Another subagent takes its place.
#[test]
fn a_subagent_streams_into_the_sidebar_beside_its_parent() {
    let (mut app, intents) = with_subagents();
    press(&mut app, "up up enter");
    let open = |session: &str| Intent::Open {
        session: session.to_string(),
        agent: "local:default".to_string(),
    };
    assert_eq!(sent(&intents), [open("c1")]);

    app.apply(Update::Opened {
        session: "c1".to_string(),
        lines: vec![said("looking", Tone::Reply, "r1")],
    });
    app.apply(item("c1", said("found it", Tone::Reply, "r2")));
    app.apply(item("s1", said("still going", Tone::Reply, "p1")));
    assert_eq!(
        app.tabs[0].lines,
        [said("still going", Tone::Reply, "p1"), blank()]
    );
    let pane = right_pane(&app);
    for text in ["Subagent: researcher", "looking", "found it"] {
        assert!(find(&pane, text).is_some(), "{text}: {pane:#?}");
    }
    assert_eq!(find(&pane, "still going"), None);
    let (row, column) = find(&screen(&app), "found it").expect("the reply");
    assert_eq!(buffer(&app)[(column, row)].bg, theme::SEL_BG);

    press(&mut app, "i");
    type_text(&mut app, "hi");
    press(&mut app, "enter");
    let send = Intent::Send {
        tab: app.tabs[0].id,
        session: Some("s1".to_string()),
        agent: "local:default".to_string(),
        text: "hi".to_string(),
    };
    assert_eq!(sent(&intents), [send]);

    press(&mut app, "esc j j enter");
    assert_eq!(sent(&intents), [open("c2")]);
    let pane = right_pane(&app);
    assert!(find(&pane, "Subagent: triage").is_some(), "{pane:#?}");
    assert_eq!(find(&pane, "looking"), None);
}

#[test]
fn a_lost_connection_opens_no_subagent() {
    let (mut app, intents) = with_subagents();
    app.apply(Update::Connection(Connection::Lost("gone".to_string())));
    press(&mut app, "up up enter");
    assert!(intents.try_recv().is_err());
    assert_eq!(app.subagent, None);
    assert!(!right_is_shown(&app));
    assert_eq!(app.notice.as_deref(), Some("gone"));
}

/// The sidebar that shows a subagent stops the subagent's turn while it has the keys, docked
/// or as a layer; elsewhere the tab's turn stops.
#[test]
fn stopping_on_the_subagent_view_stops_the_subagent() {
    let stop = |session: &str, turn: &str| Intent::Interrupt {
        session: session.to_string(),
        agent: "local:default".to_string(),
        turn: turn.to_string(),
    };
    let opened = |set, size: (u16, u16)| {
        let (mut app, intents) = with_subagents();
        app.keymap = Keymap::new(set);
        app.handle(Event::Resize(size.0, size.1));
        press(&mut app, "up up enter");
        for (session, turn) in [("s1", "t0"), ("c1", "t1")] {
            app.apply(Update::Turn {
                session: session.to_string(),
                turn: turn.to_string(),
            });
        }
        sent(&intents);
        (app, intents)
    };
    for (set, keys) in [
        (Regular, "ctrl-c"),
        (Emacs, "ctrl-c"),
        (Vim, "ctrl-c"),
        (Vim, "s"),
    ] {
        for size in [(132, 36), (70, 24)] {
            let (mut app, intents) = opened(set, size);
            press(&mut app, keys);
            assert_eq!(
                sent(&intents),
                [stop("c1", "t1")],
                "{set:?} {keys} {size:?}"
            );
        }
    }
    let (mut app, intents) = opened(Vim, (132, 36));
    press(&mut app, "h s");
    assert_eq!(
        sent(&intents),
        [stop("s1", "t0")],
        "the conversation's keys"
    );
}

/// Esc on the docked subagent view closes it and puts the right pane back as it was.
#[test]
fn esc_closes_the_docked_subagent_view() {
    for (set, back, open) in [
        (Regular, "esc", "up up enter"),
        (Vim, "esc", "j enter"),
        (Emacs, "ctrl-g", "up up enter"),
    ] {
        let (mut app, _intents) = with_subagents();
        app.keymap = Keymap::new(set);
        press(&mut app, "up up enter");
        assert!(right_is_shown(&app), "{set:?}");
        press(&mut app, back);
        assert_eq!(app.subagent, None, "{set:?}");
        assert!(!right_is_shown(&app), "{set:?}: hidden, as it was");
        assert_eq!((app.view, app.focus), (View::Diff, Pane::Agent), "{set:?}");

        app.run_effect(Effect::ShowSidebar(View::Workflows));
        press(&mut app, open);
        assert_eq!(app.view, View::Subagent, "{set:?}");
        press(&mut app, back);
        assert!(right_is_shown(&app), "{set:?}: shown, as it was");
        let shown = (app.view, app.focus, app.subagent.as_ref());
        assert_eq!(shown, (View::Workflows, Pane::Agent, None), "{set:?}");
    }
}

#[test]
fn closing_the_parent_tab_drops_its_subagent() {
    let (mut app, _intents) = with_subagents();
    press(&mut app, "up up enter ctrl-w h ctrl-w q");
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].session, None, "a new tab in its place");
    assert_eq!((app.view, app.subagent.as_ref()), (View::Diff, None));
}

/// From the bottom, as the tab scrolls its conversation, docked or as a layer.
#[test]
fn the_sidebar_scrolls_its_subagent() {
    let scroll = |app: &App| app.subagent.as_ref().map(|shown| shown.scroll);
    for (set, up, down) in [
        (Regular, "up", "down"),
        (Vim, "k", "j"),
        (Emacs, "ctrl-p", "ctrl-n"),
    ] {
        let (mut app, _intents) = with_subagents();
        app.keymap = Keymap::new(set);
        press(&mut app, "up up enter");
        app.apply(Update::Opened {
            session: "c1".to_string(),
            lines: numbered(100),
        });
        assert!(find(&right_pane(&app), "message 99").is_some(), "{set:?}");
        press(&mut app, up);
        assert_eq!(scroll(&app), Some(1), "{set:?}");
        assert_eq!(find(&right_pane(&app), "message 99"), None, "{set:?}");
        press(&mut app, down);
        assert_eq!(scroll(&app), Some(0), "{set:?}");
        press(&mut app, "pgup");
        assert!(scroll(&app) > Some(1), "{set:?}");
        press(&mut app, "pgdn");
        assert_eq!(scroll(&app), Some(0), "{set:?}");
    }

    let (mut app, _intents) = with_subagents();
    app.handle(Event::Resize(70, 24));
    press(&mut app, "up up enter");
    app.apply(Update::Opened {
        session: "c1".to_string(),
        lines: numbered(100),
    });
    press(&mut app, "k");
    assert_eq!(scroll(&app), Some(1));
    press(&mut app, "pgup");
    assert!(scroll(&app) > Some(1));
    assert!(find(&screen(&app), "esc close · j/k scroll").is_some());
}

/// A subagent shows only beside the tab it was opened from, while its parent is that tab's.
#[test]
fn leaving_or_clearing_the_tab_drops_its_subagent() {
    let mut app = session_app();
    press(&mut app, "j enter");
    assert_eq!(app.view, View::Subagent);
    enter_on_session(&mut app, "flaky");
    assert_eq!((app.active, app.subagent.as_ref()), (1, None));
    assert_eq!(app.view, View::Diff);

    press(&mut app, "g T j enter");
    assert_eq!(app.view, View::Subagent);
    press(&mut app, "i");
    type_text(&mut app, "/clear");
    press(&mut app, "enter");
    assert_eq!(app.subagent, None);
    assert_eq!(app.view, View::Diff);
}

/// Rows change live, so a selection follows its row rather than its position.
#[test]
fn a_running_selection_follows_its_row() {
    let (mut app, _intents) = with_subagents();
    let researcher = subagent("researcher", RunState::Working, "c1");
    let triage = subagent("triage", RunState::Done, "c2");
    let scout = subagent("scout", RunState::Working, "c0");
    press(&mut app, "up");
    assert_eq!(app.tabs[0].running_sel, Some(1));
    let rows = vec![scout.clone(), researcher.clone(), triage.clone()];
    app.apply(running("s1", rows));
    assert_eq!(app.tabs[0].running_sel, Some(2), "still triage");
}

#[test]
fn running_rows_follow_their_update() {
    let (mut app, _intents) = with_subagents();
    app.apply(running(
        "s1",
        vec![
            subagent("researcher", RunState::Working, "c1"),
            subagent("triage", RunState::Done, "c2"),
            subagent("scout", RunState::Failed, "c3"),
        ],
    ));
    let s = screen(&app);
    let drawn = buffer(&app);
    for (text, color) in [
        ("◆ researcher · researcher job", theme::ACCENT),
        ("◇ triage · triage job", theme::LABEL),
        ("✗ scout · scout job", theme::WARN),
    ] {
        let (row, column) = find(&s, text).expect(text);
        assert_eq!(drawn[(column, row)].fg, color, "{text}");
    }

    press(&mut app, "up");
    assert_eq!(app.tabs[0].running_sel, Some(2));
    app.apply(running(
        "s1",
        vec![subagent("researcher", RunState::Done, "c1")],
    ));
    assert_eq!(app.tabs[0].running_sel, Some(0));
    app.apply(running("s1", Vec::new()));
    assert_eq!(app.tabs[0].running_sel, None);
    assert!(app.tabs[0].running.is_empty());
}

#[test]
fn an_agents_own_reply_has_no_mark() {
    let s = screen(&session_app());
    assert_eq!(find(&s, "◆ Moving"), None);
    assert!(find(&s, "Moving them").is_some());
}

#[test]
fn enter_on_a_workflow_row_opens_workflows_on_that_row() {
    let mut app = session_app();
    press(&mut app, "j j j j enter");
    assert!(right_is_shown(&app));
    assert_eq!(app.view, View::Workflows);
    assert_eq!(app.tabs[0].running_sel, None);
    assert_eq!(app.focus, Pane::Sidebar);
    assert_eq!(app.tabs[0].agent, "Engineer");

    let mut narrow = App::new(Vim, 70, 24);
    narrow.open_session(session_row("pack registry"));
    press(&mut narrow, "j j j j enter");
    assert_eq!(
        narrow.layers.last().map(|top| top.kind),
        Some(LayerKind::Sidebar)
    );
    assert_eq!(narrow.view, View::Workflows);
    assert_eq!(narrow.tabs[0].running_sel, None);
}

#[test]
fn the_card_lists_each_file_with_counts() {
    let app = session_app();
    let rows = app.conversation_rows(80);
    let shown = [
        "M cli/args.rs   +3 -3",
        "M cli/mod.rs    +2 -1",
        "A shell/mod.rs  +3",
    ];
    for (file, shown) in shown.into_iter().enumerate() {
        let (card, _) = rows
            .iter()
            .find(|(_, tone)| *tone == Tone::File(file))
            .expect("a row per file");
        let row = app
            .view_rows(View::Diff)
            .into_iter()
            .find(|row| row.action == RowAction::OpenDiff(file))
            .expect("the same file in the Diff view");
        assert_eq!(card.trim(), shown);
        assert_eq!(row.text.trim(), shown);
    }
}

#[test]
fn file_rows_read_as_links() {
    let app = session_app();
    let (row, column) = find(&screen(&app), "M cli/args.rs").expect("the card");
    assert_eq!(buffer(&app)[(column, row)].fg, theme::ACCENT);
}

#[test]
fn show_file_opens_the_pane_on_that_diff_and_keeps_focus() {
    let mut app = session_app();
    app.show_file(1);
    assert!(right_is_shown(&app));
    assert_eq!(app.view, View::Diff);
    assert_eq!(app.diff, Some(1));
    assert_eq!(app.focus, Pane::Agent);
    let selected = app.view_sel[View::Diff as usize];
    assert_eq!(
        app.view_rows(View::Diff)[selected].action,
        RowAction::OpenDiff(1)
    );

    let mut narrow = App::new(Vim, 70, 24);
    narrow.open_session(session_row("pack registry"));
    narrow.show_file(2);
    assert_eq!(
        narrow.layers.last().map(|top| top.kind),
        Some(LayerKind::Sidebar)
    );
    assert_eq!(narrow.diff, Some(2));
    press(&mut narrow, "esc");
    assert_eq!(narrow.diff, None, "back returns to the file list first");
}

#[test]
fn emacs_reaches_rows_and_ctrl_g_backs_out() {
    let mut app = App::new(Emacs, 132, 36);
    enter_on_session(&mut app, "pack registry");
    press(&mut app, "up");
    assert_eq!(app.tabs[0].running_sel, Some(3));
    press(&mut app, "ctrl-p ctrl-p ctrl-p ctrl-p ctrl-p");
    assert_eq!(
        app.tabs[0].running_sel,
        Some(0),
        "emacs stays on the top row"
    );
    press(&mut app, "ctrl-g");
    assert_eq!(app.tabs[0].running_sel, None);
    press(&mut app, "up ctrl-n");
    assert_eq!(
        app.tabs[0].running_sel, None,
        "down past the end is the box"
    );
}

#[test]
fn opening_a_subagent_clears_a_search() {
    let mut app = session_app();
    press(&mut app, "/");
    type_text(&mut app, "Moving");
    press(&mut app, "enter ctrl-w j enter");
    assert!(app.search.is_none());
    assert_eq!(app.view, View::Subagent);
}

#[test]
fn rows_draw_at_every_breakpoint() {
    for (width, height) in [
        (168, 45),
        (132, 36),
        (104, 30),
        (90, 24),
        (70, 24),
        (60, 16),
    ] {
        let mut app = App::new(Vim, width, height);
        app.open_session(session_row("pack registry"));
        let s = screen(&app);
        assert!(find(&s, "▸ change unit").is_some(), "{width}x{height}");
        assert!(find(&s, "reading crates").is_some(), "{width}x{height}");
    }
}

#[test]
fn the_hint_names_the_way_in() {
    let mut app = session_app();
    assert!(find(&screen(&app), "j select").is_some());
    press(&mut app, "i");
    assert!(find(&screen(&app), "↑ select").is_some());
    type_text(&mut app, "x");
    assert_eq!(find(&screen(&app), "↑ select"), None);
    press(&mut app, "bs up");
    assert_eq!(find(&screen(&app), "select"), None);
    let mut narrow = App::new(Vim, 60, 16);
    narrow.open_session(session_row("pack registry"));
    assert_eq!(find(&screen(&narrow), "select"), None);
}

#[test]
fn short_keys_move_focus_and_toggle() {
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    press(&mut app, "esc h");
    assert_eq!(app.focus, Pane::Sessions);
    press(&mut app, "l l");
    assert_eq!(app.focus, Pane::Sidebar);
    press(&mut app, "h");
    assert_eq!(app.focus, Pane::Agent);
    press(&mut app, "[ ]");
    assert_eq!(
        (app.wanted().sessions, app.wanted().sidebar),
        (false, false)
    );
    press(&mut app, "[ ]");
    assert_eq!((app.wanted().sessions, app.wanted().sidebar), (true, true));
}

#[test]
fn short_keys_are_text_while_typing() {
    for set in [Vim, Emacs] {
        let mut app = with_right_pane(App::new(set, 132, 36));
        type_text(&mut app, "hl[]");
        assert_eq!(app.tabs[0].input.text(), "hl[]");
        assert_eq!(app.focus, Pane::Agent);
        assert!(app.wanted().sidebar);
    }
}

#[test]
fn bracket_closes_the_floating_sidebar() {
    let mut app = App::new(Vim, 70, 24);
    press(&mut app, "esc ]");
    assert_eq!(
        app.layers.last().map(|top| top.kind),
        Some(LayerKind::Sidebar)
    );
    press(&mut app, "]");
    assert!(app.layers.is_empty());
    press(&mut app, "[");
    assert_eq!(
        app.layers.last().map(|top| top.kind),
        Some(LayerKind::Sessions)
    );
    press(&mut app, "[");
    assert!(app.layers.is_empty());
}

#[test]
fn ctrl_p_opens_the_bar_from_normal_and_input() {
    for keys in ["ctrl-p", "esc ctrl-p"] {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, keys);
        assert_eq!(app.layers.last().map(|top| top.kind), Some(LayerKind::Jump));
        assert_eq!(app.chip(), "JUMP");
    }
}

#[test]
fn ctrl_p_moves_up_inside_the_slash_list() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/");
    press(&mut app, "down ctrl-p");
    assert_eq!(app.slash, Some(0));
    assert!(app.layers.is_empty());
}

#[test]
fn ctrl_p_still_moves_up_in_emacs() {
    let mut app = App::new(Emacs, 132, 36);
    app.tabs[0].lines = numbered(100);
    press(&mut app, "ctrl-p");
    assert!(app.layers.is_empty());
    assert_eq!(app.tabs[0].scroll, 1);
}

#[test]
fn the_keys_overlay_shows_the_short_keys() {
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc ?");
    let s = screen(&app);
    for text in [
        "h / l, ctrl-w h/j/k/l",
        "[ or ctrl-w [",
        "] or ctrl-w ]",
        ": / ctrl-p",
    ] {
        assert!(find(&s, text).is_some(), "{text}");
    }
}

fn right_is_shown(app: &App) -> bool {
    !matches!(app.panes().map(|panes| panes.right), Some(Right::None))
}

#[test]
fn the_right_pane_starts_closed_at_every_width() {
    for (width, height) in [
        (168, 45),
        (132, 36),
        (120, 30),
        (104, 30),
        (90, 24),
        (70, 24),
    ] {
        let app = App::new(Vim, width, height);
        assert!(!right_is_shown(&app), "{width}");
        let s = screen(&app);
        assert_eq!(find(&s, "Workflows"), None, "{width}");
        assert_eq!(find(&s, "+58"), None, "{width}");
    }
}

#[test]
fn the_agent_pane_takes_the_room_of_a_closed_right_pane() {
    let app = App::new(Vim, 132, 36);
    let panes = app.panes().expect("panes");
    assert_eq!((panes.agent.x, panes.agent.width), (28, 104));
}

#[test]
fn each_tab_remembers_its_right_pane() {
    let mut app = App::new(Vim, 132, 36);
    enter_on_session(&mut app, "pack registry");
    press(&mut app, "]");
    assert!(right_is_shown(&app));
    enter_on_session(&mut app, "flaky");
    assert_eq!(app.active, 1);
    assert!(!right_is_shown(&app), "a new tab starts closed");
    press(&mut app, "g T");
    assert_eq!(app.active, 0);
    assert!(right_is_shown(&app), "the first tab kept it open");
    press(&mut app, "g t");
    assert!(!right_is_shown(&app), "the second tab kept it closed");
}

#[test]
fn switching_tabs_closes_a_diff_and_moves_focus_from_the_pane() {
    let mut app = App::new(Vim, 132, 36);
    enter_on_session(&mut app, "pack registry");
    enter_on_session(&mut app, "flaky");
    press(&mut app, "g T ] l");
    assert_eq!(app.focus, Pane::Sidebar);
    press(&mut app, "enter");
    assert!(app.diff.is_some());
    press(&mut app, "g t");
    assert_eq!(app.diff, None);
    assert_eq!(app.focus, Pane::Agent);
}

#[test]
fn slash_diff_opens_the_diff_view_on_the_session() {
    let mut app = App::new(Vim, 132, 36);
    app.scope = Scope::Tree;
    type_text(&mut app, "/diff");
    press(&mut app, "enter");
    assert!(right_is_shown(&app));
    assert_eq!(app.view, View::Diff);
    assert_eq!(app.scope, Scope::Session);
    let s = screen(&app);
    assert_eq!(find(&s, " Diff "), Some((0, 100)));
    let scopes: String = s[1].chars().skip(100).collect();
    assert!(scopes.starts_with(" session   working tree "), "{scopes}");
    let buf = buffer(&app);
    assert_eq!(buf[(101, 1)].bg, theme::SEL_BG);
    assert_eq!(buf[(111, 1)].bg, theme::TAB_BG);

    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/changes");
    assert!(
        app.slash_matches().is_empty(),
        "changes is only a bar alias"
    );
}

#[test]
fn the_bar_finds_the_working_tree_by_git() {
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc :");
    type_text(&mut app, "git");
    press(&mut app, "enter");
    assert_eq!(app.view, View::Diff);
    assert_eq!(app.scope, Scope::Tree);
}

#[test]
fn slash_diff_and_workflows_open_the_pane() {
    for (name, view) in [("diff", View::Diff), ("workflows", View::Workflows)] {
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, &format!("/{name}"));
        press(&mut app, "enter");
        assert!(right_is_shown(&app), "{name}");
        assert_eq!(app.view, view);

        let mut narrow = App::new(Vim, 70, 24);
        type_text(&mut narrow, &format!("/{name}"));
        press(&mut narrow, "enter");
        assert_eq!(
            narrow.layers.last().map(|top| top.kind),
            Some(LayerKind::Sidebar)
        );
    }
}

#[test]
fn the_bar_finds_the_diff_view_by_changes() {
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc :");
    type_text(&mut app, "changes");
    press(&mut app, "enter");
    assert!(right_is_shown(&app));
    assert_eq!(app.view, View::Diff);
}

#[test]
fn emacs_up_in_a_list_moves_the_list_not_the_rows() {
    let mut app = App::new(Emacs, 132, 36);
    enter_on_session(&mut app, "pack registry");
    press(&mut app, "ctrl-x o");
    assert_eq!(app.focus, Pane::Sessions);
    let before = app.sessions_sel;
    press(&mut app, "up");
    assert_eq!(app.tabs[0].running_sel, None);
    assert_ne!(app.sessions_sel, before);

    let mut app = App::new(Emacs, 132, 36);
    enter_on_session(&mut app, "pack registry");
    press(&mut app, "ctrl-x ] ctrl-x o");
    assert_eq!(app.focus, Pane::Sidebar);
    let before = app.view_sel[View::Diff as usize];
    press(&mut app, "ctrl-n ctrl-p");
    assert_eq!(app.tabs[0].running_sel, None);
    assert_eq!(app.view_sel[View::Diff as usize], before);
}

#[test]
fn the_hint_shows_only_where_its_key_works() {
    let mut app = session_app();
    press(&mut app, "h");
    assert_eq!(find(&screen(&app), "select"), None, "sessions focused");
    press(&mut app, "l");
    app.tabs[0].lines = numbered(100);
    press(&mut app, "k");
    assert_eq!(find(&screen(&app), "select"), None, "scrolled up");
    press(&mut app, "j");
    assert!(find(&screen(&app), "j select").is_some());
}

#[test]
fn clear_on_a_session_tab_shows_a_fresh_tab() {
    for (width, height) in [(132, 36), (60, 16)] {
        let mut app = App::new(Vim, width, height);
        app.open_session(session_row("pack registry"));
        app.run_effect(Effect::ClearTab);
        let s = screen(&app);
        assert!(find(&s, "start typing").is_some(), "{width}");
        assert_eq!(find(&s, "◆ Reviewer · auth"), None, "{width}");
    }
}

fn cleared() -> (App, std::sync::mpsc::Receiver<Intent>) {
    let (mut app, intents) = connected();
    let tab = &mut app.tabs[0];
    tab.session = Some("s1".to_string());
    tab.name = "fix login".to_string();
    tab.agent_id = "local:reviewer".to_string();
    tab.lines = numbered(3);
    tab.turn = Some("t1".to_string());
    tab.following = true;
    tab.prompts = vec![crate::sample::approval("Engineer")];
    app.apply(running("s1", vec![subagent("a", RunState::Working, "c1")]));
    (app, intents)
}

#[test]
fn usage_opens_the_limits_menu_and_reads_it() {
    let (mut app, intents) = connected();
    intents.try_iter().for_each(drop);
    type_text(&mut app, "/usage");
    press(&mut app, "enter");
    assert_eq!(top_layer(&app), LayerKind::Menu(MenuId::Usage));
    assert_eq!(sent(&intents), [Intent::ReadMenu(MenuId::Usage)]);
}

#[test]
fn clear_starts_a_fresh_session_with_the_same_agent() {
    let (mut app, intents) = cleared();
    let tabs = app.tabs.len();
    type_text(&mut app, "/clear");
    press(&mut app, "enter");
    let tab = &app.tabs[0];
    assert_eq!(tab.session, None);
    assert_eq!(tab.name, "new session");
    assert!(tab.lines.is_empty() && tab.running.is_empty() && tab.prompts.is_empty());
    assert!(tab.turn.is_none() && !tab.following);
    assert_eq!(tab.agent_id, "local:reviewer");
    assert_eq!(app.tabs.len(), tabs);
    type_text(&mut app, "hi");
    press(&mut app, "enter");
    let sent = sent(&intents);
    assert!(
        matches!(&sent[..], [Intent::Send { session: None, agent, .. }] if agent == "local:reviewer"),
        "{sent:?}"
    );
}

#[test]
fn a_late_update_for_the_old_session_stays_out() {
    let (mut app, _intents) = cleared();
    type_text(&mut app, "/clear");
    press(&mut app, "enter");
    app.apply(item("s1", said("late", Tone::Reply, "r9")));
    app.apply(ended("t1"));
    assert!(app.tabs[0].lines.is_empty());
}

#[test]
fn clear_waits_for_a_starting_session() {
    let (mut app, _intents) = cleared();
    app.tabs[0].starting = true;
    type_text(&mut app, "/clear");
    press(&mut app, "enter");
    assert_eq!(app.notice.as_deref(), Some(STARTING));
    assert_eq!(app.tabs[0].lines.len(), 3);
}

#[test]
fn page_keys_scroll_the_conversation_past_the_rows() {
    for set in [Vim, Emacs] {
        let mut app = App::new(set, 132, 36);
        enter_on_session(&mut app, "pack registry");
        app.tabs[0].lines = numbered(100);
        app.tabs[0].prompts.clear();
        press(&mut app, "pgup");
        assert_eq!(app.tabs[0].scroll, 23, "{set:?}");
        assert_eq!(app.tabs[0].running_sel, None);
        press(&mut app, "pgup pgup pgup pgup pgup");
        assert_eq!(app.tabs[0].scroll, 77, "stops at the top");
        press(&mut app, "pgdn");
        assert_eq!(app.tabs[0].scroll, 54);
    }
}

#[test]
fn typing_while_a_row_is_selected_goes_to_the_box_in_emacs() {
    let mut app = App::new(Emacs, 132, 36);
    enter_on_session(&mut app, "pack registry");
    press(&mut app, "up");
    assert_eq!(app.tabs[0].running_sel, Some(3));
    type_text(&mut app, "hi");
    assert_eq!(app.tabs[0].input.text(), "hi");
    assert_eq!(app.tabs[0].running_sel, None);
}

#[test]
fn i_on_a_row_starts_typing_in_vim() {
    let mut app = session_app();
    press(&mut app, "j i");
    assert_eq!(app.tabs[0].running_sel, None);
    assert_eq!(app.ctx(), KeyCtx::Input);
}

fn enter_on_session(app: &mut App, text: &str) {
    app.focus = Pane::Sessions;
    app.sessions_sel = session_row(text);
    press(app, "enter");
}

#[test]
fn ctrl_w_moves_between_docked_panes() {
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    press(&mut app, "esc");
    press(&mut app, "ctrl-w h");
    assert_eq!(app.focus, Pane::Sessions);
    press(&mut app, "ctrl-w h");
    assert_eq!(app.focus, Pane::Sessions);
    press(&mut app, "ctrl-w l");
    assert_eq!(app.focus, Pane::Agent);
    press(&mut app, "ctrl-w l");
    assert_eq!(app.focus, Pane::Sidebar);
    press(&mut app, "ctrl-w l");
    assert_eq!(app.focus, Pane::Sidebar);
    press(&mut app, "ctrl-w j ctrl-w k");
    assert_eq!(app.focus, Pane::Sidebar);
    press(&mut app, "ctrl-w h");
    assert_eq!(app.focus, Pane::Agent);
}

#[test]
fn a_pane_other_than_the_agents_leaves_input_mode() {
    for pane in [Pane::Sessions, Pane::Sidebar] {
        let mut app = App::new(Vim, 132, 36);
        app.focus_pane(pane);
        assert_eq!(app.mode, Mode::Normal, "{pane:?}");
    }
}

#[test]
fn ctrl_w_with_ctrl_still_held_moves_the_same_way() {
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    press(&mut app, "esc ctrl-w ctrl-h");
    assert_eq!(app.focus, Pane::Sessions);
    press(&mut app, "ctrl-w ctrl-l ctrl-w ctrl-l");
    assert_eq!(app.focus, Pane::Sidebar);
}

#[test]
fn ctrl_w_while_writing_deletes_a_word_and_leaves_the_tab_alone() {
    let mut app = App::new(Vim, 132, 36);
    app.tabs.push(Tab::new_session());
    type_text(&mut app, "draft words");
    press(&mut app, "ctrl-w q");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs[0].input.text(), "draft q");
    assert_eq!(app.focus, Pane::Agent);
    assert_eq!(app.mode, Mode::Input);

    press(&mut app, "ctrl-w ctrl-w");
    type_text(&mut app, "/cl arg");
    press(&mut app, "ctrl-w");
    assert_eq!(app.tabs[0].input.text(), "/cl ");
    assert_eq!(app.slash, Some(0));
    press(&mut app, "ctrl-w");
    assert!(app.tabs[0].input.is_empty());
    assert_eq!(app.slash, None);
}

#[test]
fn ctrl_a_and_ctrl_e_reach_the_ends_of_the_line_in_emacs() {
    let mut app = App::new(Emacs, 132, 36);
    type_text(&mut app, "ac");
    press(&mut app, "ctrl-a");
    type_text(&mut app, "x");
    press(&mut app, "ctrl-e");
    type_text(&mut app, "y");
    assert_eq!(app.tabs[0].input.text(), "xacy");

    app.push_layer(LayerKind::Picker);
    type_text(&mut app, "ac");
    press(&mut app, "ctrl-a");
    type_text(&mut app, "x");
    press(&mut app, "ctrl-e");
    type_text(&mut app, "y");
    assert_eq!(app.layers[0].filter.text(), "xacy");
    assert_eq!(app.tabs[0].input.text(), "xacy");
}

#[test]
fn ctrl_x_o_cycles_visible_panes() {
    let mut app = with_right_pane(App::new(Emacs, 132, 36));
    for pane in [Pane::Sidebar, Pane::Sessions, Pane::Agent] {
        press(&mut app, "ctrl-x o");
        assert_eq!(app.focus, pane);
    }
    let mut app = with_right_pane(App::new(Emacs, 104, 30));
    for pane in [Pane::Sessions, Pane::Agent] {
        press(&mut app, "ctrl-x o");
        assert_eq!(app.focus, pane);
    }
    let mut app = with_right_pane(App::new(Emacs, 70, 24));
    press(&mut app, "ctrl-x o");
    assert_eq!(app.focus, Pane::Agent);
}

#[test]
fn i_and_printable_keys_reach_the_message_box() {
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc");
    app.focus = Pane::Sidebar;
    press(&mut app, "i");
    assert_eq!(app.focus, Pane::Agent);
    assert_eq!(app.mode, Mode::Input);

    let mut app = App::new(Emacs, 132, 36);
    app.focus = Pane::Sessions;
    type_text(&mut app, "x");
    assert_eq!(app.focus, Pane::Agent);
    assert_eq!(app.tabs[0].input.text(), "x");
}

#[test]
fn rows_skip_non_selectable_and_clamp() {
    let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
    press(&mut app, "esc ctrl-w h");
    assert_eq!(app.sessions_sel, session_row("cli ux redesign"));
    for text in ["flaky", "pack registry", "eval baseline"] {
        press(&mut app, "j");
        assert_eq!(app.sessions_sel, session_row(text));
    }
    for _ in 0..20 {
        press(&mut app, "j");
    }
    assert_eq!(app.sessions_sel, app.session_rows().len() - 1);
    for _ in 0..20 {
        press(&mut app, "k");
    }
    assert_eq!(app.sessions_sel, session_row("+ New session"));

    press(&mut app, "ctrl-w l ctrl-w l");
    let files = app.view_sel[View::Diff as usize];
    press(&mut app, "j");
    assert_eq!(app.view_sel[View::Diff as usize], files + 1);
    press(&mut app, "k");
    assert_eq!(app.view_sel[View::Diff as usize], files);
}

#[test]
fn opening_a_session_reuses_an_empty_tab_and_never_duplicates() {
    let mut app = App::new(Vim, 132, 36);
    enter_on_session(&mut app, "flaky");
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].name, "flaky");
    assert_eq!(app.tabs[0].agent, "Reviewer");
    assert_eq!(app.focus, Pane::Agent);
    assert_eq!(app.mode, Mode::Normal);
    assert!(find(&screen(&app), "Group the sign-in").is_some());
    assert!(find(&screen(&app), "! Allow Reviewer").is_some());

    enter_on_session(&mut app, "cli ux");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, 1);
    enter_on_session(&mut app, "flaky");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, 0);
}

#[test]
fn a_tab_with_messages_is_not_reused() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "hi");
    press(&mut app, "enter");
    enter_on_session(&mut app, "flaky");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs[0].name, "new session");
}

#[test]
fn tabs_cycle_and_apply_the_mode_rule() {
    let mut app = App::new(Vim, 132, 36);
    enter_on_session(&mut app, "flaky");
    app.tabs.push(Tab::new_session());
    press(&mut app, "g t");
    assert_eq!(app.active, 1);
    assert_eq!(app.mode, Mode::Input);
    press(&mut app, "esc g t");
    assert_eq!(app.active, 0);
    assert_eq!(app.mode, Mode::Normal);
    press(&mut app, "g T");
    assert_eq!(app.active, 1);
    assert_eq!(app.mode, Mode::Input);
    press(&mut app, "esc g T");
    assert_eq!(app.active, 0);
    assert_eq!(app.mode, Mode::Normal);

    let mut app = App::new(Emacs, 132, 36);
    app.tabs.push(Tab::new_session());
    press(&mut app, "ctrl-x n");
    assert_eq!(app.active, 1);
    press(&mut app, "ctrl-x n");
    assert_eq!(app.active, 0);
    press(&mut app, "ctrl-x p");
    assert_eq!(app.active, 1);
}

#[test]
fn switching_tabs_from_a_sidebar_moves_focus_to_the_agent_pane() {
    let mut app = App::new(Vim, 132, 36);
    app.tabs.push(Tab::new_session());
    press(&mut app, "esc ctrl-w h g t");
    assert_eq!(app.focus, Pane::Agent);
    assert_eq!(app.mode, Mode::Input);
}

#[test]
fn closing() {
    let mut app = App::new(Vim, 132, 36);
    enter_on_session(&mut app, "flaky");
    enter_on_session(&mut app, "cli ux");
    enter_on_session(&mut app, "pack registry");
    app.activate_tab(1);
    press(&mut app, "ctrl-w q");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, 0);
    assert_eq!(app.tabs[0].name, "flaky");
    assert_eq!(app.tabs[1].name, "packs");
    assert_eq!(app.mode, Mode::Normal);

    press(&mut app, "ctrl-w q");
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active, 0);
    assert_eq!(app.tabs[0].name, "packs");

    press(&mut app, "ctrl-w q");
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].name, "new session");
    assert_eq!(app.tabs[0].agent, "Engineer");
    assert!(app.tabs[0].lines.is_empty());
    assert_eq!(app.mode, Mode::Input);

    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc ctrl-w h ctrl-w q");
    assert!(!app.wanted().sessions);
    assert_eq!(app.focus, Pane::Agent);
    assert_eq!(app.tabs.len(), 1);

    let mut app = App::new(Emacs, 132, 36);
    app.focus = Pane::Sidebar;
    press(&mut app, "ctrl-x 0");
    assert!(!app.wanted().sidebar);
    assert_eq!(app.focus, Pane::Agent);
}

#[test]
fn docked_toggles_flip_wanted() {
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    let edge = find(&screen(&app), "enter send").map(|(_, column)| column);
    press(&mut app, "esc ctrl-w ]");
    assert!(!app.wanted().sidebar);
    let s = screen(&app);
    assert_eq!(find(&s, "Diff"), None);
    assert!(find(&s, "enter send").map(|(_, column)| column) > edge);

    press(&mut app, "ctrl-w ]");
    assert!(app.wanted().sidebar);
    assert!(find(&screen(&app), "Diff").is_some());

    let mut app = with_right_pane(App::new(Emacs, 132, 36));
    press(&mut app, "ctrl-x [");
    assert!(!app.wanted().sessions);
    assert_eq!(find(&screen(&app), "Sessions"), None);
    press(&mut app, "ctrl-x [");
    assert!(app.wanted().sessions);
    assert!(find(&screen(&app), "Sessions").is_some());
}

#[test]
fn toggling_a_focused_sidebar_off_moves_focus_to_the_agent_pane() {
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc ctrl-w l ctrl-w ]");
    assert_eq!(app.focus, Pane::Agent);
}

#[test]
fn rail_shows_only_while_wanted() {
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    press(&mut app, "esc ctrl-w ]");
    app.handle(Event::Resize(104, 30));
    assert_eq!(find(&screen(&app), "+0"), None);

    app.handle(Event::Resize(132, 36));
    press(&mut app, "ctrl-w ]");
    app.handle(Event::Resize(104, 30));
    assert!(find(&screen(&app), "+0").is_some());
}

#[test]
fn focus_falls_to_agent_when_its_pane_disappears() {
    let mut app = App::new(Vim, 132, 36);
    app.focus = Pane::Sidebar;
    app.handle(Event::Resize(104, 30));
    assert_eq!(app.focus, Pane::Agent);

    let mut app = App::new(Vim, 132, 36);
    app.focus = Pane::Sessions;
    app.handle(Event::Resize(70, 24));
    assert_eq!(app.focus, Pane::Agent);
}

#[test]
fn focus_survives_the_too_small_screen() {
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    press(&mut app, "esc ctrl-w l ctrl-w l");
    assert_eq!(app.focus, Pane::Sidebar);
    app.handle(Event::Resize(40, 10));
    assert_eq!(app.focus, Pane::Sidebar);
    app.handle(Event::Resize(132, 36));
    assert_eq!(app.focus, Pane::Sidebar);
    app.handle(Event::Resize(40, 10));
    app.handle(Event::Resize(104, 30));
    assert_eq!(app.focus, Pane::Agent);
}

#[test]
fn file_opens_its_diff_and_back_returns() {
    let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
    press(&mut app, "esc ctrl-w l ctrl-w l enter");
    assert_eq!(app.diff, Some(2), "the newest file lists first");
    let s = screen(&app);
    assert!(find(&s, "+pub fn run() {").is_some());
    assert!(find(&s, "esc back to files").is_some());
    assert_eq!(find(&s, "next hunk"), None);
    assert_eq!(find(&s, "3 files   +"), None);

    press(&mut app, "esc");
    assert_eq!(app.diff, None);
    assert!(find(&screen(&app), "3 files   +").is_some());

    let mut app = with_right_pane(with_sample_session(App::new(Emacs, 132, 36)));
    press(&mut app, "ctrl-x o enter");
    let s = screen(&app);
    assert!(find(&s, "ctrl-g back to files").is_some());
    assert_eq!(find(&s, "esc back"), None);
    press(&mut app, "ctrl-g");
    assert!(find(&screen(&app), "3 files   +").is_some());
}

#[test]
fn changing_the_view_leaves_the_diff() {
    let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
    press(&mut app, "esc ctrl-w l ctrl-w l enter");
    assert_eq!(app.diff, Some(2));
    app.set_view(View::Workflows);
    assert_eq!(app.diff, None);
    assert!(find(&screen(&app), "2 running").is_some());
}

#[test]
fn a_tall_diff_scrolls_to_its_last_row_at_the_smallest_height() {
    let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 16)));
    press(&mut app, "esc ctrl-w l ctrl-w l j j enter");
    let s = screen(&app);
    assert!(find(&s, "M cli/args.rs  +3 -3").is_some());
    assert_eq!(find(&s, "esc back to files"), None);

    press(&mut app, "j j j");
    let s = screen(&app);
    assert!(find(&s, "esc back to files").is_some());
    assert_eq!(find(&s, "M cli/args.rs  +3 -3"), None);

    press(&mut app, "j j j k k k");
    assert!(find(&screen(&app), "M cli/args.rs  +3 -3").is_some());
}

#[test]
fn a_fresh_diff_starts_at_its_top() {
    let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 16)));
    press(&mut app, "esc ctrl-w l ctrl-w l j j enter j esc enter");
    assert!(find(&screen(&app), "M cli/args.rs  +3 -3").is_some());
}

#[test]
fn slash_opens_only_as_the_first_character() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/");
    assert_eq!(app.slash, Some(0));
    let s = screen(&app);
    assert_eq!(find(&s, " Commands "), Some((18, 32)));
    assert_eq!(
        find(&s, " /agent     open an agent in a new tab"),
        Some((19, 31))
    );
    assert!(find(&s, "/quit").is_some());
    assert_eq!(find(&s, "╭"), Some((31, 30)));
    let buf = buffer(&app);
    assert_eq!(buf[(31, 19)].bg, theme::SEL_BG);
    assert_eq!(buf[(94, 19)].bg, theme::SEL_BG);
    assert_ne!(buf[(31, 20)].bg, theme::SEL_BG);
    assert_eq!(buf[(32, 19)].fg, theme::SEL_FG);
    assert_ne!(buf[(32, 20)].fg, theme::DIM);

    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "a/");
    assert_eq!(app.slash, None);
    press(&mut app, "enter");
    assert_eq!(app.tabs[0].lines[0].text, "a/");
}

#[test]
fn filters_completes_and_runs() {
    let mut app = App::new(Vim, 132, 36);
    app.tabs[0].lines = numbered(3);
    type_text(&mut app, "/cl");
    let s = screen(&app);
    assert!(find(&s, "/clear").is_some());
    assert_eq!(find(&s, "/agent"), None);
    press(&mut app, "tab");
    assert_eq!(app.tabs[0].input.text(), "/clear");
    assert_eq!(app.slash, Some(0));
    press(&mut app, "enter");
    assert!(app.tabs[0].lines.is_empty());
    assert!(app.tabs[0].input.is_empty());
    assert_eq!(app.slash, None);
}

#[test]
fn next_and_prev_choose_what_runs() {
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    type_text(&mut app, "/");
    press(&mut app, "ctrl-n down");
    assert_eq!(app.slash, Some(2));
    press(&mut app, "ctrl-p");
    assert_eq!(app.slash, Some(1));
    press(&mut app, "tab");
    assert_eq!(app.tabs[0].input.text(), "/new");

    let mut app = with_right_pane(App::new(Vim, 132, 36));
    type_text(&mut app, "/");
    for _ in 0..20 {
        press(&mut app, "ctrl-n");
    }
    assert_eq!(app.slash, Some(10));
    type_text(&mut app, "c");
    assert_eq!(app.slash, Some(0));
    press(&mut app, "enter");
    assert!(!app.should_quit());
}

#[test]
fn the_list_closes_without_losing_the_text() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/c");
    press(&mut app, "esc");
    assert_eq!(app.slash, None);
    assert_eq!(app.tabs[0].input.text(), "/c");
    assert_eq!(app.mode, Mode::Input);
    assert_eq!(find(&screen(&app), " Commands "), None);
    press(&mut app, "esc");
    assert_eq!(app.mode, Mode::Normal);

    let mut app = App::new(Emacs, 132, 36);
    type_text(&mut app, "/c");
    press(&mut app, "ctrl-g");
    assert_eq!(app.slash, None);
    assert_eq!(app.tabs[0].input.text(), "/c");
    type_text(&mut app, "x");
    assert_eq!(app.slash, None);
    press(&mut app, "enter");
    assert_eq!(app.tabs[0].lines[0].text, "/cx");
    assert!(app.tabs[0].input.is_empty());

    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/");
    press(&mut app, "bs");
    assert_eq!(app.slash, None);
    assert!(app.tabs[0].input.is_empty());
    assert_eq!(find(&screen(&app), " Commands "), None);
    type_text(&mut app, "/c");
    press(&mut app, "esc bs");
    assert_eq!(app.tabs[0].input.text(), "/");
    assert_eq!(app.slash, None);

    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/c");
    press(&mut app, "home");
    assert_eq!(app.slash, Some(0));
    type_text(&mut app, "a");
    assert_eq!(app.tabs[0].input.text(), "a/c");
    assert_eq!(app.slash, None);
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/c");
    press(&mut app, "home");
    app.handle(Event::Paste("a".to_string()));
    assert_eq!(app.tabs[0].input.text(), "a/c");
    assert_eq!(app.slash, None);
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/c");
    press(&mut app, "ctrl-c");
    assert!(app.tabs[0].input.is_empty());
    assert_eq!(app.slash, None);

    let mut app = App::new(Emacs, 132, 36);
    app.tabs.push(Tab::new_session());
    type_text(&mut app, "/");
    press(&mut app, "ctrl-x n");
    assert_eq!(app.slash, None);

    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/zzz");
    assert!(find(&screen(&app), "no matches").is_some());
    press(&mut app, "enter");
    assert_eq!(app.notice.as_deref(), Some("unknown command: /zzz"));
    assert_eq!(app.tabs[0].input.text(), "/zzz");
    assert!(app.tabs[0].lines.is_empty());
}

#[test]
fn each_command_does_what_the_table_says() {
    let run = |width, height, text: &str| {
        let mut app = App::new(Vim, width, height);
        app.tabs[app.active].sidebar = false;
        app.tabs[0].lines = numbered(3);
        app.mode = Mode::Input;
        type_text(&mut app, text);
        press(&mut app, "enter");
        assert!(app.tabs[0].input.is_empty(), "{text}");
        app
    };
    let top = |app: &App| app.layers.last().map(|layer| layer.kind);

    assert_eq!(top(&run(132, 36, "/agent")), Some(LayerKind::Picker));
    assert_eq!(top(&run(132, 36, "/new")), Some(LayerKind::Picker));
    assert!(run(132, 36, "/clear").tabs[0].lines.is_empty());

    let jobs = run(132, 36, "/jobs");
    assert_eq!(jobs.view, View::Agents);
    assert!(jobs.wanted().sidebar);
    assert!(jobs.layers.is_empty());
    let jobs = run(70, 24, "/jobs");
    assert_eq!(jobs.view, View::Agents);
    assert_eq!(top(&jobs), Some(LayerKind::Sidebar));

    assert_eq!(top(&run(132, 36, "/help")), Some(LayerKind::Keys));
    assert!(run(132, 36, "/quit").should_quit());

    let mut goal = App::new(Vim, 132, 36);
    type_text(&mut goal, "/goal");
    press(&mut goal, "enter");
    assert_eq!(goal.tabs[0].input.text(), "/goal ");
}

#[test]
fn emacs_slash_from_sessions() {
    let mut app = App::new(Emacs, 132, 36);
    app.focus = Pane::Sessions;
    type_text(&mut app, "/");
    assert_eq!(app.focus, Pane::Agent);
    assert_eq!(app.slash, Some(0));
}

#[test]
fn every_command_reachable_at_60_by_16() {
    let mut app = App::new(Vim, 60, 16);
    type_text(&mut app, "/");
    let s = screen(&app);
    assert_eq!(find(&s, " Commands "), Some((3, 4)));
    assert_eq!(find(&s, "/quit"), None);
    for _ in 0..7 {
        press(&mut app, "ctrl-n");
    }
    let s = screen(&app);
    let (row, column) = find(&s, "/quit").expect("the last command");
    assert_eq!(buffer(&app)[(column, row)].bg, theme::SEL_BG);
    assert_eq!(
        find(&s, "tab complete · enter run · esc close").map(|(r, _)| r),
        Some(8)
    );
    assert_eq!(buffer(&app)[(0, 3)].symbol(), "│");
}

#[test]
fn switch_key_set_is_not_a_slash_command() {
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    type_text(&mut app, "/");
    assert_eq!(app.slash_matches().len(), 11);
    type_text(&mut app, "switch");
    assert!(app.slash_matches().is_empty());
    assert!(find(&screen(&app), "no matches").is_some());
}

#[test]
fn the_list_has_its_own_hints_and_footer_in_each_set() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "/");
    let s = screen(&app);
    let hint = "/ at the start of a message opens commands · esc closes the list";
    assert_eq!(find(&s, hint).map(|(row, _)| row), Some(35));
    assert_eq!(find(&s, "INPUT"), Some((35, 1)));
    assert!(find(&s, "tab complete · enter run · esc close").is_some());

    let mut app = App::new(Emacs, 132, 36);
    type_text(&mut app, "/");
    let s = screen(&app);
    let hint = "tab complete · enter run · ctrl-g close";
    assert_eq!(find(&s[35..], hint).map(|(row, _)| row), Some(0));
    assert_eq!(find(&s, hint).map(|(row, _)| row), Some(28));
    assert_eq!(find(&s, "EMACS"), Some((35, 1)));
}

#[test]
fn tab_with_no_list_does_nothing() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "hi");
    press(&mut app, "tab");
    assert_eq!(app.tabs[0].input.text(), "hi");
    assert_eq!(app.slash, None);
}

#[test]
fn a_session_never_takes_over_a_tab_with_a_picked_agent() {
    let mut app = App::new(Vim, 132, 36);
    app.open_tab_with_agent(1);
    enter_on_session(&mut app, "cli ux");
    assert_eq!(app.tabs.len(), 3);
    assert_eq!(app.active, 2);
    assert_eq!(app.tabs[1].name, "new session");
    assert_eq!(app.tabs[1].agent, "Reviewer");
    assert_eq!(app.tabs[2].agent, "Engineer");
}

#[test]
fn a_session_never_takes_over_a_tab_with_a_draft() {
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "hi");
    enter_on_session(&mut app, "flaky");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, 1);
    assert_eq!(app.tabs[0].name, "new session");
    assert_eq!(app.tabs[0].agent, "Engineer");
    assert_eq!(app.tabs[0].input.text(), "hi");
    assert!(app.tabs[1].input.is_empty());
}

#[test]
fn esc_and_a_named_key_in_one_read_act_as_two_keys() {
    let alt_enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc ctrl-w h");
    app.sessions_sel = session_row("flaky");
    app.handle(alt_enter.clone());
    assert_eq!(app.tabs[0].name, "flaky");

    // While typing, `esc enter` in one read is taken as alt-enter, a line break.
    let mut app = App::new(Vim, 132, 36);
    type_text(&mut app, "hi");
    app.handle(alt_enter);
    assert_eq!(app.mode, Mode::Input);
    assert_eq!(app.tabs[0].input.text(), "hi\n");
    assert!(app.tabs[0].lines.is_empty());
}

#[test]
fn a_lone_tab_does_not_switch_to_itself() {
    let mut app = App::new(Vim, 132, 36);
    press(&mut app, "esc g t");
    assert_eq!(app.mode, Mode::Normal);
    press(&mut app, "g T");
    assert_eq!(app.mode, Mode::Normal);

    let mut app = App::new(Emacs, 132, 36);
    app.focus = Pane::Sidebar;
    press(&mut app, "ctrl-x n");
    assert_eq!(app.focus, Pane::Sidebar);
}

#[test]
fn only_inserting_text_moves_focus_to_the_box() {
    let mut app = App::new(Emacs, 132, 36);
    type_text(&mut app, "hi");
    app.focus = Pane::Sidebar;
    for key in ["bs", "del", "left", "right", "home", "end"] {
        press(&mut app, key);
        assert_eq!(app.focus, Pane::Sidebar, "{key}");
        assert_eq!(app.tabs[0].input.text(), "hi", "{key}");
    }
    type_text(&mut app, "x");
    assert_eq!(app.focus, Pane::Agent);
    assert_eq!(app.tabs[0].input.text(), "hix");
}

/// Wherever the keys are: normal mode, a menu, the search line, a picker over the bar.
#[test]
fn a_second_ctrl_c_quits_from_any_layer() {
    let setups: [&dyn Fn(&mut App); 4] = [
        &|app| press(app, "esc"),
        &|app| app.push_layer(LayerKind::Menu(crate::sample::MenuId::Accounts)),
        &|app| press(app, "esc /"),
        &|app| {
            type_text(app, "/agent");
            press(app, "enter");
        },
    ];
    for (at, setup) in setups.iter().enumerate() {
        let mut app = App::new(Vim, 132, 36);
        setup(&mut app);
        press(&mut app, "ctrl-c");
        assert_eq!(app.notice.as_deref(), Some("ctrl-c again to quit"), "{at}");
        assert!(!app.should_quit(), "{at}");
        press(&mut app, "ctrl-c");
        assert!(app.should_quit(), "{at}");
    }
}

#[test]
fn sending_on_a_session_tab_leaves_a_blank_after_the_transcript() {
    let mut app = App::new(Vim, 132, 36);
    enter_on_session(&mut app, "flaky");
    press(&mut app, "i");
    type_text(&mut app, "hi");
    press(&mut app, "enter");
    let lines = &app.tabs[0].lines;
    let sent = lines.len() - 4;
    assert_eq!(lines[sent].text, "hi");
    assert_eq!(lines[sent - 1].text, "");
    assert_eq!(lines[sent - 2].text, "  … run   cargo check -p gents-cli");
}

#[test]
fn only_the_back_row_of_a_diff_changes_with_the_key_set() {
    let emacs = with_sample_session(App::new(Emacs, 132, 36));
    let vim = with_sample_session(App::new(Vim, 132, 36));
    for file in 0..3 {
        let rows = emacs.diff_rows(file);
        let (last, rest) = rows.split_last().expect("rows");
        assert_eq!(last.0, " ctrl-g back to files");
        let vim_rows = vim.diff_rows(file);
        let (vim_last, vim_rest) = vim_rows.split_last().expect("rows");
        assert_eq!(vim_last.0, " esc back to files");
        assert_eq!(rest, vim_rest);
        assert!(rest.len() > 3, "{rest:?}");
    }
}

fn listed(name: &str, day: Day, state: SessionState) -> Session {
    Session {
        id: name.to_string(),
        agent: "Engineer".to_string(),
        name: name.to_string(),
        tab: name.to_string(),
        day,
        state,
    }
}

fn texts(rows: &[SidebarRow]) -> Vec<&str> {
    rows.iter().map(|row| row.text.as_ref()).collect()
}

#[test]
fn session_rows_group_by_day_with_state_marks() {
    let rows = session_rows(&[
        listed("one", Day::Today, SessionState::Active),
        listed("two", Day::Today, SessionState::Error),
        listed("three", Day::Yesterday, SessionState::Idle),
    ]);
    assert_eq!(
        texts(&rows),
        [
            " + New session",
            "",
            " TODAY",
            " ● one",
            " ! two",
            "",
            " YESTERDAY",
            " ○ three"
        ]
    );
    assert!(rows.iter().all(|row| row.text != " OLDER"));
    let tones: Vec<Tone> = rows[3..].iter().map(|row| row.tone).collect();
    use Tone::{Label, Plain, Warn};
    assert_eq!(tones, [Plain, Warn, Plain, Label, Plain]);
    assert_eq!(rows[4].action, RowAction::OpenSession(1));
    assert_eq!(
        (rows[7].name.as_ref(), rows[7].date),
        ("three", "yesterday")
    );
}

#[test]
fn an_older_session_gets_its_own_group() {
    let rows = session_rows(&[
        listed("new", Day::Today, SessionState::Idle),
        listed("old", Day::Older, SessionState::Idle),
    ]);
    assert_eq!(
        texts(&rows),
        [
            " + New session",
            "",
            " TODAY",
            " ○ new",
            "",
            " OLDER",
            " ○ old"
        ]
    );
    assert_eq!(rows[6].action, RowAction::OpenSession(1));
    assert_eq!(rows[6].date, "older");
}

fn connected() -> (App, std::sync::mpsc::Receiver<Intent>) {
    let (mut app, receiver) = crate::testing::connected(Vim, 132, 36);
    app.agents = crate::sample::agents();
    (app, receiver)
}

fn said(text: &str, tone: Tone, item: &str) -> ChatLine {
    ChatLine {
        text: text.to_string(),
        tone,
        item: Some(item.to_string()),
        change: None,
        under: None,
    }
}

fn item(session: &str, line: ChatLine) -> Update {
    Update::Item {
        session: session.to_string(),
        line,
    }
}

#[test]
fn a_connected_tab_sends_an_intent_and_waits() {
    let (mut app, intents) = connected();
    type_text(&mut app, "hello");
    press(&mut app, "enter");
    let sent = intents.try_recv().expect("an intent");
    assert!(
        matches!(&sent, Intent::Send { session: None, text, .. } if text == "hello"),
        "{sent:?}"
    );
    assert!(app.tabs[0].lines.is_empty());
    assert!(app.tabs[0].input.is_empty());
}

#[test]
fn started_binds_the_tab_and_items_follow() {
    let (mut app, _intents) = connected();
    type_text(&mut app, "hello");
    press(&mut app, "enter");
    let tab = app.tabs[0].id;
    app.apply(Update::Started {
        tab,
        session: "s1".to_string(),
    });
    app.apply(item("s1", said("hello", Tone::You, "u1")));
    assert_eq!(app.tabs[0].session.as_deref(), Some("s1"));
    assert_eq!(app.tabs[0].lines, [said("hello", Tone::You, "u1"), blank()]);
}

#[test]
fn an_item_seen_again_replaces_its_line() {
    let (mut app, _intents) = connected();
    app.tabs[0].session = Some("s1".to_string());
    app.apply(item("s1", said("working", Tone::Reply, "r1")));
    app.apply(item("s1", said("  … run   ls", Tone::Warn, "c1")));
    app.apply(item("s1", said("worked", Tone::Reply, "r1")));
    assert_eq!(
        app.tabs[0].lines,
        [
            said("worked", Tone::Reply, "r1"),
            blank(),
            said("  … run   ls", Tone::Warn, "c1"),
            blank()
        ]
    );
}

#[test]
fn opening_a_connected_session_emits_open_and_shows_opened() {
    let (mut app, intents) = connected();
    app.sessions = crate::sample::sessions();
    enter_on_session(&mut app, "flaky");
    let open = Intent::Open {
        session: "flaky recovery test".to_string(),
        agent: "Reviewer".to_string(),
    };
    assert_eq!(intents.try_recv(), Ok(open));
    assert!(app.tabs[0].lines.is_empty());

    app.apply(Update::Opened {
        session: "flaky recovery test".to_string(),
        lines: sample_chat(),
    });
    assert_eq!(app.tabs[0].lines, sample_chat());
    assert_eq!(app.mode, Mode::Normal);
    assert!(find(&screen(&app), "Group the sign-in").is_some());
}

#[test]
fn updates_for_a_session_no_tab_shows_are_dropped() {
    let (mut app, _intents) = connected();
    let before = app.tabs.clone();
    for update in [
        Update::Started {
            tab: 99,
            session: "s1".to_string(),
        },
        item("s1", said("hi", Tone::You, "u1")),
        Update::Opened {
            session: "s1".to_string(),
            lines: sample_chat(),
        },
        running("s1", crate::sample::running()),
        Update::Delta {
            session: "s1".to_string(),
            item: "r1".to_string(),
            text: "hi".to_string(),
            tone: Tone::Reply,
        },
        turn("t1"),
        ended("t1"),
    ] {
        app.apply(update);
    }
    assert_eq!(app.tabs, before);
}

#[test]
fn a_closed_source_marks_the_connection_lost() {
    let (mut app, intents) = connected();
    drop(intents);
    type_text(&mut app, "hello");
    press(&mut app, "enter");
    assert!(
        matches!(app.connection, Connection::Lost(_)),
        "{:?}",
        app.connection
    );
}

fn delta(item: &str, text: &str, tone: Tone) -> Update {
    Update::Delta {
        session: "s1".to_string(),
        item: item.to_string(),
        text: text.to_string(),
        tone,
    }
}

fn turn(turn: &str) -> Update {
    Update::Turn {
        session: "s1".to_string(),
        turn: turn.to_string(),
    }
}

fn ended(turn: &str) -> Update {
    Update::TurnEnded {
        session: "s1".to_string(),
        turn: turn.to_string(),
    }
}

#[test]
fn deltas_append_to_the_reply_in_order() {
    let (mut app, _intents) = connected();
    app.tabs[0].session = Some("s1".to_string());
    app.apply(item("s1", said("", Tone::Reply, "r1")));
    for text in ["Moving ", "them", " under auth"] {
        app.apply(delta("r1", text, Tone::Reply));
    }
    let reply = said("Moving them under auth", Tone::Reply, "r1");
    assert_eq!(app.tabs[0].lines, [reply, blank()]);
}

#[test]
fn a_delta_for_a_new_item_starts_a_line_in_its_tone() {
    let (mut app, _intents) = connected();
    app.tabs[0].session = Some("s1".to_string());
    app.apply(item("s1", said("  … run   ls", Tone::Warn, "c1")));
    app.apply(delta("c1/output", "Cargo.toml", Tone::Label));
    app.apply(delta("c1/output", "\nsrc", Tone::Label));
    assert_eq!(
        app.tabs[0].lines,
        [
            said("  … run   ls", Tone::Warn, "c1"),
            blank(),
            said("Cargo.toml\nsrc", Tone::Label, "c1/output"),
            blank()
        ]
    );
}

/// `gents --continue SESSION`: the starter tab binds it before any list is read, so text
/// typed while the lists load goes to it, and the list names the tab once it comes.
#[test]
fn a_continued_session_binds_the_starter_tab_before_the_lists() {
    let (mut app, intents) = connected();
    let session = crate::sample::sessions()[0].clone();
    app.apply(Update::Continue {
        session: session.id.clone(),
        agent: Some(session.agent.clone()),
    });
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].session.as_deref(), Some(session.id.as_str()));
    assert_eq!(app.tabs[0].agent_id, session.agent);
    let open = Intent::Open {
        session: session.id.clone(),
        agent: session.agent.clone(),
    };
    assert_eq!(intents.try_recv(), Ok(open));

    app.apply(Update::Sessions(crate::sample::sessions()));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].name, session.tab);

    app.apply(Update::Continue {
        session: "other".to_string(),
        agent: None,
    });
    assert_eq!(app.tabs[0].session.as_deref(), Some(session.id.as_str()));
    assert!(intents.try_recv().is_err(), "a bound tab is never rebound");
}

#[test]
fn ctrl_c_stops_a_running_turn_only_with_an_empty_box() {
    let (mut app, intents) = connected();
    app.tabs[0].session = Some("s1".to_string());
    app.apply(turn("t1"));
    type_text(&mut app, "draft");
    press(&mut app, "ctrl-c");
    assert!(app.tabs[0].input.is_empty());
    assert!(
        intents.try_recv().is_err(),
        "clearing the box stops nothing"
    );

    press(&mut app, "ctrl-c");
    let stop = Intent::Interrupt {
        session: "s1".to_string(),
        agent: "Engineer".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(intents.try_recv(), Ok(stop));
    assert_eq!(app.notice.as_deref(), Some("stopping the turn"));

    app.apply(ended("t1"));
    press(&mut app, "ctrl-c");
    assert!(intents.try_recv().is_err());
    assert_eq!(app.notice.as_deref(), Some("ctrl-c again to quit"));
    assert!(
        !app.should_quit(),
        "the press that stopped the turn was not the first"
    );
}

#[test]
fn s_in_normal_mode_stops_the_running_turn_and_keeps_the_draft() {
    let (mut app, intents) = connected();
    app.tabs[0].session = Some("s1".to_string());
    press(&mut app, "esc");
    press(&mut app, "s");
    assert!(intents.try_recv().is_err());
    assert_eq!(app.notice.as_deref(), Some("nothing is running"));

    app.apply(turn("t1"));
    press(&mut app, "i");
    type_text(&mut app, "draft");
    press(&mut app, "esc");
    press(&mut app, "s");
    let stop = Intent::Interrupt {
        session: "s1".to_string(),
        agent: "Engineer".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(intents.try_recv(), Ok(stop));
    assert_eq!(app.notice.as_deref(), Some("stopping the turn"));
    assert_eq!(app.tabs[0].input.text(), "draft");
}

#[test]
fn close_from_the_jump_bar_closes_the_tab() {
    let (mut app, _intents) = connected();
    app.push_tab(Tab::new_session());
    assert_eq!(app.tabs.len(), 2);
    press(&mut app, "esc");
    press(&mut app, ":");
    type_text(&mut app, "close");
    press(&mut app, "enter");
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn wrapped_rows_follow_a_changed_line_and_width() {
    let mut tab = Tab::new_session();
    tab.lines = vec![ChatLine {
        text: "one two three".to_string(),
        tone: Tone::Plain,
        item: None,
        change: None,
        under: None,
    }];
    let rows = |tab: &Tab, width| -> Vec<String> {
        tab.wrapped_rows(width)
            .into_iter()
            .map(|(row, _)| row)
            .collect()
    };
    assert_eq!(rows(&tab, 20), ["one two three"]);
    assert_eq!(rows(&tab, 8), ["one two", "three"]);
    tab.lines[0].text.push_str(" four");
    assert_eq!(rows(&tab, 8), ["one two", "three", "four"]);
    tab.lines[0].tone = Tone::Reply;
    assert_eq!(tab.wrapped_rows(8)[0].1, Tone::Reply);
    tab.lines.clear();
    assert!(rows(&tab, 8).is_empty());
}

#[test]
fn an_older_turn_ending_leaves_the_newer_one() {
    let (mut app, intents) = connected();
    app.tabs[0].session = Some("s1".to_string());
    app.apply(turn("A"));
    app.apply(turn("B"));
    app.apply(ended("A"));
    assert_eq!(app.tabs[0].turn.as_deref(), Some("B"));
    press(&mut app, "ctrl-c");
    let stop = Intent::Interrupt {
        session: "s1".to_string(),
        agent: "Engineer".to_string(),
        turn: "B".to_string(),
    };
    assert_eq!(intents.try_recv(), Ok(stop));
    app.apply(ended("B"));
    assert_eq!(app.tabs[0].turn, None);
}

#[test]
fn a_relist_keeps_the_selected_session() {
    let (mut app, _intents) = connected();
    let today = |name| listed(name, Day::Today, SessionState::Idle);
    app.apply(Update::Sessions(vec![today("one"), today("two")]));
    app.sessions_sel = 3;
    assert_eq!(app.session_rows()[app.sessions_sel].text, " ○ one");

    app.apply(Update::Sessions(vec![today("two"), today("one")]));
    assert_eq!(app.session_rows()[app.sessions_sel].text, " ○ one");
}

#[test]
fn a_refreshed_list_keeps_the_selection_in_range() {
    let (mut app, _intents) = connected();
    app.apply(Update::Sessions(crate::sample::sessions()));
    enter_on_session(&mut app, "weekly triage");
    app.sessions_sel = app.session_rows().len() - 1;

    let mut renamed = listed("weekly triage", Day::Today, SessionState::Idle);
    renamed.tab = "triage, renamed".to_string();
    app.apply(Update::Sessions(vec![renamed]));
    assert_eq!(app.sessions_sel, 3);
    assert_eq!(
        app.session_rows()[app.sessions_sel].text,
        " ○ weekly triage"
    );
    assert_eq!(app.tabs[0].name, "triage, renamed");

    app.apply(Update::SessionState {
        session: "weekly triage".to_string(),
        state: SessionState::Error,
    });
    let row = &app.session_rows()[3];
    assert_eq!(
        (row.text.as_ref(), row.tone),
        (" ! weekly triage", Tone::Warn)
    );
}

fn with_runtime_agents() -> (App, std::sync::mpsc::Receiver<Intent>) {
    let (mut app, receiver) = crate::testing::connected(Vim, 132, 36);
    let agent = |id: &str, name: &str| Agent {
        id: id.to_string(),
        name: name.to_string(),
        text: format!(" {name}"),
        tone: Tone::Plain,
    };
    app.apply(Update::Agents(vec![
        agent("local:default", "default"),
        agent("local:reviewer", "reviewer"),
    ]));
    (app, receiver)
}

fn agent_of(tab: &Tab) -> (&str, &str) {
    (&tab.agent, &tab.agent_id)
}

#[test]
fn a_starter_tab_already_sent_from_keeps_its_agent_and_sends_no_prompt() {
    let (mut app, intents) = with_runtime_agents();
    type_text(&mut app, "mine");
    press(&mut app, "enter");
    intents.try_recv().expect("the user's own send");

    app.apply(Update::Begin {
        agent: Some("local:reviewer".to_string()),
        prompt: Some("hello".to_string()),
    });

    assert_eq!(agent_of(&app.tabs[0]), ("default", "local:default"));
    assert!(intents.try_recv().is_err());
}

#[test]
fn closing_the_last_tab_takes_the_default_agent() {
    let (mut app, _receiver) = crate::testing::connected(Vim, 132, 36);
    press(&mut app, "esc ctrl-w q");
    let waiting = agent_of(&app.tabs[0]);
    assert_eq!(
        waiting,
        ("Engineer", "Engineer"),
        "before the agents arrive"
    );

    let (mut app, _intents) = with_runtime_agents();
    let closed = app.tabs[0].id;
    press(&mut app, "esc ctrl-w q");
    assert_eq!(app.tabs.len(), 1);
    assert_ne!(app.tabs[0].id, closed);
    assert_eq!(agent_of(&app.tabs[0]), ("default", "local:default"));
}

#[test]
fn a_new_session_tab_takes_the_default_agent() {
    let (mut app, _intents) = with_runtime_agents();
    app.focus = Pane::Sessions;
    app.sessions_sel = 0;
    press(&mut app, "enter enter");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(agent_of(&app.tabs[1]), ("default", "local:default"));

    type_text(&mut app, "/new");
    press(&mut app, "enter enter");
    assert_eq!(app.tabs.len(), 3);
    assert_eq!(agent_of(&app.tabs[2]), ("default", "local:default"));
}

fn opened(session: &str) -> Update {
    Update::Opened {
        session: session.to_string(),
        lines: sample_chat(),
    }
}

#[test]
fn opened_leaves_a_draft_typed_while_it_loaded() {
    let (mut app, _intents) = connected();
    app.sessions = crate::sample::sessions();
    enter_on_session(&mut app, "flaky");
    type_text(&mut app, "dr");
    app.apply(opened("flaky recovery test"));
    assert_eq!(app.tabs[0].lines, sample_chat());
    assert_eq!(app.mode, Mode::Input);
    assert_eq!(app.focus, Pane::Agent);
    type_text(&mut app, "aft :");
    assert_eq!(app.tabs[0].input.text(), "draft :");
    assert!(app.layers.is_empty());
}

#[test]
fn opened_for_a_tab_in_the_background_moves_nothing() {
    let (mut app, _intents) = connected();
    app.sessions = crate::sample::sessions();
    enter_on_session(&mut app, "flaky");
    enter_on_session(&mut app, "cli ux");
    assert_eq!((app.active, app.mode), (1, Mode::Input));
    app.apply(opened("flaky recovery test"));
    assert_eq!(app.tabs[0].lines, sample_chat());
    assert_eq!((app.active, app.mode), (1, Mode::Input));
}

#[test]
fn a_tab_starts_one_session_however_fast_it_sends() {
    let (mut app, intents) = connected();
    type_text(&mut app, "one");
    press(&mut app, "enter");
    type_text(&mut app, "two");
    press(&mut app, "enter");
    let first = intents.try_recv().expect("the first send");
    assert!(
        matches!(&first, Intent::Send { session: None, text, .. } if text == "one"),
        "{first:?}"
    );
    assert!(intents.try_recv().is_err(), "the second send waits");
    let waiting = "starting the session, try again in a moment";
    assert_eq!(app.notice.as_deref(), Some(waiting));
    assert_eq!(app.tabs[0].input.text(), "two");

    let tab = app.tabs[0].id;
    app.apply(Update::Started {
        tab,
        session: "s1".to_string(),
    });
    press(&mut app, "enter");
    let second = intents.try_recv().expect("the send after Started");
    assert!(
        matches!(&second, Intent::Send { session: Some(session), text, .. }
            if session == "s1" && text == "two"),
        "{second:?}"
    );
}

#[test]
fn a_failed_start_frees_the_tab_to_send_again() {
    let (mut app, intents) = connected();
    type_text(&mut app, "one");
    press(&mut app, "enter");
    assert!(intents.try_recv().is_ok());
    assert!(app.tabs[0].starting);

    app.apply(Update::StartFailed {
        tab: 99,
        message: "elsewhere".to_string(),
    });
    assert!(app.tabs[0].starting, "another tab's failure");
    assert_eq!(app.notice, None);

    let tab = app.tabs[0].id;
    app.apply(Update::StartFailed {
        tab,
        message: "the runtime refused the thread".to_string(),
    });
    assert!(!app.tabs[0].starting);
    let refused = "the runtime refused the thread";
    assert_eq!(app.notice.as_deref(), Some(refused));
    assert!(app.tabs[0].input.is_empty());

    type_text(&mut app, "two");
    press(&mut app, "enter");
    let again = intents.try_recv().expect("a send after the failure");
    assert!(
        matches!(&again, Intent::Send { session: None, text, .. } if text == "two"),
        "{again:?}"
    );
}

#[test]
fn a_session_opened_while_the_first_tab_starts_gets_its_own_tab() {
    let (mut app, intents) = connected();
    app.sessions = crate::sample::sessions();
    type_text(&mut app, "hello");
    press(&mut app, "enter");
    assert!(intents.try_recv().is_ok());
    let first = app.tabs[0].id;
    enter_on_session(&mut app, "flaky");
    let flaky = Some("flaky recovery test");
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs[0].session, None);
    assert_eq!(app.tabs[1].session.as_deref(), flaky);

    app.apply(Update::Started {
        tab: first,
        session: "s-new".to_string(),
    });
    assert_eq!(app.tabs[0].session.as_deref(), Some("s-new"));
    assert_eq!(app.tabs[1].session.as_deref(), flaky);
    app.apply(Update::Started {
        tab: first,
        session: "s-again".to_string(),
    });
    assert_eq!(app.tabs[0].session.as_deref(), Some("s-new"), "bound once");
}

#[test]
fn a_lost_source_keeps_the_draft_and_says_why() {
    let why = "runtime closed: check `gents status`, then reopen the shell";
    let lost = || Update::Connection(Connection::Lost(why.to_string()));
    let (mut app, intents) = connected();
    app.apply(lost());
    type_text(&mut app, "hello");
    press(&mut app, "enter");
    assert!(intents.try_recv().is_err());
    assert_eq!(app.tabs[0].input.text(), "hello");
    assert!(!app.tabs[0].starting);
    assert_eq!(app.notice.as_deref(), Some(why));

    app.sessions = crate::sample::sessions();
    press(&mut app, "ctrl-c");
    enter_on_session(&mut app, "flaky");
    assert!(intents.try_recv().is_err());
    assert_eq!(app.tabs[0].session, None);
    assert_eq!(app.notice.as_deref(), Some(why));

    app.tabs[0].session = Some("s1".to_string());
    app.apply(turn("t1"));
    app.focus = Pane::Agent;
    press(&mut app, "ctrl-c");
    assert!(intents.try_recv().is_err());
    assert_eq!(app.notice.as_deref(), Some(why));

    let (mut app, _receiver) = crate::testing::connected(Vim, 132, 36);
    app.apply(lost());
    type_text(&mut app, "hello");
    press(&mut app, "enter");
    assert_eq!(app.notice.as_deref(), Some(why), "before the agents arrive");
    assert_eq!(app.tabs[0].input.text(), "hello");
}

#[test]
fn the_sample_prompt_takes_its_keys() {
    for (key, notice) in [("y", Some("acknowledged")), ("n", None)] {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        enter_on_session(&mut app, "flaky");
        assert!(find(&screen(&app), "! Allow Reviewer").is_some());
        press(&mut app, key);
        assert_eq!(app.notice.as_deref(), notice, "{key}");
        assert_eq!(find(&screen(&app), "! Allow Reviewer"), None, "{key}");
    }
    let mut app = with_right_pane(App::new(Vim, 132, 36));
    enter_on_session(&mut app, "flaky");
    press(&mut app, "a");
    assert_eq!(app.notice, None);
    assert!(
        find(&screen(&app), "! Allow Reviewer").is_some(),
        "no always"
    );

    let mut app = with_right_pane(App::new(Vim, 132, 36));
    press(&mut app, "esc");
    for key in ["y", "n", "a"] {
        press(&mut app, key);
        assert_eq!(app.notice, None, "{key}");
    }

    let mut app = with_right_pane(App::new(Emacs, 132, 36));
    enter_on_session(&mut app, "flaky");
    press(&mut app, "y");
    assert_eq!(app.notice, None);
    assert_eq!(app.tabs[0].input.text(), "y");

    let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
    app.view = View::Agents;
    press(&mut app, "esc ctrl-w l ctrl-w l enter");
    assert_eq!(app.notice, None, "a background call is not selectable");
}

fn commit_skill(agent: &str) -> Update {
    Update::Skills {
        agent: agent.to_string(),
        skills: vec![Skill {
            id: "commit".to_string(),
            about: "Commit staged changes".to_string(),
        }],
    }
}

/// Types `text`, with the tab agent's skills told once the list opens.
fn type_slash(app: &mut App, text: &str) {
    type_text(app, "/");
    app.apply(commit_skill(&app.tabs[app.active].agent_id.clone()));
    type_text(app, &text[1..]);
}

#[test]
fn opening_the_list_reads_the_agents_skills() {
    let (mut app, intents) = connected();
    type_text(&mut app, "/go");
    let read = Intent::ReadSkills {
        agent: app.tabs[0].agent_id.clone(),
    };
    assert_eq!(intents.try_iter().collect::<Vec<_>>(), [read]);
}

#[test]
fn skills_list_after_the_commands() {
    let (mut app, _intents) = connected();
    type_slash(&mut app, "/");
    for _ in 0..11 {
        press(&mut app, "ctrl-n");
    }
    let s = screen(&app);
    let (quit, _) = find(&s, " /quit").expect("the last command shown");
    let (skill, _) = find(&s, " /commit    Commit staged changes").expect("the skill");
    assert!(skill > quit, "{skill} > {quit}");
    assert!(find(&s, " Commands and skills ").is_some());
}

#[test]
fn skills_for_another_agent_are_not_listed() {
    let (mut app, _intents) = connected();
    type_text(&mut app, "/");
    app.apply(commit_skill("other"));
    let rows = app.slash_matches();
    assert_eq!(rows.len(), 11);
    assert!(rows.iter().all(|row| row.name != "commit"));
}

#[test]
fn a_skill_runs_with_the_text_after_it() {
    let (mut app, intents) = connected();
    let (tab, agent) = (app.tabs[0].id, app.tabs[0].agent_id.clone());
    type_slash(&mut app, "/commit fix the bug");
    press(&mut app, "enter");
    let skill = |session: Option<&str>, text: &str| Intent::SendSkill {
        tab,
        session: session.map(str::to_string),
        agent: agent.clone(),
        skill: "commit".to_string(),
        text: text.to_string(),
    };
    assert_eq!(sent(&intents), [skill(None, "fix the bug")]);
    assert!(app.tabs[0].input.is_empty());
    assert_eq!(app.slash, None);
    assert!(app.tabs[0].following);

    app.apply(Update::Started {
        tab,
        session: "s1".to_string(),
    });
    type_slash(&mut app, "/commit");
    press(&mut app, "enter");
    assert_eq!(sent(&intents), [skill(Some("s1"), "")]);
}

#[test]
fn the_list_filters_on_the_first_word() {
    let (mut app, _intents) = connected();
    type_text(&mut app, "/goal ship it");
    let names: Vec<_> = app
        .slash_matches()
        .into_iter()
        .map(|row| row.name)
        .collect();
    assert_eq!(names, ["goal"]);

    let (mut app, _intents) = connected();
    type_text(&mut app, "/zzz now");
    press(&mut app, "enter");
    assert_eq!(app.notice.as_deref(), Some("unknown command: /zzz now"));
    assert_eq!(app.tabs[0].input.text(), "/zzz now");
}

#[test]
fn goal_alone_waits_for_its_objective() {
    let (mut app, intents) = connected();
    type_text(&mut app, "/goal");
    press(&mut app, "enter");
    assert!(sent(&intents).is_empty());
    assert_eq!(app.tabs[0].input.text(), "/goal ");
    assert_eq!(app.slash, Some(0));

    type_text(&mut app, "ship it");
    press(&mut app, "enter");
    let goal = Intent::SetGoal {
        tab: app.tabs[0].id,
        session: None,
        agent: app.tabs[0].agent_id.clone(),
        objective: "ship it".to_string(),
    };
    assert_eq!(sent(&intents), [goal]);
    assert!(app.tabs[0].starting);
    assert!(!app.tabs[0].following);
    assert!(app.tabs[0].input.is_empty());
    assert_eq!(app.slash, None);
}

#[test]
fn a_line_break_after_a_command_starts_its_argument() {
    let (mut app, intents) = connected();
    type_text(&mut app, "/goal");
    press(&mut app, "shift-enter");
    type_text(&mut app, "ship it");
    press(&mut app, "enter");
    let goal = Intent::SetGoal {
        tab: app.tabs[0].id,
        session: None,
        agent: app.tabs[0].agent_id.clone(),
        objective: "ship it".to_string(),
    };
    assert_eq!(sent(&intents), [goal]);
}

#[test]
fn a_paste_into_an_open_command_is_its_argument() {
    let (mut app, intents) = connected();
    let (tab, agent) = (app.tabs[0].id, app.tabs[0].agent_id.clone());
    type_text(&mut app, "/goal");
    press(&mut app, "enter");
    app.handle(Event::Paste("ship\nit".to_string()));
    press(&mut app, "enter");
    let goal = Intent::SetGoal {
        tab,
        session: None,
        agent: agent.clone(),
        objective: "ship\nit".to_string(),
    };
    assert_eq!(sent(&intents), [goal]);

    let (mut app, intents) = connected();
    type_slash(&mut app, "/commit ");
    app.handle(Event::Paste("fix\nthe bug".to_string()));
    press(&mut app, "enter");
    let skill = Intent::SendSkill {
        tab,
        session: None,
        agent: agent.clone(),
        skill: "commit".to_string(),
        text: "fix\nthe bug".to_string(),
    };
    assert_eq!(sent(&intents), [skill]);

    let (mut app, intents) = connected();
    app.handle(Event::Paste("/goal ship it".to_string()));
    assert_eq!(app.slash, None);
    press(&mut app, "enter");
    let text = Intent::Send {
        tab,
        session: None,
        agent,
        text: "/goal ship it".to_string(),
    };
    assert_eq!(sent(&intents), [text], "a closed list is plain text");
}

#[test]
fn a_refused_goal_or_skill_keeps_its_text() {
    for text in ["/goal ship it", "/commit fix it"] {
        let (mut app, intents) = connected();
        app.tabs[0].starting = true;
        type_slash(&mut app, text);
        press(&mut app, "enter");
        assert_eq!(app.notice.as_deref(), Some(STARTING), "{text}");
        assert_eq!(app.tabs[0].input.text(), text);
        assert!(sent(&intents).is_empty(), "{text}");

        let (mut app, intents) = connected();
        app.apply(Update::Connection(Connection::Lost("gone".to_string())));
        type_slash(&mut app, text);
        press(&mut app, "enter");
        assert_eq!(app.notice.as_deref(), Some("gone"), "{text}");
        assert_eq!(app.tabs[0].input.text(), text);
        assert!(intents.try_recv().is_err(), "a lost source reads no skills");
    }
}
