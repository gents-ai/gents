use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};

use super::layers::LayerKind;
use super::selection::Selection;
use super::{App, Pane};
use crate::draw::agent::{chip_at, first_line, message_box_area, slash_area, slash_rows, ChipHit};
use crate::draw::layers::{list_area, list_start};
use crate::draw::sidebar::{first_diff_row, scope_at};
use crate::keys::Action;
use crate::layout::{running_area, sidebar_rows_area, Right};
use crate::sample::{MenuId, SidebarRow, Tone, View};

impl App {
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        let at = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                self.click(at);
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag(at),
            MouseEventKind::Up(MouseButton::Left) => self.release(),
            MouseEventKind::ScrollUp => self.wheel(at, false),
            MouseEventKind::ScrollDown => self.wheel(at, true),
            _ => {}
        }
    }

    fn click(&mut self, at: Position) {
        let Some(panes) = self.panes() else {
            return;
        };
        if !self.layers.is_empty() {
            self.click_layer(at);
        } else if let Some(rect) = panes.sessions.filter(|rect| rect.contains(at)) {
            self.focus_pane(Pane::Sessions);
            self.click_sessions(rect, at);
        } else if panes.agent.contains(at) {
            self.click_agent(panes.agent, at);
        } else {
            match panes.right {
                Right::Sidebar(rect) if rect.contains(at) => {
                    self.focus_pane(Pane::Sidebar);
                    self.click_sidebar(rect, at);
                }
                Right::Rail(rect) if rect.contains(at) => {
                    self.showing_right_pane(|app| app.push_layer(LayerKind::Sidebar));
                }
                _ => {}
            }
        }
    }

    fn click_layer(&mut self, at: Position) {
        let (Some(rect), Some(top)) = (self.top_layer_rect(), self.layers.last()) else {
            return;
        };
        if !rect.contains(at) {
            self.layers.pop();
            return;
        }
        match top.kind {
            LayerKind::Sessions => self.click_sessions(rect, at),
            LayerKind::Sidebar => self.click_sidebar(rect, at),
            LayerKind::Picker => self.click_picker(rect, at),
            LayerKind::Menu(id) => self.click_menu(id, rect, at),
            LayerKind::Jump => self.click_jump(rect, at),
            LayerKind::Keys | LayerKind::Setup | LayerKind::Form => {}
        }
    }

    fn click_picker(&mut self, rect: Rect, at: Position) {
        let list = list_area(LayerKind::Picker, rect);
        let Some(top) = self.layers.last().filter(|_| list.contains(at)) else {
            return;
        };
        let first = list_start(top.selected, usize::from(list.height));
        let index = first + usize::from(at.y - list.y);
        if index < self.picker_matches(top.filter.text()).len() {
            if let Some(top) = self.layers.last_mut() {
                top.selected = index;
            }
            self.submit_layer();
        }
    }

    fn click_menu(&mut self, id: MenuId, rect: Rect, at: Position) {
        let list = list_area(LayerKind::Menu(id), rect);
        let rows = self.menu_rows(id).0;
        let Some(top) = self.layers.last_mut().filter(|_| list.contains(at)) else {
            return;
        };
        let first = list_start(top.selected, usize::from(list.height));
        let index = first + usize::from(at.y - list.y);
        if rows.get(index).is_some_and(SidebarRow::selectable) {
            top.selected = index;
            self.submit_layer();
        }
    }

    /// Follows `draw_jump`: a group's header takes a row of its own.
    fn click_jump(&mut self, rect: Rect, at: Position) {
        let list = list_area(LayerKind::Jump, rect);
        if !list.contains(at) {
            return;
        }
        let items = self.jump_items();
        let selected = self.layers.last().map_or(0, |top| top.selected);
        let (mut rows, mut selected_row) = (Vec::new(), 0);
        for (index, item) in items.iter().enumerate() {
            if index == 0 || items[index - 1].group != item.group {
                rows.push(None);
            }
            if index == selected {
                selected_row = rows.len();
            }
            rows.push(Some(index));
        }
        let first = list_start(selected_row, usize::from(list.height));
        if let Some(Some(index)) = rows.get(first + usize::from(at.y - list.y)) {
            if let Some(top) = self.layers.last_mut() {
                top.selected = *index;
            }
            self.submit_layer();
        }
    }

    fn click_sessions(&mut self, rect: Rect, at: Position) {
        let rows = rect.inner(Margin::new(1, 1));
        let row = usize::from(at.y.saturating_sub(rows.y));
        let selectable = self
            .session_rows()
            .get(row)
            .is_some_and(SidebarRow::selectable);
        if rows.contains(at) && selectable {
            self.sessions_sel = row;
            self.activate_session_row();
        }
    }

    fn click_sidebar(&mut self, rect: Rect, at: Position) {
        if self.view == View::Diff && at.y == rect.y + 1 {
            if let Some(scope) = scope_at(rect, at.x) {
                self.set_scope(scope);
            }
            return;
        }
        let rows = sidebar_rows_area(rect, self.view);
        if !rows.contains(at) {
            return;
        }
        let row = usize::from(at.y - rows.y);
        if let Some(file) = self.diff {
            let height = self.visible_rows(Pane::Sidebar, rows).height;
            let total = self.diff_rows(file).len();
            if first_diff_row(self.diff_scroll, total, height) + row == total - 1 {
                self.diff = None;
            }
        } else if self
            .view_rows(self.view)
            .get(row)
            .is_some_and(SidebarRow::selectable)
        {
            self.view_sel[self.view as usize] = row;
            self.activate_view_row();
        }
    }

    fn click_agent(&mut self, pane: Rect, at: Position) {
        if at.y == pane.y + 1 {
            self.click_chip(pane, at);
            return;
        }
        let message_rows = self.message_rows(pane);
        let slash = slash_area(pane, message_rows);
        if self.slash.is_some() && slash.contains(at) {
            self.click_slash(slash_rows(slash), at);
            return;
        }
        let rows = self.tabs[self.active].running.len() as u16;
        let running = running_area(self.above_message(pane), rows);
        if running.contains(at) {
            self.tabs[self.active].running_sel = Some(usize::from(at.y - running.y));
            self.activate_running();
            return;
        }
        self.tabs[self.active].running_sel = None;
        if let Some(file) = self.file_at(pane, at) {
            self.show_file(file);
            return;
        }
        self.focus_pane(Pane::Agent);
        if let Some(item) = self.opener_at(pane, at) {
            self.tabs[self.active].toggle(item);
            return;
        }
        if message_box_area(pane, message_rows).contains(at) {
            self.act(Action::EnterInput);
        } else if self.conversation_area(pane).contains(at) {
            let point = self.conversation_point(pane, at);
            self.selection = Some(Selection {
                anchor: point,
                end: point,
            });
        }
    }

    fn drag(&mut self, at: Position) {
        let (Some(panes), Some(selection)) = (self.panes(), self.selection) else {
            return;
        };
        let end = self.conversation_point(panes.agent, at);
        self.selection = Some(Selection { end, ..selection });
    }

    fn release(&mut self) {
        let (Some(panes), Some(selection)) = (self.panes(), self.selection) else {
            return;
        };
        if selection.anchor == selection.end {
            self.selection = None;
            return;
        }
        let width = self.conversation_area(panes.agent).width;
        let mut rows = self.conversation_rows(width);
        for at in self.tabs[self.active].mark_rows(width) {
            let row = &mut rows[at].0;
            row.truncate(row.trim_end_matches(['▸', '▾']).trim_end().len());
        }
        let text = selection.text(&rows, &self.tabs[self.active].row_joins(width));
        if text.trim().is_empty() {
            self.selection = None;
            return;
        }
        self.clipboard = Some(text);
        self.notice = Some("copied the selection".to_string());
    }

    fn conversation_area(&self, pane: Rect) -> Rect {
        self.visible_rows(Pane::Agent, self.conversation_rect(pane))
    }

    /// `at` as a (row index, column) of the conversation's rows, held inside its area.
    fn conversation_point(&self, pane: Rect, at: Position) -> (usize, u16) {
        let area = self.conversation_area(pane);
        let total = self.conversation_rows(area.width).len();
        let first = first_line(total, area.height, self.tabs[self.active].scroll);
        let y = at.y.min(area.bottom().saturating_sub(1)).max(area.top());
        let x = at.x.min(area.right().saturating_sub(1)).max(area.left());
        (first + usize::from(y - area.y), x - area.x)
    }

    fn click_slash(&mut self, list: Rect, at: Position) {
        let Some(selected) = self.slash.filter(|_| list.contains(at)) else {
            return;
        };
        let first = list_start(selected, usize::from(list.height));
        let index = first + usize::from(at.y - list.y);
        if index < self.slash_matches().len() {
            self.slash = Some(index);
            self.run_slash();
        }
    }

    fn click_chip(&mut self, pane: Rect, at: Position) {
        match chip_at(self, pane, at.x) {
            Some(ChipHit::Tab(index)) if index != self.active => self.activate_tab(index),
            Some(ChipHit::Tab(_)) => self.focus_pane(Pane::Agent),
            Some(ChipHit::New) => self.push_layer(LayerKind::Picker),
            None => {}
        }
    }

    fn file_at(&self, pane: Rect, at: Position) -> Option<usize> {
        let area = self.visible_rows(Pane::Agent, self.conversation_rect(pane));
        if !area.contains(at) {
            return None;
        }
        let rows = self.conversation_rows(area.width);
        let first = first_line(rows.len(), area.height, self.tabs[self.active].scroll);
        match rows.get(first + usize::from(at.y - area.y))?.1 {
            Tone::File(file) => Some(file),
            _ => None,
        }
    }

    /// The item a click at `at` opens or closes: a line that others open under.
    fn opener_at(&self, pane: Rect, at: Position) -> Option<String> {
        let area = self.conversation_area(pane);
        if !area.contains(at) {
            return None;
        }
        let tab = &self.tabs[self.active];
        let total = self.conversation_rows(area.width).len();
        let row = first_line(total, area.height, tab.scroll) + usize::from(at.y - area.y);
        tab.opener_at(area.width, row)
    }

    fn wheel(&mut self, at: Position, down: bool) {
        let Some(panes) = self.panes() else {
            return;
        };
        if !self.layers.is_empty() {
            self.step_layer(down);
        } else if panes.agent.contains(at) {
            self.scroll_agent(!down, super::By::Row);
        } else if panes.sessions.is_some_and(|rect| rect.contains(at)) {
            self.step_session(down);
        } else if matches!(panes.right, Right::Sidebar(rect) if rect.contains(at)) {
            self.step_sidebar(down);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    use crate::app::layers::LayerKind;
    use crate::app::{App, Mode, Pane};
    use crate::keys::KeyCtx;
    use crate::keys::KeySet::{Emacs, Vim};
    use crate::sample::{MenuId, Scope, View};
    use crate::testing::{
        click_text, find, numbered, press, screen, session_row, with_right_pane,
        with_sample_session,
    };

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        app.handle(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }

    fn click(app: &mut App, column: u16, row: u16) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
    }

    fn session_app(width: u16, height: u16) -> App {
        let mut app = App::new(Vim, width, height);
        app.open_session(session_row("pack registry"));
        app
    }

    fn top(app: &App) -> Option<LayerKind> {
        app.layers.last().map(|layer| layer.kind)
    }

    #[test]
    fn a_click_on_a_target_does_what_it_shows() {
        let fresh = || App::new(Vim, 132, 36);
        let session = || session_app(132, 36);
        let files = || with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        type Target = (fn() -> App, &'static str, &'static str, fn(&App) -> bool);
        let targets: [Target; 10] = [
            (fresh, "", "pack registry", |app| {
                (app.tabs[0].name.as_str(), app.focus) == ("packs", Pane::Agent)
            }),
            (fresh, "", "+ New session", |app| {
                top(app) == Some(LayerKind::Picker)
            }),
            (fresh, "ctrl-p", "App settings", |app| {
                top(app) == Some(LayerKind::Menu(MenuId::AppSettings))
            }),
            (session, "", "Triage · inbox", |app| {
                let shown = app.subagent.as_ref().map(|shown| shown.agent.as_str());
                (shown, app.tabs[0].agent.as_str()) == (Some("Triage"), "Engineer")
                    && (app.view, app.focus, app.ctx())
                        == (View::Subagent, Pane::Sidebar, KeyCtx::Normal)
            }),
            (session, "", "change unit", |app| {
                let tab = &app.tabs[0];
                app.view == View::Workflows && tab.sidebar && tab.running_sel.is_none()
            }),
            (session, "", "M cli/mod.rs", |app| {
                app.diff == Some(1) && app.tabs[0].sidebar && app.focus == Pane::Agent
            }),
            (files, "", "M cli/mod.rs", |app| app.diff == Some(1)),
            (session, "", "Message Engineer", |app| {
                (app.focus, app.mode) == (Pane::Agent, Mode::Input)
            }),
            (session, "h", "Moving them", |app| {
                app.focus == Pane::Agent && app.diff.is_none()
            }),
            (session, "j", "Moving them", |app| {
                app.tabs[0].running_sel.is_none()
            }),
        ];
        for (app, keys, target, done) in targets {
            let mut app = app();
            press(&mut app, keys);
            assert!(!done(&app), "{target} before the click");
            click_text(&mut app, target);
            assert!(done(&app), "{target}");
        }
    }

    #[test]
    fn a_click_on_a_tab_chip_switches_to_it() {
        let mut app = session_app(132, 36);
        click_text(&mut app, "flaky recovery");
        assert_eq!((app.tabs.len(), app.active), (2, 1));
        click_text(&mut app, "1 packs");
        assert_eq!(app.active, 0);
    }

    #[test]
    fn a_click_on_plus_opens_the_picker() {
        let mut app = App::new(Vim, 132, 36);
        let (row, column) = find(&screen(&app), " + ").expect("the new tab chip");
        click(&mut app, column + 1, row);
        assert_eq!(top(&app), Some(LayerKind::Picker));
    }

    #[test]
    fn a_click_on_the_active_chip_of_a_plain_tab_keeps_it() {
        let mut app = session_app(132, 36);
        click_text(&mut app, "1 packs");
        assert_eq!(app.tabs[0].name, "packs");
    }

    #[test]
    fn a_file_click_follows_a_scrolled_conversation() {
        let mut app = session_app(132, 36);
        let lines = app.tabs[0].lines.clone();
        app.tabs[0].lines = numbered(40);
        app.tabs[0].lines.extend(lines);
        app.tabs[0].scroll = 3;
        click_text(&mut app, "A shell/mod.rs");
        assert_eq!(app.diff, Some(2));
    }

    #[test]
    fn clicking_working_tree_switches_scope_and_closes_a_diff() {
        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        app.show_file(1);
        assert_eq!(app.diff, Some(1));
        click_text(&mut app, " working tree ");
        assert_eq!(app.scope, Scope::Tree);
        assert_eq!(app.diff, None);
        click(&mut app, 101, 1);
        assert_eq!(app.scope, Scope::Session);
        assert_eq!(app.view_sel[View::Diff as usize], 2, "the first file row");
    }

    #[test]
    fn rows_below_the_scope_line_take_their_clicks() {
        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        let s = screen(&app);
        let right = |y: usize| s[y].chars().skip(98).collect::<String>();
        assert!(right(5).starts_with("│ A shell/mod.rs  +3"), "{}", right(5));
        click(&mut app, 110, 5);
        assert_eq!(app.view_sel[View::Diff as usize], 2);
        assert_eq!(app.diff, Some(2));
        let s = screen(&app);
        let shown: String = s[3].chars().skip(98).collect();
        assert!(shown.starts_with("│ A shell/mod.rs  +3"), "{shown}");
    }

    #[test]
    fn a_click_on_the_back_row_closes_the_diff() {
        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        click_text(&mut app, "M cli/mod.rs");
        click_text(&mut app, "esc back to files");
        assert_eq!(app.diff, None);
    }

    #[test]
    fn a_click_on_the_rail_opens_the_floating_sidebar() {
        let mut app = with_right_pane(with_sample_session(App::new(Vim, 104, 30)));
        click(&mut app, 100, 5);
        assert_eq!(top(&app), Some(LayerKind::Sidebar));
        click_text(&mut app, "M cli/mod.rs");
        assert_eq!(app.diff, Some(1));
    }

    #[test]
    fn a_click_on_a_picker_row_opens_that_agent() {
        let mut app = App::new(Vim, 132, 36);
        let (row, column) = find(&screen(&app), " + ").expect("the new tab chip");
        click(&mut app, column + 1, row);
        click_text(&mut app, "Researcher ");
        assert_eq!(app.tabs[app.active].agent, "Researcher");
        assert!(app.layers.is_empty());
    }

    #[test]
    fn a_click_on_a_menu_row_acts_on_it() {
        let node = LayerKind::Menu(MenuId::NodeSettings);
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(node);
        click_text(&mut app, " Packs ");
        assert_eq!(top(&app), Some(LayerKind::Menu(MenuId::Packs)));
        press(&mut app, "esc");
        assert_eq!(top(&app), Some(node));
        click_text(&mut app, " Agents ");
        assert_eq!(top(&app), Some(LayerKind::Menu(MenuId::Agents)));
    }

    #[test]
    fn a_click_on_a_menu_header_does_nothing() {
        let node = LayerKind::Menu(MenuId::NodeSettings);
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(node);
        click_text(&mut app, "AUTOMATION");
        assert_eq!(app.notice, None);
        assert_eq!(top(&app), Some(node));
    }

    #[test]
    fn a_click_on_a_scrolled_menu_row_hits_the_row_drawn_there() {
        let mut app = App::new(Vim, 60, 16);
        app.push_layer(LayerKind::Menu(MenuId::NodeSettings));
        press(&mut app, "j j j j j j j j j j j j j");
        click_text(&mut app, " Tools ");
        assert_eq!(top(&app), Some(LayerKind::Menu(MenuId::Tools)));
    }

    #[test]
    fn a_click_on_a_jump_header_does_nothing() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "ctrl-p");
        click_text(&mut app, "SESSIONS");
        assert_eq!(top(&app), Some(LayerKind::Jump));
    }

    #[test]
    fn a_click_on_a_scrolled_jump_row_hits_the_row_drawn_there() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "ctrl-p");
        for _ in 0..40 {
            press(&mut app, "ctrl-n");
        }
        click_text(&mut app, "Sidebar: This session's workflows");
        assert_eq!(app.view, View::Workflows);
        assert!(app.layers.is_empty());
    }

    #[test]
    fn a_click_on_the_slash_list_runs_that_command_not_a_row_beneath() {
        let mut app = session_app(132, 36);
        press(&mut app, "i");
        crate::testing::type_text(&mut app, "/");
        click_text(&mut app, "tab complete · enter run");
        assert_eq!(app.tabs[0].agent, "Engineer", "the footer is not a row");
        assert_eq!(app.tabs[0].running_sel, None);
        assert!(app.slash.is_some());
        click_text(&mut app, "/clear");
        assert!(app.tabs[0].lines.is_empty());
        assert_eq!(app.slash, None);
        assert_eq!(app.tabs[0].agent, "Engineer");
    }

    #[test]
    fn a_click_outside_a_layer_closes_it() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "ctrl-p");
        click(&mut app, 0, 0);
        assert!(app.layers.is_empty());
        assert_eq!(app.tabs[0].name, "new session");
    }

    #[test]
    fn a_click_inside_an_empty_row_does_nothing() {
        let mut app = App::new(Vim, 132, 36);
        click(&mut app, 5, 25);
        assert_eq!(app.focus, Pane::Sessions);
        assert_eq!(app.tabs[0].name, "new session");

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        click(&mut app, 110, 30);
        assert_eq!(app.focus, Pane::Sidebar);
        assert_eq!(app.diff, None);

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc ?");
        click(&mut app, 66, 15);
        assert_eq!(top(&app), Some(LayerKind::Keys));
    }

    #[test]
    fn clicks_on_the_too_small_screen_do_nothing() {
        let mut app = App::new(Vim, 40, 10);
        let before = app.tabs.clone();
        click(&mut app, 5, 5);
        mouse(&mut app, MouseEventKind::ScrollDown, 5, 5);
        assert_eq!(app.tabs, before);
        assert_eq!(app.notice, None);
    }

    #[test]
    fn clicks_past_the_window_do_nothing() {
        let mut app = session_app(132, 36);
        let before = app.tabs.clone();
        click(&mut app, u16::MAX, u16::MAX);
        assert_eq!(app.tabs, before);
    }

    #[test]
    fn other_buttons_do_nothing() {
        let mut app = session_app(132, 36);
        let (row, column) = find(&screen(&app), "Triage · inbox").expect("a row");
        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Moved,
        ] {
            mouse(&mut app, kind, column, row);
            assert_eq!(app.subagent, None);
        }
    }

    /// Presses the left button at `from`, drags to `to` and releases there, as (row, column).
    fn drag(app: &mut App, from: (u16, u16), to: (u16, u16)) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), from.1, from.0);
        mouse(app, MouseEventKind::Drag(MouseButton::Left), to.1, to.0);
        mouse(app, MouseEventKind::Up(MouseButton::Left), to.1, to.0);
    }

    /// The sample reply's first character and its last.
    fn reply_ends(app: &App) -> ((u16, u16), (u16, u16)) {
        let shown = screen(app);
        let start = find(&shown, "Moving them under").expect("the reply");
        let (row, column) = find(&shown, "beside them.").expect("its second row");
        (start, (row, column + "beside them.".len() as u16 - 1))
    }

    const REPLY: &str = "Moving them under gents auth, one subcommand per provider,\n and folding the probes in beside them.";

    #[test]
    fn a_drag_across_rows_copies_their_text() {
        let mut app = session_app(132, 36);
        let (start, end) = reply_ends(&app);
        drag(&mut app, start, end);
        assert_eq!(app.take_clipboard().as_deref(), Some(REPLY));
        assert_eq!(app.notice.as_deref(), Some("copied the selection"));
    }

    #[test]
    fn a_drag_over_a_wrapped_reply_copies_its_lines_whole() {
        use crate::app::transcript::ChatLine;
        use crate::sample::Tone;
        let mut app = session_app(132, 36);
        let words: Vec<String> = (0..60).map(|at| format!("word{at}")).collect();
        let text = format!("{}\nnext line", words.join(" "));
        app.tabs[0].lines = vec![ChatLine {
            text: text.clone(),
            tone: Tone::Reply,
            item: None,
            change: None,
            under: None,
        }];
        let shown = screen(&app);
        let start = find(&shown, "word0").expect("the reply");
        let (row, column) = find(&shown, "next line").expect("its last line");
        assert!(row > start.0 + 1, "the first line wraps");
        drag(&mut app, start, (row, column + 8));
        assert_eq!(app.take_clipboard().as_deref(), Some(text.as_str()));
    }

    #[test]
    fn a_drag_upward_copies_the_same_text() {
        let mut app = session_app(132, 36);
        let (start, end) = reply_ends(&app);
        drag(&mut app, end, start);
        assert_eq!(app.take_clipboard().as_deref(), Some(REPLY));
    }

    #[test]
    fn a_drag_past_the_conversation_clamps() {
        let mut app = session_app(132, 36);
        app.tabs[0].lines = numbered(100);
        app.tabs[0].scroll = 10;
        let shown = screen(&app);
        let shows = |n: &usize| find(&shown, &format!("message {n}")).is_some();
        let first = (10..100).find(shows).expect("the top shown row");
        let last = (10..99).rev().find(shows).expect("rows hidden below");
        let messages = |from: usize, to: usize| {
            let rows = (from..=to).map(|n| format!("message {n}"));
            rows.collect::<Vec<_>>().join("\n")
        };
        let (row, column) = find(&shown, "message 80").expect("a shown row");
        drag(&mut app, (row, column), (35, 200));
        assert_eq!(app.take_clipboard(), Some(messages(80, last)));
        drag(&mut app, (row, column + 9), (0, 0));
        assert_eq!(app.take_clipboard(), Some(messages(first, 80)));
    }

    #[test]
    fn copying_a_command_leaves_out_its_open_mark() {
        let mut app = session_app(132, 36);
        command_tab(&mut app);
        let shown = screen(&app);
        let command = find(&shown, "✓ run").expect("the command");
        let (row, column) = find(&shown, "All good.").expect("the reply");
        drag(&mut app, (row, column + 8), command);
        assert_eq!(
            app.take_clipboard().as_deref(),
            Some("✓ run   cargo check\n\nAll good.")
        );
    }

    #[test]
    fn a_click_copies_nothing_and_a_key_ends_the_selection() {
        let mut app = session_app(132, 36);
        let (start, end) = reply_ends(&app);
        drag(&mut app, start, start);
        assert_eq!((app.take_clipboard(), app.selection), (None, None));
        drag(&mut app, start, end);
        assert!(app.selection.is_some());
        press(&mut app, "j");
        assert_eq!(app.selection, None);
    }

    #[test]
    fn a_drag_over_blank_space_copies_nothing() {
        let mut app = session_app(132, 36);
        let (start, _) = reply_ends(&app);
        let blank = (start.0 + 1, start.1 + 80);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            blank.1,
            blank.0,
        );
        let further = (blank.1 + 4, blank.0);
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            further.0,
            further.1,
        );
        assert!(app.selection.is_some(), "the press started a selection");
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            further.0,
            further.1,
        );
        let notice = app.notice.clone();
        assert_eq!(
            (app.take_clipboard(), notice, app.selection),
            (None, None, None)
        );
    }

    #[test]
    fn a_drag_from_a_file_row_opens_the_file() {
        let mut app = session_app(132, 36);
        let start = find(&screen(&app), "M cli/mod.rs").expect("a file row");
        drag(&mut app, start, (start.0 + 1, start.1 + 4));
        assert_eq!((app.diff, app.selection), (Some(1), None));
    }

    /// A command's line `c1`, a reply, and the command's output under it arriving after the
    /// reply, each followed by a blank.
    fn command_tab(app: &mut App) {
        use crate::app::transcript::ChatLine;
        use crate::sample::Tone;
        let line = |text: &str, item: &str, under: Option<&str>| ChatLine {
            text: text.to_string(),
            tone: Tone::Label,
            item: Some(item.to_string()),
            change: None,
            under: under.map(str::to_string),
        };
        let tab = &mut app.tabs[0];
        tab.lines.clear();
        tab.put(line("  ✓ run   cargo check", "c1", None));
        tab.put(line("All good.", "m2", None));
        tab.put(line("Finished in 41s", "c1/output", Some("c1")));
    }

    #[test]
    fn a_command_opens_its_output_on_click() {
        let mut app = session_app(132, 36);
        command_tab(&mut app);
        let rows = |app: &App| {
            let shown = app.tabs[0].wrapped_rows(80);
            shown.into_iter().map(|(text, _)| text).collect::<Vec<_>>()
        };
        assert_eq!(
            rows(&app),
            ["  ✓ run   cargo check  ▸", "", "All good.", ""]
        );
        click_text(&mut app, "cargo check");
        let open = [
            "  ✓ run   cargo check  ▾",
            "",
            "Finished in 41s",
            "",
            "All good.",
            "",
        ];
        assert_eq!(rows(&app), open);
        assert_eq!(
            app.selection, None,
            "the click opened the line, no selection"
        );
        click_text(&mut app, "cargo check");
        assert_eq!(
            rows(&app),
            ["  ✓ run   cargo check  ▸", "", "All good.", ""]
        );
    }

    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer() {
        let mut app = with_right_pane(session_app(132, 36));
        let lines = app.tabs[0].lines.clone();
        app.tabs[0].lines = numbered(100);
        app.tabs[0].lines.extend(lines);
        mouse(&mut app, MouseEventKind::ScrollUp, 60, 10);
        assert_eq!(app.tabs[0].scroll, 1);
        mouse(&mut app, MouseEventKind::ScrollDown, 60, 10);
        assert_eq!(app.tabs[0].scroll, 0);
        assert_eq!(
            app.tabs[0].running_sel, None,
            "the wheel never selects a row"
        );

        let before = app.sessions_sel;
        mouse(&mut app, MouseEventKind::ScrollDown, 5, 10);
        assert_ne!(app.sessions_sel, before);
        assert_eq!(app.tabs[0].scroll, 0);

        let before = app.view_sel[View::Diff as usize];
        mouse(&mut app, MouseEventKind::ScrollDown, 110, 10);
        assert_ne!(app.view_sel[View::Diff as usize], before);
    }

    #[test]
    fn the_wheel_moves_within_the_top_layer() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "ctrl-p");
        mouse(&mut app, MouseEventKind::ScrollDown, 60, 10);
        assert_eq!(app.layers[0].selected, 1);
        mouse(&mut app, MouseEventKind::ScrollUp, 60, 10);
        assert_eq!(app.layers[0].selected, 0);
    }

    #[test]
    fn emacs_takes_the_same_clicks() {
        let mut app = App::new(Emacs, 132, 36);
        click_text(&mut app, "pack registry");
        click_text(&mut app, "Triage · inbox");
        assert!(find(&screen(&app), "Subagent: Triage").is_some());
        assert_eq!(app.tabs[0].agent, "Engineer");
    }
}
