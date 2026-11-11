use ratatui::layout::{Margin, Rect};

use super::input::LineInput;
use super::{App, Pane};
use crate::keys::{Action, KeySet};
use crate::layout::{sidebar_rows_area, Right};
use crate::sample::{SidebarRow, Tone};

/// `start` and `end` count characters, not bytes or columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub pane: Pane,
    pub query: LineInput,
    pub committed: bool,
    pub current: usize,
    saved: usize,
}

pub fn matches(lines: &[String], query: &str) -> Vec<Match> {
    let query: Vec<char> = query.chars().collect();
    let mut found = Vec::new();
    if query.is_empty() {
        return found;
    }
    let same = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    for (row, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut start = 0;
        while start + query.len() <= chars.len() {
            let end = start + query.len();
            if chars[start..end]
                .iter()
                .zip(&query)
                .all(|(&a, &b)| same(a, b))
            {
                found.push(Match { row, start, end });
                start = end;
            } else {
                start += 1;
            }
        }
    }
    found
}

fn texts(rows: Vec<(String, Tone)>) -> Vec<String> {
    rows.into_iter().map(|(text, _)| text).collect()
}

fn select_at_or_after(rows: &[SidebarRow], row: usize, selected: &mut usize) {
    if let Some(found) = (row..rows.len()).find(|&index| rows[index].selectable()) {
        *selected = found;
    }
}

impl App {
    pub(super) fn open_search(&mut self) {
        self.slash = None;
        let pane = self.focus;
        self.search = Some(Search {
            pane,
            query: LineInput::default(),
            committed: false,
            current: 0,
            saved: *self.position_mut(pane),
        });
    }

    pub(super) fn edit_search(&mut self, action: Action) {
        let Some(search) = &mut self.search else {
            return;
        };
        search.query.edit(action);
        if matches!(
            action,
            Action::Insert(_)
                | Action::Backspace
                | Action::Delete
                | Action::DeleteWord
                | Action::DeleteToStart
        ) {
            search.current = 0;
            self.go_to_match();
        }
    }

    pub(super) fn submit_search(&mut self) {
        let keep = self.keymap.set() == KeySet::Vim;
        match &mut self.search {
            Some(search) if keep && !search.query.is_empty() => search.committed = true,
            _ => self.search = None,
        }
    }

    pub(super) fn back_from_search(&mut self) {
        if let Some(search) = self.search.take() {
            if !search.committed {
                *self.position_mut(search.pane) = search.saved;
            }
        }
    }

    pub(super) fn step_match(&mut self, forward: bool) {
        let Some(search) = &self.search else {
            return;
        };
        let count = matches(&self.search_lines(search.pane), search.query.text()).len();
        let Some(search) = self.search.as_mut().filter(|_| count > 0) else {
            return;
        };
        search.current = if forward {
            (search.current + 1) % count
        } else {
            (search.current + count - 1) % count
        };
        self.go_to_match();
    }

    pub(super) fn go_to_match(&mut self) {
        let Some(search) = &self.search else {
            return;
        };
        let (pane, committed, saved) = (search.pane, search.committed, search.saved);
        let lines = self.search_lines(pane);
        let found = matches(&lines, search.query.text());
        let Some(found) = found.get(search.current) else {
            if !committed {
                *self.position_mut(pane) = saved;
            }
            return;
        };
        let height = self
            .pane_rows(pane)
            .map_or(0, |rows| usize::from(self.visible_rows(pane, rows).height));
        let max = lines.len().saturating_sub(height);
        match (pane, self.diff) {
            (Pane::Agent, _) => {
                self.tabs[self.active].scroll = (lines.len() - 1 - found.row).min(max);
            }
            (Pane::Sessions, _) => {
                let rows = self.session_rows();
                select_at_or_after(&rows, found.row, &mut self.sessions_sel);
            }
            (Pane::Sidebar, Some(_)) => self.diff_scroll = found.row.min(max),
            (Pane::Sidebar, None) => {
                let rows = self.view_rows(self.view);
                select_at_or_after(&rows, found.row, &mut self.view_sel[self.view as usize]);
            }
        }
    }

    fn position_mut(&mut self, pane: Pane) -> &mut usize {
        match (pane, self.diff) {
            (Pane::Agent, _) => &mut self.tabs[self.active].scroll,
            (Pane::Sessions, _) => &mut self.sessions_sel,
            (Pane::Sidebar, Some(_)) => &mut self.diff_scroll,
            (Pane::Sidebar, None) => &mut self.view_sel[self.view as usize],
        }
    }

    pub fn search_lines(&self, pane: Pane) -> Vec<String> {
        match (pane, self.diff) {
            (Pane::Sessions, _) => self
                .session_rows()
                .into_iter()
                .map(|row| row.text.into_owned())
                .collect(),
            (Pane::Sidebar, Some(file)) => texts(self.diff_rows(file)),
            (Pane::Sidebar, None) => self
                .view_rows(self.view)
                .into_iter()
                .map(|row| row.text.into_owned())
                .collect(),
            (Pane::Agent, _) => self
                .pane_rows(pane)
                .map_or(Vec::new(), |rows| texts(self.conversation_rows(rows.width))),
        }
    }

    pub fn pane_rows(&self, pane: Pane) -> Option<Rect> {
        let panes = self.panes()?;
        match (pane, panes.right) {
            (Pane::Agent, _) => Some(self.conversation_rect(panes.agent)),
            (Pane::Sessions, _) => Some(panes.sessions?.inner(Margin::new(1, 1))),
            (Pane::Sidebar, Right::Sidebar(rect)) => Some(sidebar_rows_area(rect, self.view)),
            (Pane::Sidebar, _) => None,
        }
    }

    pub fn visible_rows(&self, pane: Pane, rows: Rect) -> Rect {
        let line = self
            .search
            .as_ref()
            .is_some_and(|search| search.pane == pane);
        Rect {
            height: rows.height.saturating_sub(u16::from(line)),
            ..rows
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{sample_chat, ChatLine, Mode, Tab};
    use crate::draw::theme;
    use crate::keys::KeyCtx;
    use crate::keys::KeySet::{Emacs, Vim};
    use crate::sample::View;
    use crate::testing::{
        buffer, find, numbered, press, screen, session_row, type_text, with_right_pane,
        with_sample_session,
    };
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::Event;
    use ratatui::Terminal;

    fn session_tab(app: &mut App) {
        app.open_session(session_row("flaky"));
    }

    fn cell_bg(app: &App, text: &str, offset: u16) -> ratatui::style::Color {
        let (row, column) = find(&screen(app), text).expect(text);
        buffer(app)[(column + offset, row)].bg
    }

    #[test]
    fn matches_count_characters_and_ignore_case() {
        assert_eq!(matches(&["Reviewer reviewer".into()], "REV").len(), 2);
        assert_eq!(
            matches(&["日本語 test".into()], "本")[0],
            Match {
                row: 0,
                start: 1,
                end: 2
            }
        );
        assert!(matches(&["x".into()], "").is_empty());
        assert_eq!(
            matches(&["a".into(), "".into(), "xaa".into()], "aa"),
            [Match {
                row: 2,
                start: 1,
                end: 3
            }]
        );
        assert_eq!(matches(&["aaaa".into()], "aa").len(), 2);
    }

    #[test]
    fn vim_search_commits_then_n_moves() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        session_tab(&mut app);
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, "/");
        assert_eq!(app.ctx(), KeyCtx::SearchLine);
        assert_eq!(app.chip(), "SEARCH");
        type_text(&mut app, "args");
        press(&mut app, "enter");
        let search = app.search.as_ref().expect("a search");
        assert!(search.committed);
        assert_eq!(search.query.text(), "args");
        assert_eq!(app.ctx(), KeyCtx::Normal);
        assert_eq!(cell_bg(&app, "src/cli/args", 8), theme::MATCH);
        assert_ne!(cell_bg(&app, "src/cli/args", 7), theme::MATCH);
        assert_ne!(cell_bg(&app, "M cli/args.rs", 6), theme::MATCH);

        for (key, current) in [("n", 1), ("n", 2), ("n", 0), ("N", 2), ("N", 1)] {
            press(&mut app, key);
            assert_eq!(app.search.as_ref().unwrap().current, current, "{key}");
        }

        press(&mut app, "esc");
        assert_eq!(app.search, None);
        assert_ne!(cell_bg(&app, "src/cli/args", 8), theme::MATCH);
        press(&mut app, "n");
        assert!(
            app.tabs[0].prompts.is_empty(),
            "n dismisses the prompt again"
        );
    }

    #[test]
    fn the_current_match_stands_out() {
        let mut app = App::new(Vim, 132, 36);
        session_tab(&mut app);
        press(&mut app, "/");
        type_text(&mut app, "args");
        press(&mut app, "enter");
        let modifier = |app: &App, row| {
            let (_, column) = find(&screen(app)[row..], "args").expect("a match");
            buffer(app)[(column, row as u16)].modifier
        };
        let (first, second) = (modifier(&app, 9), modifier(&app, 10));
        assert_ne!(first, second);
        press(&mut app, "n");
        assert_eq!(modifier(&app, 9), second);
        assert_eq!(modifier(&app, 10), first);
    }

    #[test]
    fn emacs_search_moves_while_open_and_enter_ends() {
        let mut app = App::new(Emacs, 132, 36);
        app.tabs[0].lines = numbered(40);
        press(&mut app, "ctrl-s");
        assert_eq!(app.ctx(), KeyCtx::SearchLine);
        type_text(&mut app, "message 3");
        assert!(app.tabs[0].input.is_empty());
        let first = app.tabs[0].scroll;
        assert_eq!(first, 14);
        press(&mut app, "ctrl-s");
        assert_eq!(app.search.as_ref().unwrap().current, 1);
        let second = app.tabs[0].scroll;
        assert!(second < first);
        press(&mut app, "ctrl-r");
        assert_eq!(app.search.as_ref().unwrap().current, 0);
        assert_eq!(app.tabs[0].scroll, first);
        press(&mut app, "ctrl-r");
        assert_eq!(app.search.as_ref().unwrap().current, 10);

        press(&mut app, "ctrl-s enter");
        assert_eq!(app.search, None);
        assert_eq!(app.tabs[0].scroll, first);
        assert_eq!(app.ctx(), KeyCtx::Input);
    }

    #[test]
    fn cancel_restores_the_position() {
        for (set, open, cancel) in [
            (Vim, "esc /", "esc"),
            (Vim, "esc /", "ctrl-g"),
            (Emacs, "ctrl-s", "ctrl-g"),
            (Emacs, "ctrl-s", "esc"),
        ] {
            let mut app = with_right_pane(App::new(set, 132, 36));
            app.tabs[0].lines = numbered(40);
            app.tabs[0].scroll = 2;
            press(&mut app, open);
            type_text(&mut app, "message 3");
            assert_ne!(app.tabs[0].scroll, 2);
            press(&mut app, cancel);
            assert_eq!(app.search, None, "{cancel}");
            assert_eq!(app.tabs[0].scroll, 2, "{cancel}");
        }

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w h /");
        let before = app.sessions_sel;
        type_text(&mut app, "weekly");
        assert_eq!(app.sessions_sel, session_row("weekly"));
        press(&mut app, "esc");
        assert_eq!(app.sessions_sel, before);

        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        press(&mut app, "esc ctrl-w l ctrl-w l /");
        let before = app.view_sel[View::Diff as usize];
        type_text(&mut app, "args");
        assert_ne!(app.view_sel[View::Diff as usize], before);
        press(&mut app, "ctrl-g");
        assert_eq!(app.view_sel[View::Diff as usize], before);
    }

    #[test]
    fn ctrl_w_on_the_line_deletes_a_word_and_leaves_the_tab_alone() {
        let query = |app: &App| app.search.as_ref().unwrap().query.text().to_string();
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = numbered(40);
        type_text(&mut app, "draft");
        press(&mut app, "esc /");
        type_text(&mut app, "message zzz");
        assert_eq!(app.tabs[0].scroll, 0);

        press(&mut app, "ctrl-w");
        assert_eq!(query(&app), "message ");
        assert_ne!(app.tabs[0].scroll, 0);

        press(&mut app, "ctrl-w q");
        assert_eq!(query(&app), "q");
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].input.text(), "draft");
    }

    #[test]
    fn no_match_goes_back_to_the_start_until_the_query_finds_one() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = numbered(40);
        press(&mut app, "esc /");
        type_text(&mut app, "message 3");
        assert_ne!(app.tabs[0].scroll, 0);
        type_text(&mut app, "z");
        assert_eq!(app.tabs[0].scroll, 0);
        press(&mut app, "bs");
        assert_ne!(app.tabs[0].scroll, 0);
    }

    #[test]
    fn search_covers_only_the_focused_pane() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        session_tab(&mut app);
        press(&mut app, "ctrl-w h /");
        type_text(&mut app, "flaky");
        assert_eq!(cell_bg(&app, "○ flaky", 2), theme::MATCH);
        assert_ne!(cell_bg(&app, "1 flaky", 2), theme::MATCH);

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w h /");
        type_text(&mut app, "pack");
        assert_eq!(app.sessions_sel, session_row("pack registry"));
        press(&mut app, "enter n");
        assert_eq!(app.sessions_sel, session_row("pack review"));
        press(&mut app, "n");
        assert_eq!(app.sessions_sel, session_row("pack registry"));

        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        press(&mut app, "esc ctrl-w l ctrl-w l /");
        type_text(&mut app, "cli/");
        assert_eq!(cell_bg(&app, "M cli/args.rs", 2), theme::MATCH);
        assert_eq!(app.view_sel[View::Diff as usize], 3);
        press(&mut app, "enter n");
        assert_eq!(app.view_sel[View::Diff as usize], 4);
    }

    #[test]
    fn a_match_on_a_header_selects_the_next_row_that_can_be_selected() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w h /");
        type_text(&mut app, "yesterday");
        assert_eq!(app.sessions_sel, session_row("eval baseline"));

        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        press(&mut app, "esc ctrl-w l ctrl-w l j /");
        type_text(&mut app, "files");
        assert_eq!(cell_bg(&app, "3 files", 2), theme::MATCH);
        assert_eq!(app.view_sel[View::Diff as usize], 2);
    }

    #[test]
    fn a_diff_scrolls_to_its_matches() {
        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 16)));
        press(&mut app, "esc ctrl-w l ctrl-w l j j enter /");
        type_text(&mut app, "back to");
        let s = screen(&app);
        assert_eq!(find(&s, "esc back to files").map(|(row, _)| row), Some(12));
        assert_eq!(find(&s, "search: back to").map(|(row, _)| row), Some(13));
        assert_eq!(cell_bg(&app, "esc back to files", 4), theme::MATCH);

        let mut app = with_right_pane(with_sample_session(App::new(Emacs, 132, 36)));
        press(&mut app, "ctrl-x o enter ctrl-s");
        type_text(&mut app, "ctrl-g");
        assert_eq!(cell_bg(&app, "ctrl-g back to files", 0), theme::MATCH);
    }

    #[test]
    fn changing_focus_ends_the_search() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        session_tab(&mut app);
        press(&mut app, "/");
        type_text(&mut app, "args");
        press(&mut app, "enter ctrl-w l");
        assert_eq!(app.search, None);
        assert_eq!(app.focus, Pane::Sidebar);

        let mut app = with_right_pane(App::new(Emacs, 132, 36));
        press(&mut app, "ctrl-s ctrl-x o");
        assert_eq!(app.search, None);
        assert_eq!(app.ctx(), KeyCtx::Input);

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w l ctrl-w l /");
        app.handle(ratatui::crossterm::event::Event::Resize(104, 30));
        assert_eq!(app.focus, Pane::Agent);
        assert_eq!(app.search, None);
    }

    #[test]
    fn leaving_the_rows_ends_the_search() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        session_tab(&mut app);
        app.tabs.push(Tab::new_session());
        press(&mut app, "/");
        type_text(&mut app, "args");
        press(&mut app, "enter g t");
        assert_eq!(app.search, None);

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        session_tab(&mut app);
        press(&mut app, "/");
        type_text(&mut app, "args");
        press(&mut app, "enter i");
        assert_eq!(app.search, None);
        assert_eq!(app.ctx(), KeyCtx::Input);

        let mut app = with_right_pane(App::new(Emacs, 132, 36));
        app.view = View::Agents;
        app.view_sel[View::Agents as usize] = 8;
        press(&mut app, "ctrl-x o ctrl-s");
        app.set_view(View::Diff);
        assert_eq!(app.search, None);
        assert_eq!(app.view, View::Diff);

        let mut app = with_right_pane(App::new(Emacs, 132, 36));
        press(&mut app, "ctrl-s");
        app.set_view(View::Workflows);
        assert!(app.search.is_some());
    }

    #[test]
    fn no_match_stays_readable_after_a_long_query() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc /");
        type_text(&mut app, &"z".repeat(200));
        let s = screen(&app);
        let (row, column) = find(&s, "z (no match)").expect("the whole notice");

        let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
        terminal
            .draw(|frame| crate::draw::draw(&app, frame))
            .unwrap();
        terminal
            .backend_mut()
            .assert_cursor_position((column + 1, row));
    }

    #[test]
    fn opening_a_diff_ends_the_search() {
        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        press(&mut app, "esc ctrl-w l /");
        type_text(&mut app, "args");
        press(&mut app, "enter enter");
        assert!(app.diff.is_some());
        assert_eq!(app.search, None);
        press(&mut app, "esc");
        assert_eq!(app.diff, None);
    }

    #[test]
    fn a_next_pane_that_stays_keeps_the_search() {
        let mut app = App::new(Emacs, 70, 24);
        press(&mut app, "ctrl-s");
        type_text(&mut app, "sign");
        press(&mut app, "ctrl-x o");
        assert_eq!(app.focus, Pane::Agent);
        assert!(app.search.is_some());
    }

    #[test]
    fn an_agent_search_survives_a_resize() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = numbered(40);
        press(&mut app, "esc /");
        type_text(&mut app, "message 3");
        press(&mut app, "enter n");
        for (width, height) in [(70, 24), (40, 10), (60, 16), (1, 1), (132, 36)] {
            app.handle(Event::Resize(width, height));
            screen(&app);
            press(&mut app, "n");
            screen(&app);
        }
        let search = app.search.as_ref().expect("a search");
        assert_eq!(search.pane, Pane::Agent);
        assert!(search.committed);
    }

    #[test]
    fn a_match_on_a_wide_character_highlights_its_cells() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = vec![ChatLine {
            text: "日本語 sign".to_string(),
            tone: Tone::Plain,
            item: None,
            change: None,
        }];
        press(&mut app, "esc /");
        type_text(&mut app, "本");
        let (row, column) = find(&screen(&app), "日").expect("the line");
        // A backend is sent only the first cell of a wide character, which paints both.
        let frame = buffer(&app);
        for (offset, matched) in [(1, false), (2, true), (4, false)] {
            let bg = frame[(column + offset, row)].bg;
            assert_eq!(bg == theme::MATCH, matched, "offset {offset}");
        }
    }

    #[test]
    fn no_match_does_not_panic() {
        let mut app = App::new(Vim, 132, 36);
        session_tab(&mut app);
        press(&mut app, "/");
        type_text(&mut app, "zzz");
        press(&mut app, "enter n N");
        assert_eq!(
            find(&screen(&app), "search: zzz (no match)"),
            Some((26, 30))
        );

        let mut app = App::new(Vim, 60, 16);
        press(&mut app, "esc /");
        type_text(&mut app, &"日本語".repeat(40));
        let s = screen(&app);
        assert!(find(&s, "search: ").is_some());
        assert_eq!(buffer(&app)[(59, 10)].symbol(), "│");
        press(&mut app, "enter n");
    }

    #[test]
    fn slash_means_commands_in_input_and_search_in_normal() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "/");
        assert_eq!(app.slash, Some(0));
        assert_eq!(app.search, None);
        press(&mut app, "bs esc /");
        assert_eq!(app.slash, None);
        assert!(app.search.is_some());
        assert!(app.tabs[0].input.is_empty());
    }

    #[test]
    fn the_line_sits_where_each_pane_keeps_it() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc /");
        type_text(&mut app, "q");
        assert_eq!(find(&screen(&app), "search: q"), Some((30, 30)));

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w h /");
        type_text(&mut app, "q");
        assert_eq!(find(&screen(&app), "search: q"), Some((33, 1)));

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w l ctrl-w l /");
        type_text(&mut app, "q");
        assert_eq!(find(&screen(&app), "search: q"), Some((33, 99)));
    }

    #[test]
    fn a_match_is_never_under_the_line_or_the_message_box() {
        let mut app = App::new(Vim, 60, 16);
        app.tabs[0].lines = numbered(40);
        press(&mut app, "esc /");
        type_text(&mut app, "message 39");
        let s = screen(&app);
        assert_eq!(find(&s, "message 39 ").map(|(row, _)| row), Some(9));
        assert_eq!(find(&s, "search: message 39").map(|(row, _)| row), Some(10));
        assert_eq!(cell_bg(&app, "message 39 ", 0), theme::MATCH);

        press(&mut app, "esc");
        assert_eq!(find(&screen(&app), "message 39 ").map(|(r, _)| r), Some(10));
    }

    #[test]
    fn wide_text_highlights_the_right_cells() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = vec![ChatLine {
            text: "日本語 test".to_string(),
            tone: Tone::Plain,
            item: None,
            change: None,
        }];
        press(&mut app, "esc /");
        type_text(&mut app, "TEST");
        let (row, column) = find(&screen(&app), "test").expect("the text");
        let buf = buffer(&app);
        assert_ne!(buf[(column - 1, row)].bg, theme::MATCH);
        for x in column..column + 4 {
            assert_eq!(buf[(x, row)].bg, theme::MATCH);
        }
        assert_ne!(buf[(column + 4, row)].bg, theme::MATCH);
    }

    #[test]
    fn chip_and_hints_follow_the_search() {
        let hint = |app: &App, text: &str| find(&screen(app), text).map(|(row, _)| row) == Some(35);
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc /");
        assert_eq!(app.chip(), "SEARCH");
        assert!(hint(&app, "enter keep · esc cancel"));
        type_text(&mut app, "x");
        press(&mut app, "enter");
        assert_eq!(app.chip(), "SEARCH");
        assert!(hint(&app, "n next · N previous · esc clear"));

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-s");
        assert_eq!(app.chip(), "SEARCH");
        assert!(hint(
            &app,
            "ctrl-s next · ctrl-r previous · enter end · ctrl-g cancel"
        ));
    }

    #[test]
    fn the_cursor_follows_the_open_line() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc /");
        type_text(&mut app, "ab");
        let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
        terminal
            .draw(|frame| crate::draw::draw(&app, frame))
            .unwrap();
        assert!(terminal.backend().cursor_visible());
        terminal.backend_mut().assert_cursor_position((40, 30));

        press(&mut app, "enter");
        terminal
            .draw(|frame| crate::draw::draw(&app, frame))
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn ctrl_c_on_the_line_clears_it_then_explains() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = numbered(40);
        type_text(&mut app, "draft");
        press(&mut app, "esc /");
        type_text(&mut app, "message 3");
        press(&mut app, "ctrl-c");
        let search = app.search.as_ref().expect("still open");
        assert!(search.query.is_empty());
        assert_eq!(app.tabs[0].scroll, 0);
        assert_eq!(app.tabs[0].input.text(), "draft");
        press(&mut app, "ctrl-c");
        assert_eq!(app.notice.as_deref(), Some("ctrl-c again to quit"));
    }

    #[test]
    fn an_empty_query_has_nothing_to_keep() {
        let mut app = App::new(Vim, 132, 36);
        session_tab(&mut app);
        press(&mut app, "/ enter");
        assert_eq!(app.search, None);
        press(&mut app, "n");
        assert!(app.tabs[0].prompts.is_empty(), "n dismisses the prompt");
    }

    #[test]
    fn the_transcript_and_slash_list_are_left_alone() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = sample_chat();
        press(&mut app, "esc /");
        type_text(&mut app, "n");
        assert!(app.tabs[0].input.is_empty());
        assert_eq!(app.slash, None);

        let mut app = App::new(Emacs, 132, 36);
        type_text(&mut app, "/");
        press(&mut app, "ctrl-s");
        assert_eq!(app.slash, None);
        assert_eq!(app.tabs[0].input.text(), "/");
    }
}
