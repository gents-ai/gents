use ratatui::layout::Rect;

use super::input::LineInput;
use super::work::{in_range, item};
use super::{first_selectable, step_row, stepped, wrap, App};
use crate::commands::COMMANDS;
use crate::filter::subsequence;
use crate::jump::{self, Target};
use crate::keys::{Action, Dir, KeyCtx, KeySet};
pub use crate::layout::LayerKind;
use crate::layout::{keys_rows_area, layer_rect, sessions_can_dock, sidebar_can_dock};
use crate::sample::{empty, key_rows, menu, note, MenuId, RowAction, SidebarRow, Tone};
use crate::source::{Agent, Connection, FormRef, Intent, MenuAct, MenuRow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub kind: LayerKind,
    pub filter: LineInput,
    pub selected: usize,
}

const NOT_LISTED: &str = "that session is not in the list";

impl App {
    /// The settings menus are the shell's own; every other menu shows its rows as last read.
    /// A lost source drops the reads still out, so a menu not read by then says why.
    pub fn menu_rows(&self, id: MenuId) -> (Vec<SidebarRow>, &'static str) {
        let (rows, footer) = menu(id);
        let notes = || {
            empty(id)
                .iter()
                .map(|text| note(text, Tone::Label))
                .collect()
        };
        let rows = match (id, self.menus.get(&id), &self.connection) {
            (MenuId::NodeSettings | MenuId::AppSettings, ..) => rows.to_vec(),
            (MenuId::General, ..) => {
                let keys = format!(" Keys      {}", self.keymap.set().name());
                vec![item(
                    keys,
                    Tone::Plain,
                    RowAction::OpenMenu(MenuId::KeySets),
                )]
            }
            (MenuId::KeySets, ..) => [KeySet::Regular, KeySet::Vim, KeySet::Emacs]
                .into_iter()
                .map(|set| {
                    let (text, tone) = match set == self.keymap.set() {
                        true => (format!(" ● {}", set.name()), Tone::Accent),
                        false => (format!("   {}", set.name()), Tone::Plain),
                    };
                    item(text, tone, RowAction::UseKeys(set))
                })
                .collect(),
            (MenuId::Developer | MenuId::Experimental, ..) => notes(),
            (_, Some(Err(why)), _) | (_, None, Connection::Lost(why)) => {
                vec![item(
                    format!(" could not read: {why}"),
                    Tone::Warn,
                    RowAction::None,
                )]
            }
            (_, None, _) => vec![note(" reading…", Tone::Label)],
            (_, Some(Ok(read)), _) if read.is_empty() => notes(),
            (_, Some(Ok(read)), _) => read
                .iter()
                .enumerate()
                .map(|(index, row)| item(row.text.clone(), row.tone, RowAction::MenuRow(index)))
                .collect(),
        };
        (rows, footer)
    }

    /// The layer showing the menu, on top or beneath, keeps its selection in range; rows read
    /// for another menu move nothing. A menu is in the stack at most once.
    pub(super) fn set_menu(&mut self, id: MenuId, rows: Result<Vec<MenuRow>, String>) {
        if self.for_another_agent(id, &rows) {
            return;
        }
        self.menus.insert(id, rows);
        let shown = self.menu_rows(id).0;
        let showing = self
            .layers
            .iter_mut()
            .find(|layer| layer.kind == LayerKind::Menu(id));
        if let Some(layer) = showing {
            layer.selected = in_range(&shown, layer.selected);
        }
    }

    /// A Profiles answer names its agent only in its rows' acts, so one for an agent chosen
    /// before this one is late.
    fn for_another_agent(&self, id: MenuId, rows: &Result<Vec<MenuRow>, String>) -> bool {
        let (MenuId::Profiles, Ok(rows)) = (id, rows) else {
            return false;
        };
        rows.iter().any(|row| match &row.enter {
            Some(MenuAct::SetAgentProfile { agent, .. }) => {
                self.profiles_of.as_ref() != Some(agent)
            }
            _ => false,
        })
    }

    /// A lost source reads no more intents, so the menu says why at once.
    pub(super) fn read_menu(&mut self, id: MenuId) {
        let intent = match (id, &self.profiles_of) {
            (MenuId::Profiles, Some(agent)) => Intent::ReadProfiles {
                agent: agent.clone(),
            },
            _ => Intent::ReadMenu(id),
        };
        match &self.connection {
            Connection::Lost(why) => self.set_menu(id, Err(why.clone())),
            _ => self.emit(intent),
        }
    }

    /// Enter, or with `d` the `d` key, on the menu's `row`th row as read. A row without that act
    /// does nothing. More, Profiles and Ask close as they choose, and More's choice acts on the
    /// menu it was opened over.
    pub(super) fn run_row(&mut self, menu: MenuId, row: usize, d: bool) {
        let Some(Ok(rows)) = self.menus.get(&menu) else {
            return;
        };
        let Some(act) = rows
            .get(row)
            .and_then(|row| if d { &row.d } else { &row.enter }.clone())
        else {
            return;
        };
        if matches!(menu, MenuId::More | MenuId::Profiles | MenuId::Ask) {
            self.layers
                .retain(|layer| layer.kind != LayerKind::Menu(menu));
        }
        let menu = match (menu, &self.more) {
            (MenuId::More, Some((over, _))) => *over,
            _ => menu,
        };
        match act {
            MenuAct::OpenSession { session } => {
                let rows = self.session_rows();
                let listed = rows.iter().position(|row| match row.action {
                    RowAction::OpenSession(index) => self.sessions[index].id == session,
                    _ => false,
                });
                match listed {
                    Some(row) => self.open_session(row),
                    None => self.notice = Some(NOT_LISTED.to_string()),
                }
            }
            MenuAct::OpenSetup => self.push_layer(LayerKind::Setup),
            MenuAct::Insert { text } => {
                self.layers.clear();
                self.act(Action::EnterInput);
                self.slash = None;
                let input = &mut self.tabs[self.active].input;
                input.home();
                input.paste_inline(&text);
                input.end();
            }
            MenuAct::Open(form) => self.open_form(form),
            MenuAct::ConfirmDelete {
                doc,
                id,
                label,
                name,
            } => self.confirm_delete(doc, id, label, name),
            MenuAct::ChooseProfile { agent, .. } => {
                self.profiles_of = Some(agent);
                self.menus.remove(&MenuId::Profiles);
                self.push_layer(LayerKind::Menu(MenuId::Profiles));
            }
            act if !self.says_lost() => self.emit(Intent::Act { menu, act }),
            _ => {}
        }
    }

    /// `.` on the menu's `row`th row as read: its further acts, titled with the row.
    fn open_more(&mut self, menu: MenuId, row: usize) {
        let Some(Ok(rows)) = self.menus.get(&menu) else {
            return;
        };
        let Some(row) = rows.get(row).filter(|row| !row.more.is_empty()) else {
            return;
        };
        let title = row.text.trim().to_string();
        let choices = row
            .more
            .iter()
            .map(|(label, act)| MenuRow {
                text: format!(" {label}"),
                tone: Tone::Plain,
                enter: Some(act.clone()),
                d: None,
                more: Vec::new(),
            })
            .collect();
        self.more = Some((menu, title));
        self.set_menu(MenuId::More, Ok(choices));
        self.push_layer(LayerKind::Menu(MenuId::More));
    }

    /// The source's question, on top. Each opens fresh, so no cursor left on an earlier question
    /// answers this one. Each row wraps to the layer, keeping its indent.
    pub(super) fn ask(&mut self, title: String, rows: Vec<MenuRow>) {
        self.asked = title;
        let ask = LayerKind::Menu(MenuId::Ask);
        self.layers.retain(|layer| layer.kind != ask);
        self.push_layer(ask);
        let inner = self.top_layer_rect();
        let inner = inner.map_or(usize::MAX, |rect| usize::from(rect.width.saturating_sub(2)));
        let rows = rows.into_iter().flat_map(|row| {
            let indent = row.text.len() - row.text.trim_start().len();
            let lines = wrap(&row.text[indent..], inner.saturating_sub(indent));
            lines.into_iter().map(move |line| MenuRow {
                text: format!("{}{line}", &row.text[..indent]),
                ..row.clone()
            })
        });
        self.set_menu(MenuId::Ask, Ok(rows.collect()));
    }

    /// More is titled with the row it was opened for, and Ask with its question.
    pub fn menu_title(&self, id: MenuId) -> &str {
        match (id, &self.more) {
            (MenuId::More, Some((_, title))) => title,
            (MenuId::Ask, _) => &self.asked,
            _ => id.name(),
        }
    }

    pub fn picker_matches(&self, query: &str) -> Vec<&Agent> {
        self.agents
            .iter()
            .filter(|agent| subsequence(query, &agent.name))
            .collect()
    }

    /// A kind already in the stack comes to the top. The bar looks only at the top, because
    /// opening it again over a menu is how depth grows. A menu is read again as it comes up,
    /// and the bar reads the installed workflows.
    pub fn push_layer(&mut self, kind: LayerKind) {
        let from = match kind {
            LayerKind::Jump => self.layers.len().saturating_sub(1),
            _ => 0,
        };
        let found = self.layers[from..].iter().position(|l| l.kind == kind);
        if let Some(offset) = found {
            let layer = self.layers.remove(from + offset);
            self.layers.push(layer);
        } else {
            let selected = match kind {
                LayerKind::Menu(id) => first_selectable(&self.menu_rows(id).0),
                _ => 0,
            };
            self.layers.push(Layer {
                kind,
                filter: LineInput::default(),
                selected,
            });
        }
        match kind {
            LayerKind::Menu(
                MenuId::NodeSettings
                | MenuId::AppSettings
                | MenuId::More
                | MenuId::Ask
                | MenuId::General
                | MenuId::KeySets
                | MenuId::Developer
                | MenuId::Experimental,
            ) => {}
            LayerKind::Menu(id) => self.read_menu(id),
            LayerKind::Jump => self.read_menu(MenuId::Workflows),
            LayerKind::Setup if found.is_none() => self.open_setup(),
            _ => {}
        }
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
        let took = match self.layers.last().map(|top| top.kind) {
            Some(LayerKind::Setup) => self.act_on_setup(action),
            Some(LayerKind::Form) => self.act_on_form(action),
            _ => false,
        };
        if took {
            return;
        }
        let width = self.size.0;
        let Some(top) = self.layers.last_mut() else {
            return;
        };
        match action {
            _ if action.edits_line() => {
                top.filter.edit(action);
                if action.changes_line() {
                    top.selected = 0;
                }
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
            Action::SecondAct | Action::More => {
                if let LayerKind::Menu(id) = top.kind {
                    let selected = top.selected;
                    let on = self.menu_rows(id).0.get(selected).map(|row| row.action);
                    match (on, action) {
                        (Some(RowAction::MenuRow(row)), Action::More) => self.open_more(id, row),
                        (Some(RowAction::MenuRow(row)), _) => self.run_row(id, row, true),
                        _ => {}
                    }
                }
            }
            Action::ClosePane => {
                self.layers.pop();
            }
            Action::ToggleSessions if !sessions_can_dock(width) => {
                self.toggle_layer(LayerKind::Sessions);
            }
            Action::ToggleSidebar if !sidebar_can_dock(width) => self.toggle_sidebar(),
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
            LayerKind::Menu(id) => {
                match self.menu_rows(id).0.get(top.selected).map(|row| row.action) {
                    Some(RowAction::OpenMenu(next)) => self.push_layer(LayerKind::Menu(next)),
                    Some(RowAction::MenuRow(row)) => self.run_row(id, row, false),
                    Some(RowAction::NodeForm) => self.open_form(FormRef::Node),
                    Some(RowAction::UseKeys(set)) => {
                        self.layers.pop();
                        self.use_keys(set);
                    }
                    Some(RowAction::NotWired) => self.not_wired(),
                    _ => {}
                }
            }
            LayerKind::Sessions => self.activate_session_row(),
            LayerKind::Sidebar => self.activate_view_row(),
            LayerKind::Jump => self.submit_jump(top.selected),
            LayerKind::Setup => self.submit_setup(),
            LayerKind::Keys | LayerKind::Form => {}
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
            Target::Workflow(row) => self.run_row(MenuId::Workflows, row, false),
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
            LayerKind::Menu(id) => {
                let rows = self.menu_rows(id).0;
                if let Some(top) = self.layers.last_mut() {
                    top.selected = step_row(&rows, top.selected, forward);
                }
            }
            LayerKind::Keys => {
                if let Some(rect) = rect {
                    let height = usize::from(keys_rows_area(rect).height);
                    let max = key_rows(self.keymap.set()).len().saturating_sub(height);
                    top.selected = stepped(top.selected, forward, max);
                }
            }
            LayerKind::Sessions => self.step_session(forward),
            LayerKind::Sidebar => self.step_sidebar(forward),
            LayerKind::Setup => {
                if let Some(setup) = &mut self.setup {
                    setup.step_row(forward);
                }
            }
            LayerKind::Jump => {
                let max = self.jump_items().len().saturating_sub(1);
                if let Some(top) = self.layers.last_mut() {
                    top.selected = stepped(top.selected, forward, max);
                }
            }
            LayerKind::Form => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use super::*;
    use crate::app::{Mode, Pane};
    use crate::draw::theme;
    use crate::keys::KeySet::{Emacs, Regular, Vim};
    use crate::sample::{self, MENUS};
    use crate::source::{Doc, EditedChoice, FormRef, PackAct, Update};
    use crate::testing::{
        buffer, connected, drain, find, kinds, press, screen, type_text, with_right_pane,
        with_sample_session,
    };
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::Event;
    use ratatui::Terminal;

    const ACCOUNTS: LayerKind = LayerKind::Menu(MenuId::Accounts);
    const TOOLS: LayerKind = LayerKind::Menu(MenuId::Tools);
    const TASKS: LayerKind = LayerKind::Menu(MenuId::Tasks);
    const SKILLS: LayerKind = LayerKind::Menu(MenuId::Skills);
    const MORE: LayerKind = LayerKind::Menu(MenuId::More);
    const APP: LayerKind = LayerKind::Menu(MenuId::AppSettings);

    fn menu_row(text: &str, enter: Option<MenuAct>, d: Option<MenuAct>) -> MenuRow {
        MenuRow {
            text: text.to_string(),
            tone: Tone::Plain,
            enter,
            d,
            more: Vec::new(),
        }
    }

    fn with_more(text: &str, more: &[(&str, MenuAct)]) -> MenuRow {
        let more = more
            .iter()
            .map(|(label, act)| (label.to_string(), act.clone()))
            .collect();
        MenuRow {
            more,
            ..menu_row(text, None, None)
        }
    }

    fn plain(texts: &[&str]) -> Vec<MenuRow> {
        texts
            .iter()
            .map(|text| menu_row(text, None, None))
            .collect()
    }

    fn read(app: &mut App, menu: MenuId, rows: Vec<MenuRow>) {
        app.apply(Update::Menu {
            menu,
            rows: Ok(rows),
        });
    }

    fn shown(app: &App, id: MenuId) -> Vec<(String, Tone)> {
        let rows = app.menu_rows(id).0;
        rows.iter()
            .map(|row| (row.text.to_string(), row.tone))
            .collect()
    }

    fn selected_text(app: &App, id: MenuId) -> String {
        let top = app.layers.last().expect("a layer");
        app.menu_rows(id).0[top.selected].text.to_string()
    }

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
    fn node_settings_rows_open_their_sections() {
        for (name, id) in [
            ("Skills", MenuId::Skills),
            ("Triggers", MenuId::Triggers),
            ("Schedules", MenuId::Schedules),
            ("Event sources", MenuId::EventSources),
            ("Remote Tools", MenuId::RemoteTools),
            ("Allowed folders", MenuId::AllowedFolders),
            ("Packs", MenuId::Packs),
            ("UX Plugins", MenuId::UxPlugins),
        ] {
            let (mut app, intended) = connected(Vim, 132, 36);
            app.push_layer(NODE);
            app.layers[0].selected = row_named(MenuId::NodeSettings, name);
            press(&mut app, "enter");
            assert_eq!(kinds(&app), [NODE, LayerKind::Menu(id)], "{name}");
            assert_eq!(drain(&intended), [Intent::ReadMenu(id)], "{name}");
            assert!(find(&screen(&app), &format!(" {} ", id.name())).is_some());
        }

        let (mut app, intended) = connected(Vim, 132, 36);
        app.push_layer(NODE);
        app.layers[0].selected = row_named(MenuId::NodeSettings, "Node");
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [NODE, LayerKind::Form]);
        let node = Intent::OpenForm {
            form: FormRef::Node,
            opening: 1,
        };
        assert_eq!(drain(&intended), [node]);
    }

    #[test]
    fn an_empty_section_says_what_the_desktop_says() {
        let (mut app, _intended) = connected(Vim, 132, 36);
        app.push_layer(LayerKind::Menu(MenuId::AllowedFolders));
        read(&mut app, MenuId::AllowedFolders, Vec::new());
        assert_eq!(
            shown(&app, MenuId::AllowedFolders),
            [
                (" None added.".to_string(), Tone::Label),
                (
                    " The working folder is always readable. Other paths are asked about."
                        .to_string(),
                    Tone::Label
                ),
            ]
        );
        let s = screen(&app);
        assert!(find(&s, "None added.").is_some());
        assert!(find(&s, "The working folder is always readable.").is_some());
        assert_eq!(find(&s, " none"), None);
    }

    #[test]
    fn a_section_menu_shows_the_desktops_copy_uncut() {
        for (set, back) in [(Vim, "esc back"), (Emacs, "ctrl-g back")] {
            let (mut app, _intended) = connected(set, 132, 36);
            app.push_layer(LayerKind::Menu(MenuId::AllowedFolders));
            read(&mut app, MenuId::AllowedFolders, Vec::new());
            let s = screen(&app);
            let note = " The working folder is always readable. Other paths are asked about.";
            let footer = format!(" d read only or read and write · . more · {back}");
            assert!(find(&s, note).is_some(), "{set:?}");
            assert!(find(&s, &footer).is_some(), "{set:?}");
        }
    }

    #[test]
    fn a_profiles_answer_for_another_agent_is_dropped() {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.push_layer(LayerKind::Menu(MenuId::Agents));
        let choose = MenuAct::ChooseProfile {
            agent: "a1".to_string(),
            name: "Engineer".to_string(),
        };
        let rows = vec![with_more(" Engineer", &[("Inference profile…", choose)])];
        read(&mut app, MenuId::Agents, rows);
        press(&mut app, ". enter");
        intended.try_iter().for_each(drop);
        let profile = |agent: &str| {
            let set = MenuAct::SetAgentProfile {
                agent: agent.to_string(),
                name: agent.to_string(),
                profile: "opus".to_string(),
                label: "Claude Opus".to_string(),
            };
            vec![menu_row(&format!("   Opus for {agent}"), Some(set), None)]
        };
        let reading = [(" reading…".to_string(), Tone::Label)];

        read(&mut app, MenuId::Profiles, profile("a2"));
        assert_eq!(shown(&app, MenuId::Profiles), reading);

        app.apply(Update::Menu {
            menu: MenuId::Profiles,
            rows: Err("gone".to_string()),
        });
        let why = (" could not read: gone".to_string(), Tone::Warn);
        assert_eq!(shown(&app, MenuId::Profiles), [why]);

        read(&mut app, MenuId::Profiles, profile("a1"));
        let rows = [("   Opus for a1".to_string(), Tone::Plain)];
        assert_eq!(shown(&app, MenuId::Profiles), rows);
    }

    #[test]
    fn dot_opens_the_rows_more_menu_and_runs_its_choice() {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.push_layer(SKILLS);
        let duplicate = MenuAct::Duplicate(Doc::Skill, "s1".to_string());
        let rows = vec![
            with_more(" Code review", &[("Duplicate", duplicate.clone())]),
            menu_row(" bare", None, None),
        ];
        read(&mut app, MenuId::Skills, rows);
        intended.try_iter().for_each(drop);

        press(&mut app, ".");
        assert_eq!(kinds(&app), [SKILLS, MORE]);
        assert_eq!(drain(&intended), [], "More reads nothing");
        let rect = app.top_layer_rect().expect("More");
        let s = screen(&app);
        assert!(s[usize::from(rect.y)].contains(" Code review "));
        assert!(find(&s, " Duplicate").is_some());
        assert!(find(&s, " enter choose · esc back").is_some());

        press(&mut app, "enter");
        let act = Intent::Act {
            menu: MenuId::Skills,
            act: duplicate,
        };
        assert_eq!(drain(&intended), [act]);
        assert_eq!(kinds(&app), [SKILLS]);

        press(&mut app, "j .");
        assert_eq!(kinds(&app), [SKILLS], "a row without more ignores it");
        assert_eq!(drain(&intended), []);
    }

    #[test]
    fn delete_from_more_asks_for_the_name() {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.push_layer(SKILLS);
        let delete = MenuAct::ConfirmDelete {
            doc: Doc::Skill,
            id: "s1".to_string(),
            label: "Review".to_string(),
            name: "review".to_string(),
        };
        let rows = vec![with_more(" Review", &[("Delete skill…", delete)])];
        read(&mut app, MenuId::Skills, rows);
        intended.try_iter().for_each(drop);

        press(&mut app, ". enter");
        assert_eq!(kinds(&app), [SKILLS, LayerKind::Form]);
        assert!(find(&screen(&app), " Delete Review? ").is_some());
        assert_eq!(drain(&intended), []);
    }

    #[test]
    fn p_is_gone_and_profiles_come_from_more() {
        let (mut app, intended) = connected(Vim, 132, 36);
        let agents = LayerKind::Menu(MenuId::Agents);
        let profiles = LayerKind::Menu(MenuId::Profiles);
        app.push_layer(agents);
        let choose = |agent: &str, name: &str| {
            let (agent, name) = (agent.to_string(), name.to_string());
            [("Inference profile…", MenuAct::ChooseProfile { agent, name })]
        };
        let rows = vec![
            with_more(" Engineer", &choose("a1", "Engineer")),
            with_more(" Reviewer", &choose("a2", "Reviewer")),
        ];
        read(&mut app, MenuId::Agents, rows);
        intended.try_iter().for_each(drop);

        press(&mut app, "p");
        assert_eq!(kinds(&app), [agents]);
        assert_eq!(drain(&intended), []);

        press(&mut app, ". enter");
        assert_eq!(kinds(&app), [agents, profiles]);
        let agent = "a1".to_string();
        assert_eq!(drain(&intended), [Intent::ReadProfiles { agent }]);
        assert!(find(&screen(&app), " Inference profile ").is_some());

        let set = MenuAct::SetAgentProfile {
            agent: "a1".to_string(),
            name: "Engineer".to_string(),
            profile: "opus".to_string(),
            label: "Claude Opus".to_string(),
        };
        let rows = vec![
            menu_row(" ● Claude Sonnet", None, None),
            menu_row("   Claude Opus", Some(set.clone()), None),
        ];
        read(&mut app, MenuId::Profiles, rows);
        press(&mut app, "j enter");
        let act = Intent::Act {
            menu: MenuId::Profiles,
            act: set,
        };
        assert_eq!(drain(&intended), [act]);
        assert_eq!(kinds(&app), [agents]);

        press(&mut app, "j . enter");
        let agent = "a2".to_string();
        assert_eq!(drain(&intended), [Intent::ReadProfiles { agent }]);
        assert_eq!(
            shown(&app, MenuId::Profiles),
            [(" reading…".to_string(), Tone::Label)],
            "another agent's profiles are not shown for this one"
        );
    }

    #[test]
    fn app_settings_saves_the_key_set() {
        let (mut app, intended) = connected(Vim, 132, 36);
        let (general, sets) = (
            LayerKind::Menu(MenuId::General),
            LayerKind::Menu(MenuId::KeySets),
        );
        app.push_layer(APP);
        app.layers[0].selected = row_named(MenuId::AppSettings, "General");
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [APP, general]);
        assert_eq!(drain(&intended), [], "General reads nothing");
        let s = screen(&app);
        assert!(find(&s, " Keys      vim").is_some());
        assert!(find(&s, " enter change · esc back").is_some());

        press(&mut app, "enter");
        assert_eq!(kinds(&app), [APP, general, sets]);
        press(&mut app, "down down enter");
        assert_eq!(app.keymap.set(), Emacs);
        assert_eq!(drain(&intended), [Intent::SaveKeys(Emacs)]);
        assert_eq!(kinds(&app), [APP, general]);
        assert!(find(&screen(&app), " Keys      emacs").is_some());
    }

    #[test]
    fn switching_keys_from_the_bar_lists_each_set() {
        let (mut app, intended) = connected(Regular, 132, 36);
        press(&mut app, "ctrl-p");
        intended.try_iter().for_each(drop);
        type_text(&mut app, "Switch key set");
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [LayerKind::Menu(MenuId::KeySets)]);
        let s = screen(&app);
        for row in [" ● regular", "   vim", "   emacs", " enter use · esc back"] {
            assert!(find(&s, row).is_some(), "{row}");
        }
        assert_eq!(drain(&intended), [], "the list reads nothing");

        press(&mut app, "down enter");
        assert_eq!(app.keymap.set(), Vim);
        assert_eq!(drain(&intended), [Intent::SaveKeys(Vim)]);
        assert!(app.layers.is_empty());
    }

    #[test]
    fn developer_and_experimental_say_there_is_nothing_here() {
        for (name, id) in [
            ("Developer", MenuId::Developer),
            ("Experimental features", MenuId::Experimental),
        ] {
            let (mut app, intended) = connected(Vim, 132, 36);
            app.push_layer(APP);
            app.layers[0].selected = row_named(MenuId::AppSettings, name);
            press(&mut app, "enter");
            assert_eq!(kinds(&app), [APP, LayerKind::Menu(id)], "{name}");
            let s = screen(&app);
            assert!(find(&s, " Nothing to set up here on this computer.").is_some());
            assert!(find(&s, &format!(" {} ", id.name())).is_some());
            assert_eq!(drain(&intended), [], "{name}");
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
    fn d_does_nothing_outside_a_menu() {
        for kind in [LayerKind::Keys, LayerKind::Sessions, LayerKind::Sidebar] {
            let mut app = App::new(Vim, 70, 24);
            app.push_layer(kind);
            press(&mut app, "d");
            assert_eq!(app.notice, None, "{kind:?}");
        }
    }

    #[test]
    fn a_sample_menu_row_takes_no_act() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        assert!(find(&screen(&app), "signed in").is_some());
        press(&mut app, "j");
        assert_eq!(app.layers[0].selected, 1);
        for key in ["enter", "d"] {
            press(&mut app, key);
            assert_eq!(app.notice, None, "{key}");
            assert_eq!(kinds(&app), [ACCOUNTS]);
        }
    }

    #[test]
    fn menu_rows_skip_headers_and_clamp() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(NODE);
        assert_eq!(
            app.layers[0].selected,
            row_named(MenuId::NodeSettings, "Node")
        );
        press(&mut app, "j j j j");
        assert_eq!(
            app.layers[0].selected,
            row_named(MenuId::NodeSettings, "Tasks")
        );
        press(&mut app, "k");
        assert_eq!(
            app.layers[0].selected,
            row_named(MenuId::NodeSettings, "Providers")
        );
        press(&mut app, "k k k k k");
        assert_eq!(
            app.layers[0].selected,
            row_named(MenuId::NodeSettings, "Node")
        );
        for _ in 0..20 {
            press(&mut app, "j");
        }
        let last = menu(MenuId::NodeSettings).0.len() - 1;
        assert_eq!(app.layers[0].selected, last);
    }

    #[test]
    fn every_menu_shows_its_rows_and_footer() {
        for (id, text, footer) in [
            (
                MenuId::Accounts,
                "Claude    subscription",
                " gents accounts login",
            ),
            (MenuId::Agents, "+ New agent", " enter make default"),
            (MenuId::Tools, "code review pack   3 tools", " esc back"),
            (MenuId::Tasks, "nightly eval     02:00", " enter run now"),
            (
                MenuId::Mailbox,
                "approve: cargo test",
                " enter open its session",
            ),
            (MenuId::Status, "runtime    not connected", " esc back"),
        ] {
            let mut app = App::new(Vim, 60, 16);
            app.push_layer(LayerKind::Menu(id));
            let s = screen(&app);
            let above_status = &s[..s.len() - 1];
            assert!(find(&s, id.name()).is_some(), "{id:?}");
            assert!(find(&s, text).is_some(), "{id:?}");
            assert!(find(above_status, footer).is_some(), "{id:?}");
        }
    }

    #[test]
    fn footers_name_the_active_key_set() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        assert!(find(&screen(&app), "signs in · esc back").is_some());

        let mut app = App::new(Emacs, 132, 36);
        app.push_layer(ACCOUNTS);
        let s = screen(&app);
        assert!(find(&s, "signs in · ctrl-g back").is_some());
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
        let sets = [
            (Regular, "?", "ctrl-o"),
            (Vim, "esc ?", "ctrl-w h/j/k/l"),
            (Emacs, "ctrl-x ?", "ctrl-x o"),
        ];
        for (set, open, moves) in sets {
            let mut app = App::new(set, 132, 36);
            press(&mut app, open);
            assert_eq!(top(&app), LayerKind::Keys);
            let s = screen(&app);
            for text in ["Move between panes", moves, set.name()] {
                assert!(find(&s, text).is_some(), "{set:?} {text}");
            }
            for (other, _, others) in sets.iter().filter(|(other, ..)| *other != set) {
                assert_eq!(find(&s, others), None, "{set:?} shows {other:?}'s keys");
            }
            assert!(find(&s, "Sample bindings to react to. Switch sets").is_some());
            assert_eq!(find(&s, "HELP"), Some((35, 1)));
            assert!(find(&s, "clears, or says how to quit").is_some());
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
    fn a_question_mark_opens_the_keys_only_from_an_empty_box_in_regular() {
        let mut app = App::new(Regular, 132, 36);
        type_text(&mut app, "why?");
        assert_eq!(app.tabs[0].input.text(), "why?");
        assert!(app.layers.is_empty());
        press(&mut app, "ctrl-u ?");
        assert_eq!(kinds(&app), [LayerKind::Keys]);

        let mut app = App::new(Emacs, 132, 36);
        type_text(&mut app, "?");
        assert_eq!(app.tabs[0].input.text(), "?");
        assert!(app.layers.is_empty());
    }

    #[test]
    fn the_keys_hint_says_the_hidden_row_is_a_scroll_away() {
        let sets = [
            (Regular, "?", "down", 22),
            (Vim, "esc ?", "j", 23),
            (Emacs, "ctrl-x ?", "ctrl-n", 22),
        ];
        for (set, open, down, height) in sets {
            let mut app = App::new(set, 80, height);
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
        let mut app = with_sample_session(App::new(Vim, 104, 30));
        press(&mut app, "esc ctrl-w ]");
        assert!(find(&screen(&app), "3 files   +").is_some());
        press(&mut app, "enter");
        assert!(find(&screen(&app), "+pub fn run() {").is_some());
        press(&mut app, "esc");
        assert_eq!(app.diff, None);
        assert_eq!(kinds(&app), [LayerKind::Sidebar]);
        assert!(find(&screen(&app), "3 files   +").is_some());
        press(&mut app, "esc");
        assert!(app.layers.is_empty());
    }

    #[test]
    fn sidebar_layer_reaches_a_tall_diffs_last_row() {
        let mut app = with_sample_session(App::new(Vim, 60, 16));
        press(&mut app, "esc ctrl-w ]");
        press(&mut app, "j j enter");
        assert_eq!(find(&screen(&app), "esc back to files"), None);
        press(&mut app, "j j j");
        assert!(find(&screen(&app), "esc back to files").is_some());
    }

    #[test]
    fn back_order_with_nothing_above_a_docked_diff() {
        let mut app = with_right_pane(with_sample_session(App::new(Vim, 132, 36)));
        press(&mut app, "esc ctrl-w l ctrl-w l enter");
        app.push_layer(ACCOUNTS);
        press(&mut app, "esc");
        assert!(app.layers.is_empty());
        assert_eq!(app.diff, Some(2));
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

    #[test]
    fn opening_a_menu_asks_the_source_for_it() {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.push_layer(TASKS);
        let sent: Vec<_> = intended.try_iter().collect();
        assert_eq!(sent, [Intent::ReadMenu(MenuId::Tasks)]);

        app.push_layer(NODE);
        assert!(intended.try_recv().is_err(), "Node settings reads nothing");

        app.layers.clear();
        press(&mut app, "ctrl-p");
        assert_eq!(kinds(&app), [LayerKind::Jump]);
        let sent: Vec<_> = intended.try_iter().collect();
        assert_eq!(sent, [Intent::ReadMenu(MenuId::Workflows)]);
    }

    #[test]
    fn a_menu_shows_reading_then_its_rows_or_why_not() {
        let (mut app, _intended) = connected(Vim, 132, 36);
        app.push_layer(TASKS);
        assert_eq!(
            shown(&app, MenuId::Tasks),
            [(" reading…".to_string(), Tone::Label)]
        );
        let s = screen(&app);
        assert!(find(&s, " reading…").is_some());
        assert!(find(&s, " enter run now · d on/off · esc back").is_some());

        read(&mut app, MenuId::Tasks, Vec::new());
        assert_eq!(
            shown(&app, MenuId::Tasks),
            [(" none".to_string(), Tone::Label)]
        );

        let why = " could not read: jobs and the mailbox are off: x";
        app.apply(Update::Menu {
            menu: MenuId::Tasks,
            rows: Err("jobs and the mailbox are off: x".to_string()),
        });
        assert_eq!(shown(&app, MenuId::Tasks), [(why.to_string(), Tone::Warn)]);
        assert!(find(&screen(&app), why).is_some());
    }

    #[test]
    fn enter_and_d_run_the_rows_acts() {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.push_layer(TASKS);
        let run = MenuAct::RunTask {
            task: "t".to_string(),
        };
        let off = MenuAct::SetEnabled {
            doc: Doc::Task,
            id: "t".to_string(),
            label: "T".to_string(),
            enabled: false,
        };
        let rows = vec![
            menu_row(" t     02:00", Some(run.clone()), Some(off.clone())),
            menu_row(" u      idle", None, None),
        ];
        read(&mut app, MenuId::Tasks, rows);
        intended.try_iter().for_each(drop);

        let act = |act| Intent::Act {
            menu: MenuId::Tasks,
            act,
        };
        press(&mut app, "enter");
        assert_eq!(intended.try_iter().collect::<Vec<_>>(), [act(run)]);
        press(&mut app, "d");
        assert_eq!(intended.try_iter().collect::<Vec<_>>(), [act(off)]);

        press(&mut app, "j");
        for key in ["enter", "d"] {
            press(&mut app, key);
            assert!(intended.try_recv().is_err(), "{key}");
            assert_eq!(app.notice, None, "{key}");
            assert_eq!(kinds(&app), [TASKS], "{key}");
        }
    }

    #[test]
    fn a_mailbox_row_opens_its_session() {
        let mailbox = LayerKind::Menu(MenuId::Mailbox);
        let open = |session: &str| {
            let session = session.to_string();
            Some(MenuAct::OpenSession { session })
        };
        let rows = vec![
            menu_row(" ! approve: cargo test", open("flaky recovery test"), None),
            menu_row(" ! from an unlisted session", open("gone"), None),
        ];
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(mailbox);
        read(&mut app, MenuId::Mailbox, rows.clone());
        press(&mut app, "enter");
        assert!(app.layers.is_empty());
        let tab = &app.tabs[app.active];
        assert_eq!(tab.session.as_deref(), Some("flaky recovery test"));
        assert_eq!(tab.name, "flaky");

        app.push_layer(mailbox);
        read(&mut app, MenuId::Mailbox, rows);
        press(&mut app, "j enter");
        assert_eq!(
            app.notice.as_deref(),
            Some("that session is not in the list")
        );
        assert_eq!(kinds(&app), [mailbox]);
    }

    #[test]
    fn node_settings_workflows_opens_the_installed_list() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(NODE);
        app.layers[0].selected = row_named(MenuId::NodeSettings, "Workflows");
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [NODE, LayerKind::Menu(MenuId::Workflows)]);
    }

    #[test]
    fn a_menu_on_a_lost_shell_says_why_without_asking() {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.apply(Update::Connection(Connection::Lost("gone".to_string())));
        app.push_layer(LayerKind::Menu(MenuId::Status));
        assert!(intended.try_recv().is_err());
        assert!(find(&screen(&app), " could not read: gone").is_some());
    }

    #[test]
    fn rows_for_another_menu_leave_the_open_one_alone() {
        let (mut app, _intended) = connected(Vim, 132, 36);
        app.push_layer(TASKS);
        read(&mut app, MenuId::Tasks, plain(&[" a", " b", " c"]));
        press(&mut app, "j");
        assert_eq!(selected_text(&app, MenuId::Tasks), " b");

        read(&mut app, MenuId::Mailbox, plain(&[" m"]));
        assert_eq!(kinds(&app), [TASKS]);
        assert_eq!(app.layers[0].selected, 1);
        assert_eq!(selected_text(&app, MenuId::Tasks), " b");
        assert_eq!(app.menus.get(&MenuId::Mailbox), Some(&Ok(plain(&[" m"]))));
    }

    #[test]
    fn a_reread_with_fewer_rows_keeps_the_selection_in_range() {
        let (mut app, _intended) = connected(Vim, 132, 36);
        app.push_layer(TASKS);
        read(&mut app, MenuId::Tasks, plain(&[" a", " b", " c"]));
        press(&mut app, "j j");
        assert_eq!(selected_text(&app, MenuId::Tasks), " c");

        read(&mut app, MenuId::Tasks, plain(&[" a"]));
        assert_eq!(app.layers[0].selected, 0);
        assert_eq!(selected_text(&app, MenuId::Tasks), " a");
    }

    #[test]
    fn a_menu_beneath_the_bar_keeps_its_selection_in_range() {
        let (mut app, intended) = connected(Vim, 132, 36);
        let mailbox = LayerKind::Menu(MenuId::Mailbox);
        app.push_layer(mailbox);
        read(&mut app, MenuId::Mailbox, plain(&[" a", " b", " c", " d"]));
        press(&mut app, "j j j :");
        assert_eq!(kinds(&app), [mailbox, LayerKind::Jump]);

        let dismiss = MenuAct::Dismiss {
            item: "a".to_string(),
        };
        let left = menu_row(" a", None, Some(dismiss.clone()));
        read(&mut app, MenuId::Mailbox, vec![left]);
        assert_eq!(app.layers[0].selected, 0);
        screen(&app);
        press(&mut app, "esc");
        assert_eq!(kinds(&app), [mailbox]);
        intended.try_iter().for_each(drop);

        press(&mut app, "d");
        let sent: Vec<_> = intended.try_iter().collect();
        let act = Intent::Act {
            menu: MenuId::Mailbox,
            act: dismiss,
        };
        assert_eq!(sent, [act]);
        press(&mut app, "k j");
        assert_eq!(selected_text(&app, MenuId::Mailbox), " a");
    }

    #[test]
    fn a_read_still_out_when_the_connection_is_lost_says_why() {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.push_layer(LayerKind::Menu(MenuId::Status));
        let sent: Vec<_> = intended.try_iter().collect();
        assert_eq!(sent, [Intent::ReadMenu(MenuId::Status)]);

        let lost = Connection::Lost("no runtime".to_string());
        app.apply(Update::Connection(lost));
        let why = " could not read: no runtime";
        assert_eq!(shown(&app, MenuId::Status), [(why.to_string(), Tone::Warn)]);
        assert!(find(&screen(&app), why).is_some());
    }

    #[test]
    fn sample_menus_keep_their_rows() {
        for id in MENUS {
            let mut app = App::new(Vim, 132, 36);
            app.push_layer(LayerKind::Menu(id));
            let rows = app.menu_rows(id).0;
            let first = &menu(id).0[0];
            assert_eq!(rows[0].text, first.text, "{id:?}");
            assert_eq!(rows[0].action, RowAction::MenuRow(0), "{id:?}");
            assert!(find(&screen(&app), first.text.trim()).is_some(), "{id:?}");
        }

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "ctrl-p");
        type_text(&mut app, "review_c");
        let s = screen(&app);
        let (header, _) = find(&s, " WORKFLOWS").expect("the group");
        let listed = find(&s, "review_changes").map(|(row, _)| row);
        assert_eq!(listed, Some(header + 1));

        let act = Intent::Act {
            menu: MenuId::Tasks,
            act: MenuAct::RunTask {
                task: "t".to_string(),
            },
        };
        assert_eq!(
            sample::answer(&act),
            vec![Update::Notice("not wired yet".to_string())]
        );
    }

    const PACKS: LayerKind = LayerKind::Menu(MenuId::Packs);
    const ASK: LayerKind = LayerKind::Menu(MenuId::Ask);

    fn allow(pack: &str) -> MenuAct {
        MenuAct::Pack(PackAct::Install {
            pack: pack.to_string(),
            edited: EditedChoice::Refuse,
            grant: true,
            slots: Vec::new(),
        })
    }

    /// The source's question for an install of `pack` that asks for authority.
    fn install_question(pack: &str) -> Update {
        let refusal = " plugin acme/http asks for HTTP to api.example.com; install it with --grant-authority to allow that";
        let rows = vec![
            MenuRow {
                tone: Tone::Warn,
                ..menu_row(refusal, None, None)
            },
            MenuRow {
                tone: Tone::Label,
                ..menu_row(" Allow it?", None, None)
            },
            menu_row(" Allow", Some(allow(pack)), None),
        ];
        let title = format!("Install {pack}");
        Update::Ask { title, rows }
    }

    /// Packs, and over it the question for installing acme/git.
    fn asked() -> (App, Receiver<Intent>) {
        let (mut app, intended) = connected(Vim, 132, 36);
        app.push_layer(PACKS);
        intended.try_iter().for_each(drop);
        app.apply(install_question("acme/git"));
        (app, intended)
    }

    #[test]
    fn an_ask_opens_over_the_menu_and_enter_runs_the_row() {
        let (mut app, intended) = asked();

        assert_eq!(kinds(&app), [PACKS, ASK]);
        assert_eq!(drain(&intended), [], "an Ask reads nothing");
        let rect = app.top_layer_rect().expect("the Ask");
        let s = screen(&app);
        assert!(s[usize::from(rect.y)].contains(" Install acme/git "));
        assert!(find(&s, " enter choose · esc cancel").is_some());
        let rows = shown(&app, MenuId::Ask);
        let warn = |text: &str| (text.to_string(), Tone::Warn);
        assert_eq!(
            rows[..2],
            [
                warn(" plugin acme/http asks for HTTP to api.example.com; install it with"),
                warn(" --grant-authority to allow that"),
            ],
            "the refusal wraps inside the layer"
        );
        let allow_row = rows.iter().position(|(text, _)| text == " Allow");

        press(&mut app, &"j ".repeat(allow_row.expect("Allow")));
        press(&mut app, "enter");
        let act = Intent::Act {
            menu: MenuId::Ask,
            act: allow("acme/git"),
        };
        assert_eq!(drain(&intended), [act]);
        assert_eq!(kinds(&app), [PACKS]);
    }

    #[test]
    fn a_second_ask_replaces_the_first_with_its_cursor_on_top() {
        let (mut app, intended) = asked();
        press(&mut app, "j j j");
        assert_eq!(selected_text(&app, MenuId::Ask), " Allow");

        app.apply(install_question("acme/ocr"));

        assert_eq!(kinds(&app), [PACKS, ASK]);
        assert_eq!(app.layers[1].selected, 0);
        let rect = app.top_layer_rect().expect("the Ask");
        assert!(screen(&app)[usize::from(rect.y)].contains(" Install acme/ocr "));
        press(&mut app, "enter");
        assert_eq!(
            drain(&intended),
            [],
            "acme/git's cursor does not allow acme/ocr"
        );
    }

    #[test]
    fn esc_leaves_an_ask_without_acting() {
        let (mut app, intended) = asked();

        press(&mut app, "enter");
        assert_eq!(kinds(&app), [PACKS, ASK], "the refusal is no choice");
        press(&mut app, "esc");

        assert_eq!(kinds(&app), [PACKS]);
        assert_eq!(drain(&intended), []);
    }
}
