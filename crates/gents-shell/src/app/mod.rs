pub mod input;
pub mod layers;
pub mod search;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::commands::{Command, Effect, COMMANDS};
use crate::filter::subsequence;
use crate::keys::{in_key_set, Action, Dir, KeyCtx, KeySet, Keymap};
use crate::layout::{
    conversation_area, layout, sessions_can_dock, sidebar_can_dock, sidebar_rows_area, Panes,
    Right, Screen, Wanted,
};
use crate::sample::{
    diff, view_rows, RowAction, SidebarRow, Tone, View, CHAT, DIFF_BACK_ROW, PROMPT_ROW, SESSIONS,
    START_SESSION,
};
use input::LineInput;
use layers::{Layer, LayerKind};
use search::Search;

const SAMPLE_REPLY: &str = "sample reply: nothing is wired to an agent yet.";
const NOT_WIRED: &str = "not wired yet";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sessions,
    Agent,
    Sidebar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Input,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub text: String,
    pub tone: Tone,
}

/// `scroll` counts lines up from the bottom. No `lines` means no messages yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub session: Option<usize>,
    pub name: String,
    pub agent: String,
    pub lines: Vec<ChatLine>,
    pub scroll: usize,
    pub input: LineInput,
    pub agent_picked: bool,
}

impl Tab {
    pub fn new_session() -> Tab {
        Tab {
            session: None,
            name: "new session".to_string(),
            agent: "Engineer".to_string(),
            lines: Vec::new(),
            scroll: 0,
            input: LineInput::default(),
            agent_picked: false,
        }
    }

    pub fn wrapped_rows(&self, width: u16) -> Vec<(String, Tone)> {
        self.lines
            .iter()
            .flat_map(|line| {
                wrap(&line.text, usize::from(width))
                    .into_iter()
                    .map(|row| (row, line.tone))
            })
            .collect()
    }
}

/// Breaks between grapheme clusters, the units `Buffer` draws, so a cluster stays whole.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let (mut row, mut used) = (String::new(), 0);
    for grapheme in Span::raw(text).styled_graphemes(Style::new()) {
        let cell = Span::raw(grapheme.symbol).width();
        if used + cell > width && used > 0 {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push_str(grapheme.symbol);
        used += cell;
    }
    rows.push(row);
    rows
}

pub fn sample_chat(agent: &str) -> Vec<ChatLine> {
    CHAT.iter()
        .map(|&(text, tone)| ChatLine {
            text: text.replace("Engineer", agent),
            tone,
        })
        .collect()
}

pub struct App {
    pub size: (u16, u16),
    pub keymap: Keymap,
    pub focus: Pane,
    pub mode: Mode,
    pub wanted: Wanted,
    pub view: View,
    pub sessions_sel: usize,
    pub view_sel: [usize; 3],
    pub notice: Option<String>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub diff: Option<usize>,
    pub diff_scroll: usize,
    pub layers: Vec<Layer>,
    /// The selected row of the open slash list, as a position in `slash_matches`.
    pub slash: Option<usize>,
    pub search: Option<Search>,
    quit: bool,
}

fn first_selectable(rows: &[SidebarRow]) -> usize {
    rows.iter().position(SidebarRow::selectable).unwrap_or(0)
}

/// One step within `0..=max`. A value left past `max` by a resize comes back to `max` first,
/// so the step moves at once.
fn stepped(value: usize, increase: bool, max: usize) -> usize {
    let value = value.min(max);
    if increase {
        (value + 1).min(max)
    } else {
        value.saturating_sub(1)
    }
}

fn step_row(rows: &[SidebarRow], from: usize, forward: bool) -> usize {
    let found = if forward {
        (from + 1..rows.len()).find(|&index| rows[index].selectable())
    } else {
        (0..from).rev().find(|&index| rows[index].selectable())
    };
    found.unwrap_or(from)
}

impl App {
    pub fn new(set: KeySet, width: u16, height: u16) -> App {
        App {
            size: (width, height),
            keymap: Keymap::new(set),
            focus: Pane::Agent,
            mode: Mode::Input,
            wanted: Wanted::default(),
            view: View::Changes,
            sessions_sel: START_SESSION,
            view_sel: [View::Changes, View::Workflows, View::Agents]
                .map(|view| first_selectable(view_rows(view))),
            notice: None,
            tabs: vec![Tab::new_session()],
            active: 0,
            diff: None,
            diff_scroll: 0,
            layers: Vec::new(),
            slash: None,
            search: None,
            quit: false,
        }
    }

    pub fn chip(&self) -> &'static str {
        match self.layers.last().map(|top| top.kind) {
            Some(LayerKind::Jump) => "JUMP",
            Some(LayerKind::Keys) => "HELP",
            Some(_) => "MENU",
            None if self.search.is_some() => "SEARCH",
            None => match (self.keymap.set(), self.mode) {
                (KeySet::Emacs, _) => "EMACS",
                (KeySet::Vim, Mode::Input) => "INPUT",
                (KeySet::Vim, Mode::Normal) => "NORMAL",
            },
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn ctx(&self) -> KeyCtx {
        match self.layers.last().map(|top| top.kind) {
            Some(LayerKind::Jump | LayerKind::Picker) => KeyCtx::Filter,
            Some(_) => KeyCtx::Menu,
            None if self.search.as_ref().is_some_and(|search| !search.committed) => {
                KeyCtx::SearchLine
            }
            None if self.keymap.set() == KeySet::Vim && self.mode == Mode::Normal => KeyCtx::Normal,
            None => KeyCtx::Input,
        }
    }

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Resize(width, height) => {
                self.size = (width, height);
                if self.panes().is_some() {
                    let before = self.layers.len();
                    self.layers.retain(|layer| match layer.kind {
                        LayerKind::Sessions => !sessions_can_dock(width),
                        LayerKind::Sidebar => !sidebar_can_dock(width),
                        _ => true,
                    });
                    // A held chord was meant for that layer; its next key must not reach the tab.
                    if self.layers.len() < before {
                        self.keymap.cancel_pending();
                    }
                }
                self.recheck_focus();
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Esc and the next key in one read reach crossterm as alt-<key>
        // (crossterm-0.29.0/src/event/sys/unix/parse.rs:34-42).
        if self.keymap.set() == KeySet::Vim && key.modifiers.contains(KeyModifiers::ALT) {
            self.press(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            self.press(KeyEvent::new(key.code, key.modifiers - KeyModifiers::ALT));
        } else {
            self.press(key);
        }
    }

    fn press(&mut self, key: KeyEvent) {
        self.notice = None;
        let action = self.keymap.map(self.ctx(), key);
        if self.layers.is_empty() {
            self.act(action);
        } else {
            self.act_on_layer(action);
        }
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Insert(_)
            | Action::Backspace
            | Action::Delete
            | Action::DeleteWord
            | Action::CursorLeft
            | Action::CursorRight
            | Action::Home
            | Action::End => self.edit(action),
            Action::Submit if self.ctx() == KeyCtx::SearchLine => self.submit_search(),
            Action::Submit if self.slash.is_some() => self.run_slash(),
            Action::Next if self.slash.is_some() => self.step_slash(true),
            Action::Prev if self.slash.is_some() => self.step_slash(false),
            Action::Complete => self.complete_slash(),
            Action::Submit => self.submit(),
            Action::Back => self.back(),
            Action::Interrupt => self.interrupt(),
            Action::EnterInput => {
                self.search = None;
                self.focus = Pane::Agent;
                self.mode = Mode::Input;
            }
            Action::MoveFocus(dir) => self.move_focus(dir),
            Action::NextPane => self.next_pane(),
            Action::NewTab => self.push_layer(LayerKind::Picker),
            Action::OpenKeys => self.push_layer(LayerKind::Keys),
            Action::OpenBar if self.ctx() != KeyCtx::SearchLine => {
                self.push_layer(LayerKind::Jump);
            }
            Action::OpenSearch => self.open_search(),
            Action::SearchNext if self.search.is_some() => self.step_match(true),
            Action::SearchPrev if self.search.is_some() => self.step_match(false),
            Action::NextTab => self.step_tab(1),
            Action::PrevTab => self.step_tab(self.tabs.len() - 1),
            Action::ClosePane => self.close_pane(),
            Action::ToggleSessions => self.toggle_sessions(),
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::NextView => self.next_view_if_shown(),
            Action::Next => self.step(true),
            Action::Prev => self.step(false),
            Action::NotWired | Action::SearchNext if self.shows_prompt() => self.not_wired(),
            Action::Quit => self.quit = true,
            _ => {}
        }
    }

    fn edit(&mut self, action: Action) {
        if self.ctx() == KeyCtx::SearchLine {
            self.edit_search(action);
            return;
        }
        if matches!(action, Action::Insert(_)) {
            self.focus = Pane::Agent;
        }
        if self.focus == Pane::Agent {
            self.edit_message(action);
        }
    }

    fn edit_message(&mut self, action: Action) {
        let input = &mut self.tabs[self.active].input;
        input.edit(action);
        if matches!(action, Action::Insert(_)) && input.text() == "/" {
            self.slash = Some(0);
        } else if !input.text().starts_with('/') {
            self.slash = None;
        } else if matches!(
            action,
            Action::Insert(_) | Action::Backspace | Action::Delete | Action::DeleteWord
        ) {
            self.slash = self.slash.map(|_| 0);
        }
    }

    pub fn slash_matches(&self) -> Vec<usize> {
        let text = self.tabs[self.active].input.text();
        let query = text.strip_prefix('/').unwrap_or(text);
        (0..COMMANDS.len())
            .filter(|&index| {
                COMMANDS[index]
                    .slash
                    .is_some_and(|name| subsequence(query, name))
            })
            .collect()
    }

    fn selected_slash(&self) -> Option<&'static Command> {
        let index = *self.slash_matches().get(self.slash?)?;
        Some(&COMMANDS[index])
    }

    fn run_slash(&mut self) {
        let Some(command) = self.selected_slash() else {
            let text = self.tabs[self.active].input.text();
            self.notice = Some(format!("unknown command: {text}"));
            return;
        };
        self.tabs[self.active].input.clear();
        self.slash = None;
        self.run_effect(command.effect);
    }

    fn step_slash(&mut self, forward: bool) {
        let max = self.slash_matches().len().saturating_sub(1);
        self.slash = self.slash.map(|selected| stepped(selected, forward, max));
    }

    fn complete_slash(&mut self) {
        if let Some(name) = self.selected_slash().and_then(|command| command.slash) {
            self.tabs[self.active].input.set(&format!("/{name}"));
            self.slash = Some(0);
        }
    }

    pub fn run_effect(&mut self, effect: Effect) {
        if !matches!(effect, Effect::OpenPicker | Effect::OpenKeys) {
            self.layers.clear();
        }
        match effect {
            Effect::OpenPicker => self.push_layer(LayerKind::Picker),
            Effect::OpenKeys => self.push_layer(LayerKind::Keys),
            Effect::ClearTab => {
                let tab = &mut self.tabs[self.active];
                tab.lines.clear();
                tab.scroll = 0;
                self.activate_tab(self.active);
            }
            Effect::ShowSidebar(view) => {
                self.set_view(view);
                if sidebar_can_dock(self.size.0) {
                    self.wanted.sidebar = true;
                } else {
                    self.push_layer(LayerKind::Sidebar);
                }
            }
            Effect::NotWired => self.not_wired(),
            Effect::SwitchKeys => {
                self.keymap = Keymap::new(match self.keymap.set() {
                    KeySet::Vim => KeySet::Emacs,
                    KeySet::Emacs => KeySet::Vim,
                });
                self.activate_tab(self.active);
            }
            Effect::Quit => self.quit = true,
        }
    }

    fn submit(&mut self) {
        match self.focus {
            Pane::Agent if self.ctx() == KeyCtx::Input => self.send(),
            Pane::Agent => {}
            Pane::Sessions => self.activate_session_row(),
            Pane::Sidebar => self.activate_view_row(),
        }
    }

    fn back(&mut self) {
        if let Some(top) = self.layers.last() {
            if top.kind == LayerKind::Sidebar && self.diff.is_some() {
                self.diff = None;
            } else {
                self.layers.pop();
            }
        } else if self.slash.take().is_some() {
        } else if self.search.is_some() {
            self.back_from_search();
        } else if self.diff.is_some() && self.focus == Pane::Sidebar {
            self.diff = None;
        } else {
            self.mode = Mode::Normal;
        }
    }

    fn send(&mut self) {
        let tab = &mut self.tabs[self.active];
        if tab.input.text().trim().is_empty() {
            return;
        }
        let rows = [
            ("you", Tone::Accent),
            (tab.input.text(), Tone::Plain),
            ("", Tone::Plain),
            (tab.agent.as_str(), Tone::Accent),
            (SAMPLE_REPLY, Tone::Label),
            ("", Tone::Plain),
        ]
        .map(|(text, tone)| ChatLine {
            text: text.to_string(),
            tone,
        });
        tab.lines.extend(rows);
        tab.input.clear();
        tab.scroll = 0;
    }

    fn interrupt(&mut self) {
        let searching = self.ctx() == KeyCtx::SearchLine;
        let input = match (self.layers.last_mut(), self.search.as_mut()) {
            (Some(top), _) => &mut top.filter,
            (None, Some(search)) if searching => &mut search.query,
            _ => &mut self.tabs[self.active].input,
        };
        if input.is_empty() {
            self.notice = Some(self.quit_hint().to_string());
        } else {
            input.clear();
            if self.layers.is_empty() {
                self.slash = None;
            }
            if searching {
                self.go_to_match();
            }
        }
    }

    /// `:` opens the bar only in normal mode and menus. Anywhere else it is text, and `esc` leaves
    /// each layer before it leaves input mode.
    fn quit_hint(&self) -> &'static str {
        let esc_twice = match self.layers.iter().rev().nth(1) {
            Some(beneath) => matches!(beneath.kind, LayerKind::Jump | LayerKind::Picker),
            None => self.mode == Mode::Input,
        };
        match (self.keymap.set(), self.ctx()) {
            (KeySet::Emacs, _) => "ctrl-x ctrl-c to quit",
            (KeySet::Vim, KeyCtx::Normal | KeyCtx::Menu) => ":q then enter to quit",
            (KeySet::Vim, KeyCtx::Filter) if esc_twice => "esc twice, then :q then enter to quit",
            (KeySet::Vim, _) => "esc first, then :q then enter to quit",
        }
    }

    fn not_wired(&mut self) {
        self.notice = Some(NOT_WIRED.to_string());
    }

    fn shows_prompt(&self) -> bool {
        let lines = &self.tabs[self.active].lines;
        lines.iter().any(|line| line.text == PROMPT_ROW)
    }

    fn panes(&self) -> Option<Panes> {
        match layout(self.size.0, self.size.1, self.wanted) {
            Screen::Shell(panes) => Some(panes),
            Screen::TooSmall => None,
        }
    }

    fn focus_pane(&mut self, pane: Pane) {
        if self.focus == pane {
            return;
        }
        self.focus = pane;
        self.search = None;
        if pane != Pane::Agent {
            self.mode = Mode::Normal;
            self.slash = None;
        }
    }

    /// The too-small screen shows no panes but keeps the focus, for when the window grows back.
    fn recheck_focus(&mut self) {
        let Some(panes) = self.panes() else {
            return;
        };
        let visible = match self.focus {
            Pane::Sessions => panes.sessions.is_some(),
            Pane::Agent => true,
            Pane::Sidebar => matches!(panes.right, Right::Sidebar(_)),
        };
        if !visible {
            self.focus_pane(Pane::Agent);
        }
    }

    fn move_focus(&mut self, dir: Dir) {
        let Some(panes) = self.panes() else {
            return;
        };
        match (self.focus, dir) {
            (Pane::Agent, Dir::Left) if panes.sessions.is_some() => {
                self.focus_pane(Pane::Sessions);
            }
            (Pane::Agent, Dir::Right) if matches!(panes.right, Right::Sidebar(_)) => {
                self.focus_pane(Pane::Sidebar);
            }
            (Pane::Agent, Dir::Left) if !sessions_can_dock(self.size.0) => {
                self.push_layer(LayerKind::Sessions);
            }
            (Pane::Agent, Dir::Right) if !sidebar_can_dock(self.size.0) => {
                self.push_layer(LayerKind::Sidebar);
            }
            (Pane::Sessions, Dir::Right) | (Pane::Sidebar, Dir::Left) => {
                self.focus_pane(Pane::Agent);
            }
            _ => {}
        }
    }

    fn next_pane(&mut self) {
        let Some(panes) = self.panes() else {
            return;
        };
        let sidebar = matches!(panes.right, Right::Sidebar(_));
        let next = match self.focus {
            Pane::Agent if sidebar => Pane::Sidebar,
            Pane::Agent | Pane::Sidebar if panes.sessions.is_some() => Pane::Sessions,
            _ => Pane::Agent,
        };
        self.focus_pane(next);
    }

    fn step_tab(&mut self, by: usize) {
        if self.tabs.len() > 1 {
            self.activate_tab((self.active + by) % self.tabs.len());
        }
    }

    pub fn activate_tab(&mut self, index: usize) {
        self.active = index;
        self.slash = None;
        self.search = None;
        self.focus = Pane::Agent;
        self.mode = if self.tabs[index].lines.is_empty() {
            Mode::Input
        } else {
            Mode::Normal
        };
    }

    pub fn open_tab_with_agent(&mut self, agent: &str) {
        self.layers.clear();
        let mut tab = Tab::new_session();
        tab.agent = agent.to_string();
        tab.agent_picked = true;
        self.tabs.push(tab);
        self.activate_tab(self.tabs.len() - 1);
    }

    pub fn close_tab(&mut self) {
        self.tabs.remove(self.active);
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new_session());
        }
        self.activate_tab(self.active.saturating_sub(1));
    }

    fn close_pane(&mut self) {
        match self.focus {
            Pane::Agent => self.close_tab(),
            Pane::Sessions => self.wanted.sessions = false,
            Pane::Sidebar => self.wanted.sidebar = false,
        }
        self.recheck_focus();
    }

    /// The untouched starter tab may become the session's. Any other keeps its draft and its
    /// picked agent, because a tab cannot switch agent.
    pub fn open_session(&mut self, row: usize) {
        let RowAction::OpenSession { tab: name, agent } = SESSIONS[row].action else {
            return;
        };
        self.layers.clear();
        if let Some(index) = self.tabs.iter().position(|tab| tab.session == Some(row)) {
            self.activate_tab(index);
            return;
        }
        let current = &self.tabs[self.active];
        let untouched = current.session.is_none()
            && current.lines.is_empty()
            && current.input.is_empty()
            && !current.agent_picked;
        if !untouched {
            self.tabs.push(Tab::new_session());
            self.active = self.tabs.len() - 1;
        }
        let tab = &mut self.tabs[self.active];
        tab.session = Some(row);
        tab.name = name.to_string();
        tab.agent = agent.to_string();
        tab.lines = sample_chat(agent);
        self.activate_tab(self.active);
    }

    fn toggle_sessions(&mut self) {
        if sessions_can_dock(self.size.0) {
            self.wanted.sessions = !self.wanted.sessions;
            self.recheck_focus();
        } else {
            self.toggle_layer(LayerKind::Sessions);
        }
    }

    fn toggle_sidebar(&mut self) {
        if sidebar_can_dock(self.size.0) {
            self.wanted.sidebar = !self.wanted.sidebar;
            self.recheck_focus();
        } else {
            self.toggle_layer(LayerKind::Sidebar);
        }
    }

    fn next_view_if_shown(&mut self) {
        let right = self.panes().map(|panes| panes.right);
        if matches!(right, Some(Right::Sidebar(_))) {
            self.next_view();
        } else {
            self.notice = Some(self.no_sidebar_hint().to_string());
        }
    }

    fn no_sidebar_hint(&self) -> &'static str {
        match self.keymap.set() {
            KeySet::Vim => "no right sidebar on screen: ctrl-w ] opens it",
            KeySet::Emacs => "no right sidebar on screen: ctrl-x ] opens it",
        }
    }

    fn next_view(&mut self) {
        self.set_view(match self.view {
            View::Changes => View::Workflows,
            View::Workflows => View::Agents,
            View::Agents => View::Changes,
        });
    }

    /// A diff belongs to the Changes view, so it closes when the view changes.
    pub fn set_view(&mut self, view: View) {
        self.view = view;
        self.diff = None;
        self.search.take_if(|search| search.pane == Pane::Sidebar);
    }

    pub fn diff_rows(&self, file: usize) -> Vec<(String, Tone)> {
        diff(file)
            .iter()
            .map(|&(text, tone)| {
                if text == DIFF_BACK_ROW {
                    (in_key_set(self.keymap.set(), text), tone)
                } else {
                    (text.to_string(), tone)
                }
            })
            .collect()
    }

    fn step(&mut self, forward: bool) {
        match self.focus {
            Pane::Agent => self.scroll_agent(!forward),
            Pane::Sessions => self.step_session(forward),
            Pane::Sidebar => self.step_sidebar(forward),
        }
    }

    fn step_session(&mut self, forward: bool) {
        self.sessions_sel = step_row(SESSIONS, self.sessions_sel, forward);
    }

    fn step_sidebar(&mut self, forward: bool) {
        let Some(file) = self.diff else {
            let selected = &mut self.view_sel[self.view as usize];
            *selected = step_row(view_rows(self.view), *selected, forward);
            return;
        };
        if let Some(pane) = self.sidebar_pane() {
            let rows = self.visible_rows(Pane::Sidebar, sidebar_rows_area(pane));
            let height = usize::from(rows.height);
            let max = diff(file).len().saturating_sub(height);
            self.diff_scroll = stepped(self.diff_scroll, forward, max);
        }
    }

    /// With a layer open, the only sidebar taking keys is that layer.
    fn sidebar_pane(&self) -> Option<Rect> {
        if !self.layers.is_empty() {
            return self.top_layer_rect();
        }
        match self.panes()?.right {
            Right::Sidebar(pane) => Some(pane),
            _ => None,
        }
    }

    fn scroll_agent(&mut self, up: bool) {
        let Some(panes) = self.panes() else {
            return;
        };
        let area = self.visible_rows(Pane::Agent, conversation_area(panes.agent));
        let tab = &mut self.tabs[self.active];
        let lines = tab.wrapped_rows(area.width).len();
        let max = lines.saturating_sub(usize::from(area.height));
        tab.scroll = stepped(tab.scroll, up, max);
    }

    fn activate_session_row(&mut self) {
        match SESSIONS[self.sessions_sel].action {
            RowAction::NewSession => self.push_layer(LayerKind::Picker),
            RowAction::OpenSession { .. } => self.open_session(self.sessions_sel),
            _ => {}
        }
    }

    fn activate_view_row(&mut self) {
        if self.diff.is_some() {
            return;
        }
        match view_rows(self.view)[self.view_sel[self.view as usize]].action {
            RowAction::OpenDiff(file) => {
                self.diff = Some(file);
                self.diff_scroll = 0;
                self.search = None;
            }
            RowAction::NotWired => self.not_wired(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw;
    use crate::draw::theme;
    use crate::keys::KeySet::{Emacs, Vim};
    use crate::testing::{buffer, find, keys, numbered, press, screen, session_row, type_text};
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
        assert_eq!(
            app.notice.as_deref(),
            Some("esc first, then :q then enter to quit")
        );
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
        let app = App::new(Vim, 80, 24);
        assert!(crate::sample::SESSIONS[app.sessions_sel]
            .text
            .contains("cli ux redesign"));
        for (view, selected) in [View::Changes, View::Workflows, View::Agents]
            .into_iter()
            .zip(app.view_sel)
        {
            assert!(view_rows(view)[selected].selectable(), "{view:?}");
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
            let mut app = App::new(set, 132, 36);
            type_text(&mut app, "hi");
            press(&mut app, "enter");
            let s = screen(&app);
            assert_eq!(find(&s, "you"), Some((4, 30)));
            assert_eq!(find(&s, "hi"), Some((5, 30)));
            assert_eq!(find(&s, "Engineer"), Some((1, 46)));
            let reply = "sample reply: nothing is wired to an agent yet.";
            assert_eq!(find(&s, reply), Some((8, 30)));
            assert!(app.tabs[0].input.is_empty());
            assert!(buffer(&app).content().iter().all(|c| c.bg != theme::LOGO));

            let tones: Vec<Tone> = app.tabs[0].lines.iter().map(|l| l.tone).collect();
            use Tone::{Accent, Label, Plain};
            assert_eq!(tones, [Accent, Plain, Plain, Accent, Label, Plain]);
            assert_eq!(app.tabs[0].lines[3].text, "Engineer");
        }
    }

    #[test]
    fn whitespace_only_is_not_sent() {
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "   ");
        press(&mut app, "enter");
        assert!(app.tabs[0].lines.is_empty());
        assert_eq!(app.tabs[0].input.text(), "   ");
    }

    #[test]
    fn surrounding_spaces_are_kept_in_the_message() {
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, " hi ");
        press(&mut app, "enter");
        assert_eq!(app.tabs[0].lines[1].text, " hi ");
    }

    #[test]
    fn enter_in_normal_mode_does_not_send_the_draft() {
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "hi");
        press(&mut app, "esc enter");
        assert!(app.tabs[0].lines.is_empty());
        assert_eq!(app.tabs[0].input.text(), "hi");
    }

    #[test]
    fn ctrl_c_clears_then_explains() {
        for (set, hint) in [
            (Vim, "esc first, then :q then enter to quit"),
            (Emacs, "ctrl-x ctrl-c to quit"),
        ] {
            let mut app = App::new(set, 132, 36);
            type_text(&mut app, "hi");
            press(&mut app, "ctrl-c");
            assert!(app.tabs[0].input.is_empty());
            assert_eq!(app.notice, None);

            press(&mut app, "ctrl-c");
            assert_eq!(app.notice.as_deref(), Some(hint));
            assert!(find(&screen(&app), hint).is_some());
            assert!(!app.should_quit());

            press(&mut app, "esc");
            assert_eq!(app.notice, None);
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
    fn alt_i_in_vim_stays_in_input() {
        let mut app = App::new(Vim, 132, 36);
        app.handle(alt('i'));
        assert_eq!(app.mode, Mode::Input);
        assert!(app.tabs[0].input.is_empty());
    }

    #[test]
    fn pasted_newline_sends_that_line() {
        let mut app = App::new(Vim, 132, 36);
        for code in [KeyCode::Char('a'), KeyCode::Enter, KeyCode::Char('b')] {
            app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        }
        assert_eq!(app.tabs[0].lines[1].text, "a");
        assert_eq!(
            app.tabs[0].lines.iter().filter(|l| l.text == "you").count(),
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

    fn enter_on_session(app: &mut App, text: &str) {
        app.focus = Pane::Sessions;
        app.sessions_sel = session_row(text);
        press(app, "enter");
    }

    #[test]
    fn ctrl_w_moves_between_docked_panes() {
        let mut app = App::new(Vim, 132, 36);
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
        let mut app = App::new(Vim, 132, 36);
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
        let mut app = App::new(Emacs, 132, 36);
        for pane in [Pane::Sidebar, Pane::Sessions, Pane::Agent] {
            press(&mut app, "ctrl-x o");
            assert_eq!(app.focus, pane);
        }
        let mut app = App::new(Emacs, 104, 30);
        for pane in [Pane::Sessions, Pane::Agent] {
            press(&mut app, "ctrl-x o");
            assert_eq!(app.focus, pane);
        }
        let mut app = App::new(Emacs, 70, 24);
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
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc ctrl-w h");
        assert_eq!(app.sessions_sel, session_row("cli ux redesign"));
        for text in ["flaky", "pack registry", "eval baseline"] {
            press(&mut app, "j");
            assert_eq!(app.sessions_sel, session_row(text));
        }
        for _ in 0..20 {
            press(&mut app, "j");
        }
        assert_eq!(app.sessions_sel, SESSIONS.len() - 1);
        for _ in 0..20 {
            press(&mut app, "k");
        }
        assert_eq!(app.sessions_sel, session_row("+ New session"));

        press(&mut app, "ctrl-w l ctrl-w l");
        let files = app.view_sel[View::Changes as usize];
        press(&mut app, "j");
        assert_eq!(app.view_sel[View::Changes as usize], files + 1);
        press(&mut app, "ctrl-p");
        assert_eq!(app.view_sel[View::Changes as usize], files);
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
        assert!(!app.wanted.sessions);
        assert_eq!(app.focus, Pane::Agent);
        assert_eq!(app.tabs.len(), 1);

        let mut app = App::new(Emacs, 132, 36);
        app.focus = Pane::Sidebar;
        press(&mut app, "ctrl-x 0");
        assert!(!app.wanted.sidebar);
        assert_eq!(app.focus, Pane::Agent);
    }

    #[test]
    fn docked_toggles_flip_wanted() {
        let mut app = App::new(Vim, 132, 36);
        let edge = find(&screen(&app), "enter send").map(|(_, column)| column);
        press(&mut app, "esc ctrl-w ]");
        assert!(!app.wanted.sidebar);
        let s = screen(&app);
        assert_eq!(find(&s, "Changes"), None);
        assert!(find(&s, "enter send").map(|(_, column)| column) > edge);

        press(&mut app, "ctrl-w ]");
        assert!(app.wanted.sidebar);
        assert!(find(&screen(&app), "Changes").is_some());

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-x [");
        assert!(!app.wanted.sessions);
        assert_eq!(find(&screen(&app), "Sessions"), None);
        press(&mut app, "ctrl-x [");
        assert!(app.wanted.sessions);
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
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc ctrl-w ]");
        app.handle(Event::Resize(104, 30));
        assert_eq!(find(&screen(&app), "+58"), None);

        app.handle(Event::Resize(132, 36));
        press(&mut app, "ctrl-w ]");
        app.handle(Event::Resize(104, 30));
        assert!(find(&screen(&app), "+58").is_some());
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
        let mut app = App::new(Vim, 132, 36);
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
    fn view_cycles_and_opens_on_changes() {
        let mut app = App::new(Vim, 132, 36);
        assert_eq!(app.view, View::Changes);
        for view in [View::Workflows, View::Agents, View::Changes] {
            press(&mut app, "esc ctrl-w }");
            assert_eq!(app.view, view);
        }
        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-x }");
        assert_eq!(app.view, View::Workflows);
        assert!(find(&screen(&app), "2 running").is_some());
    }

    #[test]
    fn file_opens_its_diff_and_back_returns() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc ctrl-w l ctrl-w l enter");
        assert_eq!(app.diff, Some(0));
        let s = screen(&app);
        assert!(find(&s, "@@ -85,8 +85,4 @@").is_some());
        assert!(find(&s, "esc back to files").is_some());
        assert_eq!(find(&s, "next hunk"), None);
        assert_eq!(find(&s, "3 files"), None);

        press(&mut app, "esc");
        assert_eq!(app.diff, None);
        assert!(find(&screen(&app), "3 files").is_some());

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-x o enter");
        let s = screen(&app);
        assert!(find(&s, "ctrl-g back to files").is_some());
        assert_eq!(find(&s, "esc back"), None);
        press(&mut app, "ctrl-g");
        assert!(find(&screen(&app), "3 files").is_some());
    }

    #[test]
    fn changing_the_view_leaves_the_diff() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc ctrl-w l ctrl-w l enter ctrl-w }");
        assert_eq!(app.diff, None);
        assert!(find(&screen(&app), "2 running").is_some());
    }

    #[test]
    fn a_tall_diff_scrolls_to_its_last_row_at_the_smallest_height() {
        let mut app = App::new(Vim, 132, 16);
        press(&mut app, "esc ctrl-w l ctrl-w l enter");
        let s = screen(&app);
        assert!(find(&s, "cli/args.rs       +42 -7").is_some());
        assert_eq!(find(&s, "esc back to files"), None);

        press(&mut app, "j");
        let s = screen(&app);
        assert!(find(&s, "esc back to files").is_some());
        assert_eq!(find(&s, "cli/args.rs       +42 -7"), None);

        press(&mut app, "j j j k");
        assert!(find(&screen(&app), "cli/args.rs       +42 -7").is_some());
    }

    #[test]
    fn a_fresh_diff_starts_at_its_top() {
        let mut app = App::new(Vim, 132, 16);
        press(&mut app, "esc ctrl-w l ctrl-w l enter j esc enter");
        assert!(find(&screen(&app), "cli/args.rs       +42 -7").is_some());
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
        assert_eq!(app.tabs[0].lines[1].text, "a/");
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
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "/");
        press(&mut app, "ctrl-n down");
        assert_eq!(app.slash, Some(2));
        press(&mut app, "ctrl-p");
        assert_eq!(app.slash, Some(1));
        press(&mut app, "tab");
        assert_eq!(app.tabs[0].input.text(), "/new");

        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "/");
        for _ in 0..20 {
            press(&mut app, "ctrl-n");
        }
        assert_eq!(app.slash, Some(7));
        type_text(&mut app, "c");
        assert_eq!(app.slash, Some(0));
        press(&mut app, "enter");
        assert!(!app.should_quit());
    }

    #[test]
    fn back_closes_the_list_and_keeps_the_text() {
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
        assert_eq!(app.tabs[0].lines[1].text, "/cx");
        assert!(app.tabs[0].input.is_empty());
    }

    #[test]
    fn backspacing_the_slash_closes_the_list() {
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
    }

    #[test]
    fn text_that_stops_starting_with_a_slash_closes_the_list() {
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "/c");
        press(&mut app, "home");
        assert_eq!(app.slash, Some(0));
        type_text(&mut app, "a");
        assert_eq!(app.tabs[0].input.text(), "a/c");
        assert_eq!(app.slash, None);

        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "/c");
        press(&mut app, "ctrl-c");
        assert!(app.tabs[0].input.is_empty());
        assert_eq!(app.slash, None);
    }

    #[test]
    fn leaving_the_box_closes_the_list() {
        let mut app = App::new(Emacs, 132, 36);
        app.tabs.push(Tab::new_session());
        type_text(&mut app, "/");
        press(&mut app, "ctrl-x n");
        assert_eq!(app.slash, None);
    }

    #[test]
    fn unknown_command_keeps_the_text() {
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
            app.wanted.sidebar = false;
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
        assert!(jobs.wanted.sidebar);
        assert!(jobs.layers.is_empty());
        let jobs = run(70, 24, "/jobs");
        assert_eq!(jobs.view, View::Agents);
        assert_eq!(top(&jobs), Some(LayerKind::Sidebar));

        assert_eq!(top(&run(132, 36, "/help")), Some(LayerKind::Keys));
        for text in ["/model", "/goal"] {
            let app = run(132, 36, text);
            assert_eq!(app.notice.as_deref(), Some("not wired yet"), "{text}");
        }
        assert!(run(132, 36, "/quit").should_quit());
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
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "/");
        assert_eq!(app.slash_matches().len(), 8);
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
        app.open_tab_with_agent("Reviewer");
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

        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "hi");
        app.handle(alt_enter);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.tabs[0].input.text(), "hi");
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

    #[test]
    fn next_view_needs_a_sidebar_on_screen() {
        let hint = "no right sidebar on screen: ctrl-w ] opens it";
        for width in [104, 70] {
            let mut app = App::new(Vim, width, 30);
            press(&mut app, "esc ctrl-w }");
            assert_eq!(app.view, View::Changes);
            assert_eq!(app.notice.as_deref(), Some(hint));
        }

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc ctrl-w l ctrl-w l enter ctrl-w ] ctrl-w }");
        assert_eq!(app.view, View::Changes);
        assert_eq!(app.diff, Some(0));
        assert_eq!(app.notice.as_deref(), Some(hint));

        let mut app = App::new(Emacs, 70, 24);
        press(&mut app, "ctrl-x }");
        assert_eq!(app.view, View::Changes);
        assert!(app
            .notice
            .as_deref()
            .is_some_and(|text| text.contains("ctrl-x ]")));
    }

    #[test]
    fn the_quit_hint_fits_where_it_is_read() {
        let hint = ":q then enter to quit";
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc ctrl-c");
        assert_eq!(app.notice.as_deref(), Some(hint));

        app.push_layer(LayerKind::Menu(crate::sample::MenuId::Accounts));
        press(&mut app, "ctrl-c");
        assert_eq!(app.notice.as_deref(), Some(hint));

        let esc_first = "esc first, then :q then enter to quit";
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc /");
        press(&mut app, "ctrl-c");
        assert_eq!(app.notice.as_deref(), Some(esc_first));
    }

    #[test]
    fn the_quit_hint_counts_what_esc_leaves_first() {
        let esc_twice = "esc twice, then :q then enter to quit";
        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "/agent");
        press(&mut app, "enter ctrl-c");
        assert_eq!(app.notice.as_deref(), Some(esc_twice));
        press(&mut app, "esc esc : q enter");
        assert!(app.should_quit());

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "tabnew");
        press(&mut app, "enter ctrl-c");
        assert_eq!(app.notice.as_deref(), Some(esc_twice));
        press(&mut app, "esc esc : q enter");
        assert!(app.should_quit());

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc : ctrl-c");
        let esc_first = "esc first, then :q then enter to quit";
        assert_eq!(app.notice.as_deref(), Some(esc_first));
        press(&mut app, "esc : q enter");
        assert!(app.should_quit());

        let mut app = App::new(Vim, 132, 36);
        type_text(&mut app, "/help");
        press(&mut app, "enter : ctrl-c");
        assert_eq!(app.notice.as_deref(), Some(esc_first));
        press(&mut app, "esc : q enter");
        assert!(app.should_quit());
    }

    #[test]
    fn sending_on_a_session_tab_leaves_a_blank_after_the_prompt() {
        let mut app = App::new(Vim, 132, 36);
        enter_on_session(&mut app, "flaky");
        press(&mut app, "i");
        type_text(&mut app, "hi");
        press(&mut app, "enter");
        let lines = &app.tabs[0].lines;
        let sent = lines.len() - 6;
        assert_eq!(lines[sent].text, "you");
        assert_eq!(lines[sent - 1].text, "");
        assert_eq!(lines[sent - 2].text, PROMPT_ROW);
    }

    #[test]
    fn only_the_back_row_of_a_diff_changes_with_the_key_set() {
        let emacs = App::new(Emacs, 132, 36);
        for file in 0..3 {
            let rows = emacs.diff_rows(file);
            let (last, rest) = rows.split_last().expect("rows");
            assert_eq!(last.0, " ctrl-g back to files");
            for ((text, tone), (raw, raw_tone)) in rest.iter().zip(diff(file)) {
                assert_eq!(text, raw);
                assert_eq!(tone, raw_tone);
            }
        }
        let vim = App::new(Vim, 132, 36);
        assert_eq!(
            vim.diff_rows(0).last().expect("rows").0,
            " esc back to files"
        );
    }

    #[test]
    fn prompt_keys_are_not_live() {
        let mut app = App::new(Vim, 132, 36);
        enter_on_session(&mut app, "flaky");
        for key in ["y", "n", "a"] {
            press(&mut app, key);
            assert_eq!(app.notice.as_deref(), Some("not wired yet"), "{key}");
            press(&mut app, "x");
            assert_eq!(app.notice, None);
        }

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc");
        for key in ["y", "n", "a"] {
            press(&mut app, key);
            assert_eq!(app.notice, None, "{key}");
        }

        let mut app = App::new(Emacs, 132, 36);
        enter_on_session(&mut app, "flaky");
        press(&mut app, "y");
        assert_eq!(app.notice, None);
        assert_eq!(app.tabs[0].input.text(), "y");

        for view in [View::Workflows, View::Agents] {
            let mut app = App::new(Vim, 132, 36);
            app.view = view;
            press(&mut app, "esc ctrl-w l ctrl-w l enter");
            assert_eq!(app.notice.as_deref(), Some("not wired yet"));
        }
    }
}
