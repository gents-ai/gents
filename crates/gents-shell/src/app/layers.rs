use ratatui::layout::Rect;

use super::input::LineInput;
use super::{first_selectable, step_row, stepped, App};
use crate::commands::COMMANDS;
use crate::filter::subsequence;
use crate::jump::{self, Target};
use crate::keys::{Action, Dir, KeyCtx};
pub use crate::layout::LayerKind;
use crate::layout::{keys_rows_area, layer_rect, sessions_can_dock, sidebar_can_dock};
use crate::sample::{menu, RowAction, KEY_ROWS};
use crate::source::Agent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub kind: LayerKind,
    pub filter: LineInput,
    pub selected: usize,
}

impl App {
    pub fn picker_matches(&self, query: &str) -> Vec<&Agent> {
        self.agents
            .iter()
            .filter(|agent| subsequence(query, &agent.name))
            .collect()
    }

    /// A kind already in the stack comes to the top. The bar looks only at the top, because
    /// opening it again over a menu is how depth grows.
    pub fn push_layer(&mut self, kind: LayerKind) {
        let from = match kind {
            LayerKind::Jump => self.layers.len().saturating_sub(1),
            _ => 0,
        };
        let found = self.layers[from..].iter().position(|l| l.kind == kind);
        if let Some(offset) = found {
            let layer = self.layers.remove(from + offset);
            self.layers.push(layer);
            return;
        }
        let selected = match kind {
            LayerKind::Menu(id) => first_selectable(menu(id).0),
            _ => 0,
        };
        self.layers.push(Layer {
            kind,
            filter: LineInput::default(),
            selected,
        });
    }

    pub(super) fn toggle_layer(&mut self, kind: LayerKind) {
        match self.layers.iter().position(|layer| layer.kind == kind) {
            Some(index) => {
                self.layers.remove(index);
            }
            None => self.push_layer(kind),
        }
    }

    pub fn top_layer_rect(&self) -> Option<Rect> {
        let area = self.panes()?.above_status();
        let top = self.layers.last()?;
        let depth = (self.layers.len() - 1) as u16;
        Some(layer_rect(top.kind, area, depth, true))
    }

    pub(super) fn act_on_layer(&mut self, action: Action) {
        let width = self.size.0;
        let Some(top) = self.layers.last_mut() else {
            return;
        };
        match action {
            Action::Insert(_) | Action::Backspace | Action::Delete | Action::DeleteToStart => {
                top.filter.edit(action);
                top.selected = 0;
            }
            Action::CursorLeft
            | Action::CursorRight
            | Action::WordLeft
            | Action::WordRight
            | Action::Home
            | Action::End => {
                top.filter.edit(action);
            }
            Action::Submit => self.submit_layer(),
            Action::Back => self.back(),
            Action::Interrupt => {
                if !top.filter.is_empty() {
                    top.selected = 0;
                }
                self.interrupt();
            }
            Action::MoveFocus(Dir::Left | Dir::Up) => self.layers.rotate_left(1),
            Action::MoveFocus(_) | Action::NextPane => self.layers.rotate_right(1),
            Action::Next => self.step_layer(true),
            Action::Prev => self.step_layer(false),
            Action::NotWired if matches!(top.kind, LayerKind::Menu(_)) => self.not_wired(),
            Action::ClosePane => {
                self.layers.pop();
            }
            Action::ToggleSessions if !sessions_can_dock(width) => {
                self.toggle_layer(LayerKind::Sessions);
            }
            Action::ToggleSidebar if !sidebar_can_dock(width) => {
                self.toggle_layer(LayerKind::Sidebar);
            }
            Action::NextView if top.kind == LayerKind::Sidebar => self.next_view(),
            Action::OpenBar if self.ctx() == KeyCtx::Menu => self.push_layer(LayerKind::Jump),
            Action::NewTab => self.push_layer(LayerKind::Picker),
            Action::OpenKeys => self.push_layer(LayerKind::Keys),
            Action::Quit => self.quit = true,
            _ => {}
        }
    }

    pub(super) fn submit_layer(&mut self) {
        let Some(top) = self.layers.last() else {
            return;
        };
        match top.kind {
            LayerKind::Picker => {
                let matches = self.picker_matches(top.filter.text());
                let picked = matches
                    .get(top.selected)
                    .and_then(|picked| self.agents.iter().position(|agent| agent.id == picked.id));
                if let Some(agent) = picked {
                    self.open_tab_with_agent(agent);
                }
            }
            LayerKind::Menu(id) => match menu(id).0[top.selected].action {
                RowAction::OpenMenu(next) => self.push_layer(LayerKind::Menu(next)),
                _ => self.not_wired(),
            },
            LayerKind::Sessions => self.activate_session_row(),
            LayerKind::Sidebar => self.activate_view_row(),
            LayerKind::Jump => self.submit_jump(top.selected),
            LayerKind::Keys => {}
        }
    }

    fn submit_jump(&mut self, selected: usize) {
        let Some(item) = self.jump_items().into_iter().nth(selected) else {
            return;
        };
        match item.target {
            Target::Tab(index) => {
                self.layers.clear();
                self.activate_tab(index);
            }
            Target::Session(row) => self.open_session(row),
            Target::Agent(agent) => self.open_tab_with_agent(agent),
            Target::Menu(id) => self.push_layer(LayerKind::Menu(id)),
            Target::Command(index) => self.run_effect(COMMANDS[index].effect),
        }
    }

    pub(super) fn jump_items(&self) -> Vec<jump::JumpItem> {
        let query = self.layers.last().map_or("", |top| top.filter.text());
        jump::items(self, query)
    }

    pub(super) fn step_layer(&mut self, forward: bool) {
        let rect = self.top_layer_rect();
        let Some(top) = self.layers.last_mut() else {
            return;
        };
        match top.kind {
            LayerKind::Picker => {
                let query = top.filter.text().to_string();
                let max = self.picker_matches(&query).len().saturating_sub(1);
                if let Some(top) = self.layers.last_mut() {
                    top.selected = stepped(top.selected, forward, max);
                }
            }
            LayerKind::Menu(id) => top.selected = step_row(menu(id).0, top.selected, forward),
            LayerKind::Keys => {
                if let Some(rect) = rect {
                    let height = usize::from(keys_rows_area(rect).height);
                    let max = KEY_ROWS.len().saturating_sub(height);
                    top.selected = stepped(top.selected, forward, max);
                }
            }
            LayerKind::Sessions => self.step_session(forward),
            LayerKind::Sidebar => self.step_sidebar(forward),
            LayerKind::Jump => {
                let max = self.jump_items().len().saturating_sub(1);
                if let Some(top) = self.layers.last_mut() {
                    top.selected = stepped(top.selected, forward, max);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Mode, Pane};
    use crate::draw::theme;
    use crate::keys::KeySet::{Emacs, Vim};
    use crate::sample::{MenuId, View};
    use crate::testing::{buffer, find, kinds, press, screen, type_text, with_right_pane};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::Event;
    use ratatui::Terminal;

    const ACCOUNTS: LayerKind = LayerKind::Menu(MenuId::Accounts);
    const TOOLS: LayerKind = LayerKind::Menu(MenuId::Tools);

    fn top(app: &App) -> LayerKind {
        app.layers.last().expect("a layer").kind
    }

    const NODE: LayerKind = LayerKind::Menu(MenuId::NodeSettings);

    fn row_named(id: MenuId, name: &str) -> usize {
        menu(id)
            .0
            .iter()
            .position(|row| row.text.trim() == name)
            .expect(name)
    }

    #[test]
    fn the_bar_opens_both_settings_menus() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "ctrl-p enter");
        assert_eq!(top(&app), NODE);
        press(&mut app, "esc ctrl-n enter");
        assert_eq!(top(&app), LayerKind::Menu(MenuId::AppSettings));
    }

    #[test]
    fn node_settings_lists_every_desktop_section() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(NODE);
        let s = screen(&app);
        for text in [
            " CONFIGURE ",
            " Node ",
            " Agents ",
            " Skills ",
            " Providers ",
            " AUTOMATION ",
            " Tasks ",
            " Triggers ",
            " Schedules ",
            " Event sources ",
            " Workflows ",
            " TOOLS ",
            " Tools ",
            " Remote Tools ",
            " Allowed folders ",
            " PACKS ",
            " Packs ",
            " UX Plugins ",
        ] {
            assert!(find(&s, text).is_some(), "{text}");
        }
    }

    #[test]
    fn app_settings_lists_its_three_sections() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Menu(MenuId::AppSettings));
        let s = screen(&app);
        for text in ["General", "Developer", "Experimental features"] {
            assert!(find(&s, text).is_some(), "{text}");
        }
    }

    #[test]
    fn a_section_with_a_menu_stacks_it() {
        for (name, opens) in [
            ("Agents", MenuId::Agents),
            ("Providers", MenuId::Accounts),
            ("Tasks", MenuId::Tasks),
            ("Tools", MenuId::Tools),
        ] {
            let mut app = App::new(Vim, 132, 36);
            app.push_layer(NODE);
            app.layers[0].selected = row_named(MenuId::NodeSettings, name);
            press(&mut app, "enter");
            assert_eq!(kinds(&app), [NODE, LayerKind::Menu(opens)]);
            press(&mut app, "esc");
            assert_eq!(kinds(&app), [NODE]);
        }
    }

    #[test]
    fn other_sections_say_not_wired() {
        for name in ["Node", "Skills", "Triggers", "Packs", "UX Plugins"] {
            let mut app = App::new(Vim, 132, 36);
            app.push_layer(NODE);
            app.layers[0].selected = row_named(MenuId::NodeSettings, name);
            press(&mut app, "enter");
            assert_eq!(app.notice.as_deref(), Some("not wired yet"), "{name}");
            assert_eq!(kinds(&app), [NODE]);
        }
    }

    #[test]
    fn every_settings_row_can_be_reached_at_60_by_16() {
        let mut app = App::new(Vim, 60, 16);
        app.push_layer(NODE);
        let rows = menu(MenuId::NodeSettings).0;
        for row in rows.iter().filter(|row| row.selectable()) {
            assert!(
                find(&screen(&app), row.text.trim()).is_some(),
                "{}",
                row.text
            );
            press(&mut app, "j");
        }
        assert_eq!(app.layers[0].selected, rows.len() - 1);
    }

    #[test]
    fn undocked_toggle_opens_a_layer_and_leaves_wanted() {
        let mut app = with_right_pane(App::new(Vim, 70, 24));
        press(&mut app, "esc ctrl-w [");
        assert_eq!(top(&app), LayerKind::Sessions);
        assert!(app.wanted().sessions);
        let s = screen(&app);
        assert!(find(&s, "Sessions").is_some());
        assert_eq!(find(&s, "MENU"), Some((23, 1)));
        press(&mut app, "esc");
        assert!(app.layers.is_empty());

        let mut app = with_right_pane(App::new(Vim, 104, 30));
        press(&mut app, "esc ctrl-w ]");
        assert_eq!(top(&app), LayerKind::Sidebar);
        assert!(app.wanted().sidebar);
        press(&mut app, "ctrl-w ]");
        assert!(app.layers.is_empty());

        let mut app = with_right_pane(App::new(Emacs, 70, 24));
        press(&mut app, "ctrl-x ]");
        assert_eq!(top(&app), LayerKind::Sidebar);
        press(&mut app, "ctrl-x ]");
        assert!(app.layers.is_empty());
    }

    #[test]
    fn a_wanted_flag_set_off_still_opens_the_layer() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w ]");
        app.handle(Event::Resize(104, 30));
        press(&mut app, "ctrl-w ]");
        assert_eq!(top(&app), LayerKind::Sidebar);
        assert!(!app.wanted().sidebar);
    }

    #[test]
    fn ctrl_w_with_nothing_on_that_side_opens_the_layer() {
        let mut app = with_right_pane(App::new(Vim, 70, 24));
        press(&mut app, "esc ctrl-w h");
        assert_eq!(top(&app), LayerKind::Sessions);
        assert_eq!(app.focus, Pane::Agent);

        let mut app = with_right_pane(App::new(Vim, 70, 24));
        press(&mut app, "esc ctrl-w l");
        assert_eq!(top(&app), LayerKind::Sidebar);

        let mut app = with_right_pane(App::new(Vim, 104, 30));
        press(&mut app, "esc ctrl-w l");
        assert_eq!(top(&app), LayerKind::Sidebar);

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w ]");
        press(&mut app, "ctrl-w l");
        assert!(app.layers.is_empty());
        assert_eq!(app.focus, Pane::Agent);

        let mut app = with_right_pane(App::new(Vim, 104, 30));
        press(&mut app, "esc ctrl-w [");
        press(&mut app, "ctrl-w h");
        assert!(app.layers.is_empty());
    }

    #[test]
    fn widening_closes_the_layer_and_docks_if_wanted() {
        let mut app = App::new(Vim, 70, 24);
        press(&mut app, "esc ctrl-w [");
        app.handle(Event::Resize(132, 36));
        assert!(app.layers.is_empty());
        assert_eq!(find(&screen(&app), "Sessions"), Some((0, 3)));

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc ctrl-w [");
        app.handle(Event::Resize(70, 24));
        press(&mut app, "ctrl-w [");
        assert_eq!(top(&app), LayerKind::Sessions);
        app.handle(Event::Resize(132, 36));
        assert!(app.layers.is_empty());
        assert_eq!(find(&screen(&app), "Sessions"), None);

        let mut app = App::new(Vim, 70, 24);
        press(&mut app, "esc ctrl-w [");
        app.push_layer(ACCOUNTS);
        app.handle(Event::Resize(132, 36));
        assert_eq!(kinds(&app), [ACCOUNTS]);

        let mut app = App::new(Vim, 70, 24);
        press(&mut app, "esc ctrl-w ]");
        app.handle(Event::Resize(104, 30));
        assert_eq!(top(&app), LayerKind::Sidebar);
        app.handle(Event::Resize(120, 30));
        assert!(app.layers.is_empty());
    }

    #[test]
    fn top_layer_owns_the_keyboard_and_movement_cycles() {
        for (set, cycle) in [(Vim, "ctrl-w l"), (Emacs, "ctrl-x o")] {
            let mut app = App::new(set, 132, 36);
            app.push_layer(ACCOUNTS);
            app.push_layer(TOOLS);
            press(&mut app, "esc");
            assert_eq!(kinds(&app), [ACCOUNTS]);
            app.push_layer(TOOLS);
            press(&mut app, cycle);
            assert_eq!(kinds(&app), [TOOLS, ACCOUNTS]);
            press(&mut app, cycle);
            assert_eq!(kinds(&app), [ACCOUNTS, TOOLS]);
        }

        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        app.push_layer(TOOLS);
        press(&mut app, "ctrl-w h");
        assert_eq!(app.focus, Pane::Agent);
        assert_eq!(kinds(&app), [TOOLS, ACCOUNTS]);

        let mut app = App::new(Emacs, 132, 36);
        app.push_layer(ACCOUNTS);
        type_text(&mut app, "x");
        assert!(app.tabs[0].input.is_empty());
    }

    #[test]
    fn three_layers_cycle_both_ways() {
        let mut app = App::new(Vim, 132, 36);
        let status = LayerKind::Menu(MenuId::Status);
        app.push_layer(ACCOUNTS);
        app.push_layer(TOOLS);
        app.push_layer(status);
        press(&mut app, "ctrl-w l");
        assert_eq!(kinds(&app), [status, ACCOUNTS, TOOLS]);
        press(&mut app, "ctrl-w h");
        assert_eq!(kinds(&app), [ACCOUNTS, TOOLS, status]);
        press(&mut app, "ctrl-w k");
        assert_eq!(kinds(&app), [TOOLS, status, ACCOUNTS]);
        press(&mut app, "ctrl-w j");
        assert_eq!(kinds(&app), [ACCOUNTS, TOOLS, status]);
    }

    #[test]
    fn toggles_at_a_docking_width_wait_while_a_layer_is_open() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        app.push_layer(ACCOUNTS);
        press(&mut app, "ctrl-w ]");
        assert!(app.wanted().sidebar);
        assert_eq!(kinds(&app), [ACCOUNTS]);
    }

    #[test]
    fn close_pane_closes_the_top_layer() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        press(&mut app, "ctrl-w q");
        assert!(app.layers.is_empty());
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn a_layer_on_top_of_itself_does_not_stack() {
        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-x t ctrl-x t");
        assert_eq!(kinds(&app), [LayerKind::Picker]);
        press(&mut app, "ctrl-x ?");
        assert_eq!(kinds(&app), [LayerKind::Picker, LayerKind::Keys]);
    }

    #[test]
    fn a_kind_in_the_stack_comes_to_the_top_instead_of_stacking() {
        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-x t");
        type_text(&mut app, "re");
        press(&mut app, "ctrl-x ? ctrl-x t");
        assert_eq!(kinds(&app), [LayerKind::Keys, LayerKind::Picker]);
        assert_eq!(app.layers[1].filter.text(), "re");
        press(&mut app, "ctrl-x ? ctrl-x t ctrl-x ?");
        assert_eq!(kinds(&app), [LayerKind::Picker, LayerKind::Keys]);

        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        app.push_layer(TOOLS);
        app.push_layer(ACCOUNTS);
        assert_eq!(kinds(&app), [TOOLS, ACCOUNTS]);
    }

    #[test]
    fn only_the_bar_grows_the_stack() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        app.push_layer(LayerKind::Jump);
        app.push_layer(ACCOUNTS);
        app.push_layer(LayerKind::Jump);
        assert_eq!(kinds(&app), [LayerKind::Jump, ACCOUNTS, LayerKind::Jump]);
        app.push_layer(LayerKind::Jump);
        assert_eq!(app.layers.len(), 3);
    }

    #[test]
    fn d_says_not_wired_only_in_a_menu() {
        for kind in [LayerKind::Keys, LayerKind::Sessions, LayerKind::Sidebar] {
            let mut app = App::new(Vim, 70, 24);
            app.push_layer(kind);
            press(&mut app, "d");
            assert_eq!(app.notice, None, "{kind:?}");
        }
    }

    #[test]
    fn menu_actions_say_not_wired() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        assert!(find(&screen(&app), "signed in").is_some());
        press(&mut app, "j");
        assert_eq!(app.layers[0].selected, 1);
        for key in ["enter", "d"] {
            press(&mut app, key);
            assert_eq!(app.notice.as_deref(), Some("not wired yet"), "{key}");
            assert_eq!(kinds(&app), [ACCOUNTS]);
        }
    }

    #[test]
    fn menu_rows_skip_blanks_and_clamp() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Menu(MenuId::Agents));
        press(&mut app, "j j j");
        assert_eq!(app.layers[0].selected, 4);
        press(&mut app, "j j");
        assert_eq!(app.layers[0].selected, 4);
        press(&mut app, "k k k k k");
        assert_eq!(app.layers[0].selected, 0);
    }

    #[test]
    fn every_menu_shows_its_rows_and_footer() {
        for (id, text, footer) in [
            (MenuId::Accounts, "Claude    subscription", "enter sign in"),
            (MenuId::Agents, "+ New agent", "enter open"),
            (MenuId::Tools, "code review pack   3 tools", "enter open"),
            (MenuId::Tasks, "nightly eval     02:00", "enter open"),
            (MenuId::Mailbox, "approve: cargo test", "d dismiss"),
            (
                MenuId::Status,
                "runtime    not connected",
                "nothing here is wired",
            ),
        ] {
            let mut app = App::new(Vim, 60, 16);
            app.push_layer(LayerKind::Menu(id));
            let s = screen(&app);
            assert!(find(&s, id.name()).is_some(), "{id:?}");
            assert!(find(&s, text).is_some(), "{id:?}");
            assert!(find(&s, footer).is_some(), "{id:?}");
        }
    }

    #[test]
    fn footers_name_the_active_key_set() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        assert!(find(&screen(&app), "d disable · esc back").is_some());

        let mut app = App::new(Emacs, 132, 36);
        app.push_layer(ACCOUNTS);
        let s = screen(&app);
        assert!(find(&s, "d disable · ctrl-g back").is_some());
        assert_eq!(find(&s, "esc"), None);
    }

    #[test]
    fn dims_the_shell_beneath() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        let buf = buffer(&app);
        let s = screen(&app);
        let (row, column) = find(&s, "cli ux redesign").expect("sessions row");
        assert_eq!(buf[(column, row)].fg, theme::DIM);
        assert_eq!(buf[(column, row)].bg, theme::BG);
        let (row, column) = find(&s, "Accounts").expect("layer title");
        assert_ne!(buf[(column, row)].fg, theme::DIM);
        assert!(find(&s, "sample data").is_some());
        assert_eq!(find(&s, "MENU"), Some((35, 1)));
        let (row, column) = find(&s, "sample data").expect("status");
        assert_eq!(buf[(column, row)].fg, theme::ACCENT);
    }

    #[test]
    fn only_the_top_layer_has_a_focused_border() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        app.push_layer(TOOLS);
        let buf = buffer(&app);
        let s = screen(&app);
        let (row, column) = find(&s, "Tools").expect("top title");
        assert_eq!(buf[(column, row)].bg, theme::FOCUS);
        let (row, column) = find(&s, "Accounts").expect("lower title");
        assert_eq!(buf[(column, row)].fg, theme::TITLE);
        assert_ne!(buf[(column, row)].bg, theme::FOCUS);
    }

    #[test]
    fn a_layer_hides_what_is_beneath_it() {
        let mut app = App::new(Vim, 132, 36);
        app.tabs[0].lines = crate::app::sample_chat();
        press(&mut app, "esc ?");
        let s = screen(&app);
        assert_eq!(find(&s, "Moving them"), None);
        assert!(find(&s, "Group the sign-in").is_some());
    }

    #[test]
    fn no_cursor_while_a_layer_is_open() {
        let mut app = App::new(Emacs, 132, 36);
        app.push_layer(ACCOUNTS);
        let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
        terminal
            .draw(|frame| crate::draw::draw(&app, frame))
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn a_filter_layer_draws_its_cursor_at_the_display_column() {
        for (depth, kind, x, y) in [
            (0, LayerKind::Jump, 34, 6),
            (1, LayerKind::Jump, 38, 8),
            (0, LayerKind::Picker, 36, 8),
        ] {
            let mut app = App::new(Emacs, 132, 36);
            if depth == 1 {
                app.push_layer(ACCOUNTS);
            }
            app.push_layer(kind);
            let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
            let mut at = |app: &App, column: u16| {
                terminal
                    .draw(|frame| crate::draw::draw(app, frame))
                    .unwrap();
                assert!(terminal.backend().cursor_visible(), "{kind:?}");
                terminal
                    .backend_mut()
                    .assert_cursor_position((x + column, y));
            };
            at(&app, 0);
            type_text(&mut app, "a日本");
            at(&app, 5);
            press(&mut app, "left");
            at(&app, 3);
            press(&mut app, "home");
            at(&app, 0);
            press(&mut app, "end");
            at(&app, 5);
        }
    }

    #[test]
    fn a_long_filter_keeps_the_cursor_inside_the_prompt() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Jump);
        type_text(&mut app, &"x".repeat(200));
        let mut terminal = Terminal::new(TestBackend::new(132, 36)).unwrap();
        terminal
            .draw(|frame| crate::draw::draw(&app, frame))
            .unwrap();
        terminal.backend_mut().assert_cursor_position((100, 6));
    }

    #[test]
    fn chips_and_hints_follow_the_layer_and_key_set() {
        for (set, kind, chip, hint) in [
            (
                Vim,
                ACCOUNTS,
                "MENU",
                "esc back one layer · ctrl-w h/j/k/l cycles layers",
            ),
            (
                Emacs,
                ACCOUNTS,
                "MENU",
                "ctrl-g back one layer · ctrl-x o cycles layers",
            ),
            (
                Vim,
                LayerKind::Picker,
                "MENU",
                "each tab keeps its own agent, side by side",
            ),
            (
                Emacs,
                LayerKind::Picker,
                "MENU",
                "each tab keeps its own agent, side by side",
            ),
            (Vim, LayerKind::Keys, "HELP", "j/k scroll · esc close"),
            (
                Emacs,
                LayerKind::Keys,
                "HELP",
                "ctrl-n/ctrl-p scroll · ctrl-g close",
            ),
            (
                Vim,
                LayerKind::Sessions,
                "MENU",
                "esc close · j/k move · enter open",
            ),
            (
                Emacs,
                LayerKind::Sidebar,
                "MENU",
                "ctrl-g close · ctrl-n/ctrl-p move · enter open",
            ),
        ] {
            let mut app = App::new(set, 132, 36);
            app.push_layer(kind);
            let s = screen(&app);
            assert_eq!(find(&s, chip), Some((35, 1)), "{kind:?}");
            assert_eq!(find(&s, hint).map(|(row, _)| row), Some(35), "{kind:?}");
        }
    }

    #[test]
    fn picker_opens_a_tab_and_back_cancels() {
        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-x t");
        assert!(find(&screen(&app), "New tab: pick an agent").is_some());
        type_text(&mut app, "rev");
        press(&mut app, "enter");
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active, 1);
        assert_eq!(app.tabs[1].name, "new session");
        assert_eq!(app.tabs[1].agent, "Reviewer");
        assert!(app.layers.is_empty());

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-x t esc");
        assert_eq!(app.tabs.len(), 1);
        assert!(app.layers.is_empty());

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc");
        app.focus = Pane::Sessions;
        app.sessions_sel = 0;
        press(&mut app, "enter");
        assert_eq!(top(&app), LayerKind::Picker);
        press(&mut app, "ctrl-n enter");
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[1].agent, "Reviewer");
        assert_eq!(app.focus, Pane::Agent);
        assert_eq!(app.mode, Mode::Input);
    }

    #[test]
    fn picker_lists_the_agents_and_filters_them() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Picker);
        let s = screen(&app);
        assert!(find(&s, "Engineer     default").is_some());
        assert!(find(&s, "Triage       signed out").is_some());
        assert!(find(&s, "enter open in a new tab · esc back").is_some());
        assert_eq!(find(&s, " > "), Some((8, 33)));

        type_text(&mut app, "rev");
        let s = screen(&app);
        assert!(find(&s, " > rev").is_some());
        assert!(find(&s, "Reviewer     ").is_some());
        assert_eq!(find(&s, "Engineer     default"), None);
    }

    #[test]
    fn picker_selection_clamps_and_a_new_filter_goes_back_to_the_first_row() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Picker);
        press(&mut app, "ctrl-n ctrl-n ctrl-n ctrl-n ctrl-n");
        assert_eq!(app.layers[0].selected, 3);
        press(&mut app, "up");
        assert_eq!(app.layers[0].selected, 2);
        type_text(&mut app, "e");
        assert_eq!(app.layers[0].selected, 0);
    }

    #[test]
    fn filter_layers_take_unicode_text() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Picker);
        type_text(&mut app, "日本");
        press(&mut app, "bs");
        assert_eq!(app.layers[0].filter.text(), "日");
        assert!(find(&screen(&app), "no matches").is_some());
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [LayerKind::Picker]);
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn keys_overlay_at_each_width() {
        for (set, open) in [(Vim, "esc ?"), (Emacs, "ctrl-x ?")] {
            let mut app = App::new(set, 132, 36);
            press(&mut app, open);
            assert_eq!(top(&app), LayerKind::Keys);
            let s = screen(&app);
            for text in [
                "Move between panes",
                "ctrl-w h/j/k/l",
                "ctrl-x o",
                "vim",
                "emacs",
            ] {
                assert!(find(&s, text).is_some(), "{text}");
            }
            assert!(find(&s, "Sample bindings to react to. Switch sets").is_some());
            assert_eq!(find(&s, "HELP"), Some((35, 1)));
            assert_eq!(find(&s, "clears, or says how to quit"), Some((25, 61)));
        }

        let mut app = App::new(Vim, 60, 16);
        press(&mut app, "esc ?");
        let s = screen(&app);
        assert!(find(&s, "ctrl-w h/j/k/l").is_some());
        assert_eq!(find(&s, "ctrl-x o"), None);
        assert_eq!(find(&s, "Quit"), None);
        assert!(find(&s, "jump").is_some());
        for _ in 0..30 {
            press(&mut app, "j");
        }
        let s = screen(&app);
        assert!(find(&s, "Quit").is_some());
        assert!(find(&s, "clears, or says how to quit").is_some());
        assert_eq!(find(&s, "ctrl-w h/j/k/l"), None);
        press(&mut app, "k");
        assert!(find(&screen(&app), "Scroll the focused pane").is_some());

        let mut app = App::new(Emacs, 60, 16);
        press(&mut app, "ctrl-x ?");
        let s = screen(&app);
        assert!(find(&s, "ctrl-x o").is_some());
        assert_eq!(find(&s, "ctrl-w h/j/k/l"), None);
    }

    #[test]
    fn the_keys_hint_says_the_hidden_row_is_a_scroll_away() {
        for (set, open, down) in [(Vim, "esc ?", "j"), (Emacs, "ctrl-x ?", "ctrl-n")] {
            let mut app = App::new(set, 80, 24);
            press(&mut app, open);
            let last = "clears, or says how to quit";
            assert_eq!(find(&screen(&app), last), None);
            press(&mut app, down);
            assert!(find(&screen(&app), last).is_some());
        }
    }

    #[test]
    fn the_keys_footnote_warns_about_ctrl_w_only_in_vim() {
        let warning = "ctrl-w deletes a word";
        for (width, height) in [(132, 36), (60, 16)] {
            let mut app = App::new(Vim, width, height);
            press(&mut app, "esc ?");
            assert!(find(&screen(&app), warning).is_some(), "{width}");

            let mut app = App::new(Emacs, width, height);
            press(&mut app, "ctrl-x ?");
            assert_eq!(find(&screen(&app), warning), None, "{width}");
        }
    }

    #[test]
    fn the_keys_layer_fits_its_longest_row_in_a_narrow_window() {
        let mut app = App::new(Emacs, 60, 16);
        press(&mut app, "ctrl-x ?");
        for _ in 0..30 {
            press(&mut app, "ctrl-n");
        }
        let s = screen(&app);
        assert!(find(&s, "clears, or says how to quit").is_some());
        assert!(find(&s, "ctrl-x ctrl-c").is_some());
    }

    #[test]
    fn keys_layer_scrolls_back_at_once_after_the_window_grows() {
        let mut app = App::new(Vim, 60, 16);
        press(&mut app, "esc ?");
        for _ in 0..30 {
            press(&mut app, "j");
        }
        app.handle(Event::Resize(132, 36));
        press(&mut app, "k");
        assert_eq!(app.layers[0].selected, 0);
    }

    #[test]
    fn resize_through_too_small_keeps_state() {
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "hi");
        app.push_layer(ACCOUNTS);
        app.handle(Event::Resize(40, 10));
        assert!(find(&screen(&app), "Window too small").is_some());
        press(&mut app, "esc");
        assert!(app.layers.is_empty());
        app.handle(Event::Resize(132, 36));
        assert!(app.layers.is_empty());
        assert_eq!(app.tabs[0].input.text(), "hi");

        let mut app = App::new(Emacs, 132, 36);
        app.push_layer(ACCOUNTS);
        app.handle(Event::Resize(40, 10));
        press(&mut app, "ctrl-x ctrl-c");
        assert!(app.should_quit());

        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        app.handle(Event::Resize(1, 1));
        screen(&app);
    }

    #[test]
    fn vim_quits_on_the_too_small_screen() {
        let mut app = App::new(Vim, 132, 36);
        app.handle(Event::Resize(40, 10));
        press(&mut app, "esc : q enter");
        assert!(app.should_quit());

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc");
        app.push_layer(ACCOUNTS);
        app.handle(Event::Resize(40, 10));
        press(&mut app, "esc : q enter");
        assert!(app.should_quit());
    }

    #[test]
    fn a_resize_that_drops_a_layer_cancels_a_held_chord() {
        let mut app = App::new(Vim, 70, 24);
        type_text(&mut app, "draft");
        press(&mut app, "esc ctrl-w [ ctrl-w");
        assert_eq!(app.keymap.pending(), Some("ctrl-w ..."));
        app.handle(Event::Resize(132, 36));
        assert!(app.layers.is_empty());
        assert_eq!(app.keymap.pending(), None);
        press(&mut app, "q");
        assert_eq!(app.tabs[0].input.text(), "draft");

        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        press(&mut app, "ctrl-w");
        app.handle(Event::Resize(70, 24));
        assert_eq!(app.keymap.pending(), Some("ctrl-w ..."));
    }

    #[test]
    fn a_layer_survives_a_short_stop_at_too_small() {
        for through in [(40, 10), (100, 10)] {
            let mut app = App::new(Vim, 70, 24);
            press(&mut app, "esc ctrl-w [");
            app.handle(Event::Resize(through.0, through.1));
            assert_eq!(top(&app), LayerKind::Sessions);
            app.handle(Event::Resize(70, 24));
            assert_eq!(top(&app), LayerKind::Sessions);
        }
    }

    #[test]
    fn sessions_layer_opens_a_session() {
        let mut app = App::new(Vim, 70, 24);
        press(&mut app, "esc ctrl-w [");
        let s = screen(&app);
        assert!(find(&s, "Sessions").is_some());
        for row in app.session_rows().iter().filter(|row| !row.text.is_empty()) {
            assert!(find(&s, row.text.trim()).is_some(), "{}", row.text);
        }
        press(&mut app, "j enter");
        assert!(app.layers.is_empty());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].name, "flaky");
    }

    #[test]
    fn new_session_row_in_a_sessions_layer_stacks_the_picker() {
        let mut app = App::new(Vim, 70, 24);
        press(&mut app, "esc ctrl-w [");
        app.sessions_sel = 0;
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [LayerKind::Sessions, LayerKind::Picker]);
        press(&mut app, "esc");
        assert_eq!(kinds(&app), [LayerKind::Sessions]);
    }

    #[test]
    fn sidebar_layer_opens_a_diff_and_back_closes_the_diff_first() {
        let mut app = App::new(Vim, 104, 30);
        press(&mut app, "esc ctrl-w ]");
        assert!(find(&screen(&app), "3 files").is_some());
        press(&mut app, "enter");
        assert!(find(&screen(&app), "@@ -85,8 +85,4 @@").is_some());
        press(&mut app, "esc");
        assert_eq!(app.diff, None);
        assert_eq!(kinds(&app), [LayerKind::Sidebar]);
        assert!(find(&screen(&app), "3 files").is_some());
        press(&mut app, "esc");
        assert!(app.layers.is_empty());
    }

    #[test]
    fn sidebar_layer_changes_view_and_reaches_a_tall_diffs_last_row() {
        let mut app = App::new(Vim, 60, 16);
        press(&mut app, "esc ctrl-w ]");
        press(&mut app, "enter");
        assert_eq!(find(&screen(&app), "esc back to files"), None);
        press(&mut app, "j");
        assert!(find(&screen(&app), "esc back to files").is_some());
        press(&mut app, "esc ctrl-w }");
        assert_eq!(app.view, View::Workflows);
        assert!(find(&screen(&app), "2 running").is_some());
        press(&mut app, "ctrl-w }");
        assert_eq!(app.view, View::Agents);
        press(&mut app, "esc ctrl-w }");
        assert_eq!(app.view, View::Agents);
        assert!(app.notice.is_some());
    }

    #[test]
    fn next_view_is_left_to_the_sidebar_layer() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        press(&mut app, "ctrl-w }");
        assert_eq!(app.view, View::Changes);
    }

    #[test]
    fn back_order_with_nothing_above_a_docked_diff() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w l ctrl-w l enter");
        app.push_layer(ACCOUNTS);
        press(&mut app, "esc");
        assert!(app.layers.is_empty());
        assert_eq!(app.diff, Some(0));
        press(&mut app, "esc");
        assert_eq!(app.diff, None);
    }

    #[test]
    fn ctrl_c_in_a_filter_clears_it_then_explains() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(LayerKind::Picker);
        type_text(&mut app, "rev");
        press(&mut app, "ctrl-n");
        press(&mut app, "ctrl-c");
        assert!(app.layers[0].filter.is_empty());
        assert_eq!(app.layers[0].selected, 0);
        assert_eq!(kinds(&app), [LayerKind::Picker]);
        assert_eq!(app.notice, None);
        press(&mut app, "ctrl-c");
        assert_eq!(
            app.notice.as_deref(),
            Some("esc twice, then :q then enter to quit")
        );
        assert_eq!(kinds(&app), [LayerKind::Picker]);
    }

    #[test]
    fn ctrl_c_in_a_menu_keeps_the_draft() {
        let mut app = App::new(Emacs, 132, 36);
        type_text(&mut app, "hi");
        app.push_layer(ACCOUNTS);
        press(&mut app, "ctrl-c");
        assert_eq!(app.tabs[0].input.text(), "hi");
        assert_eq!(app.notice.as_deref(), Some("ctrl-x ctrl-c to quit"));
    }
}
