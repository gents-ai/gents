pub mod input;
pub mod layers;
mod mouse;
pub mod search;

use std::sync::mpsc::Sender;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::commands::{Command, Effect, COMMANDS};
use crate::filter::subsequence;
use crate::keys::{in_key_set, in_line, Action, Dir, KeyCtx, KeySet, Keymap};
use crate::layout::{
    conversation_area, layout, sessions_can_dock, sidebar_can_dock, sidebar_rows_area, Panes,
    Right, Screen, Wanted,
};
use crate::sample::{
    answer, diff, note, row, view_rows, Opens, RowAction, Running, SidebarRow, Tone, View, CHAT,
    DIFF_BACK_ROW, PROMPT_ROW, START_SESSION,
};
use crate::source::{Agent, Connection, Day, Intent, Session, SessionState, Update};
use input::LineInput;
use layers::{Layer, LayerKind};
use search::Search;

const NOT_WIRED: &str = "not wired yet";
const SOURCE_GONE: &str = "the runtime source stopped: reopen the shell";
const CONNECTING: &str = "still connecting to the runtime";
const STOPPING: &str = "stopping the turn";
const STARTING: &str = "starting the session, try again in a moment";

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
    /// The transcript item the line shows.
    pub item: Option<String>,
}

fn blank() -> ChatLine {
    ChatLine {
        text: String::new(),
        tone: Tone::Plain,
        item: None,
    }
}

/// `scroll` counts lines up from the bottom. No `lines` means no messages yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// Set by `App::push_tab`; a stepped-in subagent keeps its parent's.
    pub id: u64,
    /// The id of the session the tab shows.
    pub session: Option<String>,
    pub name: String,
    pub agent: String,
    pub agent_id: String,
    pub lines: Vec<ChatLine>,
    pub scroll: usize,
    pub input: LineInput,
    pub agent_picked: bool,
    pub running: &'static [Running],
    /// The selected running row, while those rows have the keys.
    pub running_sel: Option<usize>,
    /// The session's turn in progress.
    pub turn: Option<String>,
    /// A send without a session is waiting for its `Started`.
    pub starting: bool,
    /// Whether the right pane is wanted while this tab shows. Not saved across runs.
    pub sidebar: bool,
    /// The tab to step back out to. Stepping in swaps the subagent's tab into this slot.
    pub parent: Option<Box<Tab>>,
}

impl Tab {
    pub fn new_session() -> Tab {
        Tab {
            id: 0,
            session: None,
            name: "new session".to_string(),
            agent: "Engineer".to_string(),
            agent_id: "Engineer".to_string(),
            lines: Vec::new(),
            scroll: 0,
            input: LineInput::default(),
            agent_picked: false,
            running: &[],
            running_sel: None,
            turn: None,
            starting: false,
            sidebar: false,
            parent: None,
        }
    }

    pub fn wrapped_rows(&self, width: u16) -> Vec<(String, Tone)> {
        let width = usize::from(width);
        self.lines
            .iter()
            .flat_map(|line| {
                let rows = if line.tone == Tone::You {
                    bubble(&line.text, width)
                } else {
                    wrap_lines(&line.text, width)
                };
                rows.into_iter().map(|row| (row, line.tone))
            })
            .collect()
    }

    /// An unseen item appends its line and a blank, and a seen one replaces its line.
    fn put(&mut self, line: ChatLine) {
        let seen = line.item.as_deref().and_then(|item| self.item_line(item));
        match seen {
            Some(index) => self.lines[index] = line,
            None => self.lines.extend([line, blank()]),
        }
    }

    fn set_agent(&mut self, agent: &Agent) {
        self.agent = agent.name.clone();
        self.agent_id = agent.id.clone();
    }

    fn item_line(&self, item: &str) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| line.item.as_deref() == Some(item))
    }
}

fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap(line, width))
        .collect()
}

/// Breaks at the last space that fits, dropping the spaces there, and inside a word only when the row holds no
/// space past its indent. Breaks fall between grapheme clusters, the units `Buffer` draws.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let (mut row, mut used, mut broke) = (String::new(), 0, false);
    for grapheme in Span::raw(text).styled_graphemes(Style::new()) {
        let cell = Span::raw(grapheme.symbol).width();
        if broke && row.is_empty() && grapheme.symbol == " " {
            continue;
        }
        if used + cell > width && used > 0 {
            broke = true;
            if grapheme.symbol == " " {
                rows.push(std::mem::take(&mut row).trim_end().to_string());
                used = 0;
                continue;
            }
            let space = row.rfind(' ').filter(|&at| !row[..at].trim().is_empty());
            let rest = space.map_or(String::new(), |at| {
                row.split_off(at).trim_start().to_string()
            });
            rows.push(std::mem::replace(&mut row, rest).trim_end().to_string());
            used = Span::raw(row.as_str()).width();
        }
        row.push_str(grapheme.symbol);
        used += cell;
    }
    rows.push(row);
    rows
}

/// Padded on the left so one block of text sits against the right edge, leaving a cell of tint
/// on each side. Padding in the text keeps search columns right.
fn bubble(text: &str, width: usize) -> Vec<String> {
    let room = (width * 3 / 4).min(width.saturating_sub(2)).max(1);
    let rows = wrap_lines(text, room);
    let rows: Vec<&str> = rows.iter().map(|row| row.trim_start()).collect();
    let widest = rows.iter().map(|row| Span::raw(*row).width()).max();
    let left = width.saturating_sub(widest.unwrap_or(0) + 1);
    rows.iter()
        .map(|row| format!("{:left$}{row}", ""))
        .collect()
}

fn chat_lines(chat: &[(&str, Tone)]) -> Vec<ChatLine> {
    chat.iter()
        .map(|&(text, tone)| ChatLine {
            text: text.to_string(),
            tone,
            item: None,
        })
        .collect()
}

pub fn sample_chat(agent: &str) -> Vec<ChatLine> {
    CHAT.iter()
        .map(|&(text, tone)| ChatLine {
            text: text.replace("Engineer", agent),
            tone,
            item: None,
        })
        .collect()
}

/// `+ New session`, then each day's sessions under its header, in list order.
pub fn session_rows(sessions: &[Session]) -> Vec<SidebarRow> {
    let mut rows = vec![row(" + New session", Tone::Accent, RowAction::NewSession)];
    for day in [Day::Today, Day::Yesterday, Day::Older] {
        let mut listed = sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.day == day)
            .peekable();
        if listed.peek().is_none() {
            continue;
        }
        let header = format!(" {}", day.label().to_uppercase());
        rows.extend([
            note("", Tone::Plain),
            SidebarRow {
                text: header.into(),
                ..note("", Tone::Label)
            },
        ]);
        rows.extend(listed.map(|(index, session)| {
            let (mark, tone) = match session.state {
                SessionState::Active => ('●', Tone::Plain),
                SessionState::Idle => ('○', Tone::Plain),
                SessionState::Error => ('!', Tone::Warn),
            };
            SidebarRow {
                text: format!(" {mark} {}", session.name).into(),
                tone,
                action: RowAction::OpenSession(index),
                name: session.name.clone().into(),
                date: day.label(),
            }
        }));
    }
    rows
}

pub struct App {
    pub size: (u16, u16),
    pub keymap: Keymap,
    pub focus: Pane,
    pub mode: Mode,
    pub sessions_wanted: bool,
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
    pub sessions: Vec<Session>,
    pub agents: Vec<Agent>,
    /// Without a source, the sample answers each intent at once.
    intents: Option<Sender<Intent>>,
    pub connection: Connection,
    next_tab: u64,
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
        let mut app = App {
            size: (width, height),
            keymap: Keymap::new(set),
            focus: Pane::Agent,
            mode: Mode::Input,
            sessions_wanted: true,
            view: View::Changes,
            sessions_sel: START_SESSION,
            view_sel: [View::Changes, View::Workflows, View::Agents]
                .map(|view| first_selectable(view_rows(view))),
            notice: None,
            tabs: Vec::new(),
            active: 0,
            diff: None,
            diff_scroll: 0,
            layers: Vec::new(),
            slash: None,
            search: None,
            sessions: crate::sample::sessions(),
            agents: crate::sample::agents(),
            intents: None,
            connection: Connection::Sample,
            next_tab: 0,
            quit: false,
        };
        app.push_tab(Tab::new_session());
        app
    }

    pub fn connected(set: KeySet, width: u16, height: u16, intents: Sender<Intent>) -> App {
        App {
            sessions: Vec::new(),
            agents: Vec::new(),
            sessions_sel: 0,
            intents: Some(intents),
            connection: Connection::Connecting,
            ..App::new(set, width, height)
        }
    }

    /// A tab with no picked agent starts on the default one, once the agents are known.
    fn push_tab(&mut self, mut tab: Tab) -> usize {
        tab.id = self.next_tab;
        self.next_tab += 1;
        if let Some(agent) = self.agents.first().filter(|_| !tab.agent_picked) {
            tab.set_agent(agent);
        }
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    /// A lost source reads no more intents, so an action that needs one keeps its state.
    fn says_lost(&mut self) -> bool {
        let Connection::Lost(why) = &self.connection else {
            return false;
        };
        self.notice = Some(why.clone());
        true
    }

    fn emit(&mut self, intent: Intent) {
        let Some(intents) = &self.intents else {
            let subagent = self.tabs[self.active].parent.is_some();
            for update in answer(&intent, subagent) {
                self.apply(update);
            }
            return;
        };
        if intents.send(intent).is_err() {
            self.connection = Connection::Lost(SOURCE_GONE.to_string());
        }
    }

    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Opened {
                session,
                lines,
                running,
            } => self.opened(&session, lines, running),
            Update::Started { tab, session } => self.started(tab, session),
            Update::StartFailed { tab, message } => self.start_failed(tab, message),
            Update::Item { session, line } => {
                if let Some(index) = self.showing(&session) {
                    self.tabs[index].put(line);
                }
            }
            Update::Delta {
                session,
                item,
                text,
                tone,
            } => self.delta(&session, item, &text, tone),
            Update::Turn { session, turn } => {
                if let Some(index) = self.showing(&session) {
                    self.tabs[index].turn = Some(turn);
                }
            }
            Update::TurnEnded { session, turn } => {
                if let Some(index) = self.showing(&session) {
                    let tab = &mut self.tabs[index];
                    if tab.turn.as_ref() == Some(&turn) {
                        tab.turn = None;
                    }
                }
            }
            Update::SessionState { session, state } => {
                if let Some(listed) = self.sessions.iter_mut().find(|listed| listed.id == session) {
                    listed.state = state;
                }
            }
            Update::Sessions(sessions) => self.set_sessions(sessions),
            Update::Agents(agents) => self.set_agents(agents),
            Update::Connection(connection) => self.connection = connection,
            Update::Notice(notice) => self.notice = Some(notice),
            Update::Begin { agent, prompt } => self.begin(agent, prompt),
        }
    }

    fn delta(&mut self, session: &str, item: String, text: &str, tone: Tone) {
        let Some(index) = self.showing(session) else {
            return;
        };
        let tab = &mut self.tabs[index];
        match tab.item_line(&item) {
            Some(line) => tab.lines[line].text.push_str(text),
            None => tab.put(ChatLine {
                text: text.to_string(),
                tone,
                item: Some(item),
            }),
        }
    }

    fn set_sessions(&mut self, sessions: Vec<Session>) {
        for tab in &mut self.tabs {
            let listed = sessions
                .iter()
                .find(|listed| tab.session.as_ref() == Some(&listed.id));
            if let Some(listed) = listed {
                tab.name = listed.tab.clone();
            }
        }
        let selected = match self
            .session_rows()
            .get(self.sessions_sel)
            .map(|row| row.action)
        {
            Some(RowAction::OpenSession(index)) => Some(self.sessions[index].id.clone()),
            _ => None,
        };
        self.sessions = sessions;
        let rows = self.session_rows();
        let kept = rows.iter().position(|row| match row.action {
            RowAction::OpenSession(index) => selected.as_ref() == Some(&self.sessions[index].id),
            _ => false,
        });
        self.sessions_sel = kept.unwrap_or_else(|| {
            let clamped = self.sessions_sel.min(rows.len() - 1);
            if rows[clamped].selectable() {
                clamped
            } else {
                step_row(&rows, clamped, false)
            }
        });
    }

    /// A first tab with no session and no picked agent is still the starter tab, even with a
    /// draft typed while connecting.
    fn set_agents(&mut self, agents: Vec<Agent>) {
        let first = &mut self.tabs[0];
        if let (Some(agent), None, false) = (agents.first(), &first.session, first.agent_picked) {
            first.set_agent(agent);
        }
        self.agents = agents;
    }

    /// A starter tab the user has already sent from, opened a session in or picked an agent
    /// for keeps what they did.
    fn begin(&mut self, agent: Option<String>, prompt: Option<String>) {
        let first = &self.tabs[0];
        if first.session.is_some() || first.starting || first.agent_picked {
            return;
        }
        if let Some(id) = agent {
            match self.agents.iter().find(|known| known.id == id) {
                Some(known) => {
                    let first = &mut self.tabs[0];
                    first.set_agent(known);
                    first.agent_picked = true;
                }
                None => {
                    let default = &self.tabs[0].agent;
                    self.notice = Some(format!(
                        "agent {id} is not open in this shell; using {default}"
                    ));
                }
            }
        }
        if let Some(text) = prompt {
            let first = &mut self.tabs[0];
            first.starting = true;
            let intent = Intent::Send {
                tab: first.id,
                session: None,
                agent: first.agent_id.clone(),
                text,
            };
            self.emit(intent);
        }
    }

    fn showing(&self, session: &str) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.session.as_deref() == Some(session))
    }

    fn opened(&mut self, session: &str, lines: Vec<ChatLine>, running: &'static [Running]) {
        let Some(index) = self.showing(session) else {
            return;
        };
        let tab = &mut self.tabs[index];
        tab.lines = lines;
        tab.running = running;
        // Opening left an empty tab in input mode; a user who has acted since keeps their mode.
        let untouched = index == self.active
            && self.mode == Mode::Input
            && self.tabs[index].input.is_empty()
            && self.search.is_none()
            && self.slash.is_none()
            && self.layers.is_empty();
        if untouched {
            self.mode = Mode::Normal;
        }
    }

    fn start_failed(&mut self, tab: u64, message: String) {
        if let Some(tab) = self.tabs.iter_mut().find(|shown| shown.id == tab) {
            tab.starting = false;
            self.notice = Some(message);
        }
    }

    /// A tab binds one session; a late `Started` never rebinds it.
    fn started(&mut self, tab: u64, session: String) {
        let waiting = |shown: &&mut Tab| shown.id == tab && shown.session.is_none();
        if let Some(tab) = self.tabs.iter_mut().find(waiting) {
            tab.session = Some(session);
            tab.starting = false;
        }
    }

    pub fn session_rows(&self) -> Vec<SidebarRow> {
        session_rows(&self.sessions)
    }

    pub fn wanted(&self) -> Wanted {
        Wanted {
            sessions: self.sessions_wanted,
            sidebar: self.tabs[self.active].sidebar,
        }
    }

    pub fn chip(&self) -> &'static str {
        match self.layers.last().map(|top| top.kind) {
            Some(LayerKind::Jump) => "JUMP",
            Some(LayerKind::Keys) => "HELP",
            Some(_) => "MENU",
            None if self.search.is_some() => "SEARCH",
            None if self.in_running() => "AGENTS",
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
            None if self.in_running() && self.keymap.set() == KeySet::Vim => KeyCtx::Menu,
            None if self.keymap.set() == KeySet::Vim && self.mode == Mode::Normal => KeyCtx::Normal,
            None => KeyCtx::Input,
        }
    }

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
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

    pub fn in_running(&self) -> bool {
        self.tabs[self.active].running_sel.is_some()
    }

    pub(crate) fn can_enter_running(&self) -> bool {
        let tab = &self.tabs[self.active];
        self.ctx() == KeyCtx::Input
            && self.focus == Pane::Agent
            && self.slash.is_none()
            && tab.input.is_empty()
            && !tab.running.is_empty()
    }

    /// In vim normal mode the rows sit below the conversation: `j` at its end comes down into
    /// them, and `k` on the top row goes back up.
    pub(crate) fn can_fall_into_running(&self) -> bool {
        let tab = &self.tabs[self.active];
        self.ctx() == KeyCtx::Normal
            && self.focus == Pane::Agent
            && tab.scroll == 0
            && !tab.running.is_empty()
    }

    /// Past the last row is the message box, reached only by someone who came from it. Above the
    /// top row is the conversation, which only vim can move to.
    fn step_running(&mut self, forward: bool) {
        let vim = self.keymap.set() == KeySet::Vim;
        let typing = !vim || self.mode == Mode::Input;
        let tab = &mut self.tabs[self.active];
        let (Some(row), max) = (tab.running_sel, tab.running.len().saturating_sub(1)) else {
            return;
        };
        if forward && row == max && typing {
            tab.running_sel = None;
        } else if !forward && row == 0 && vim {
            tab.running_sel = None;
            self.mode = Mode::Normal;
        } else {
            tab.running_sel = Some(stepped(row, forward, max));
        }
    }

    pub(crate) fn activate_running(&mut self) {
        let tab = &mut self.tabs[self.active];
        let running = tab.running;
        let Some(row) = tab.running_sel.and_then(|row| running.get(row)) else {
            return;
        };
        match row.opens {
            Opens::Agent(chat) => {
                let mut child = Tab::new_session();
                child.id = tab.id;
                child.session = tab.session.clone();
                child.name = row.job.to_string();
                child.agent = row.name.to_string();
                child.agent_id = row.name.to_string();
                child.lines = chat_lines(chat);
                child.sidebar = tab.sidebar;
                let parent = std::mem::replace(tab, child);
                tab.parent = Some(Box::new(parent));
                self.activate_tab(self.active);
            }
            Opens::Workflow(row) => self.show_workflow(row),
        }
    }

    fn show_workflow(&mut self, row: usize) {
        self.tabs[self.active].running_sel = None;
        self.run_effect(Effect::ShowSidebar(View::Workflows));
        self.view_sel[View::Workflows as usize] = row;
        if sidebar_can_dock(self.size.0) {
            self.focus_pane(Pane::Sidebar);
        }
    }

    /// Focus stays put, so reading the diff does not cost the place in the conversation.
    pub fn show_file(&mut self, file: usize) {
        self.run_effect(Effect::ShowSidebar(View::Changes));
        self.diff = Some(file);
        self.diff_scroll = 0;
        let rows = view_rows(View::Changes);
        if let Some(row) = rows
            .iter()
            .position(|row| row.action == RowAction::OpenDiff(file))
        {
            self.view_sel[View::Changes as usize] = row;
        }
    }

    pub(crate) fn step_out(&mut self) {
        let tab = &mut self.tabs[self.active];
        if let Some(parent) = tab.parent.take() {
            *tab = *parent;
        }
        self.activate_tab(self.active);
    }

    pub fn conversation_rect(&self, pane: Rect) -> Rect {
        let rows = self.tabs[self.active].running.len() as u16;
        conversation_area(Rect {
            height: pane.height - rows,
            ..pane
        })
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Esc and the next key in one read reach crossterm as alt-<key>
        // (crossterm-0.29.0/src/event/sys/unix/parse.rs:34-42). In a line, `esc b` and `esc f`
        // are also what terminals send for option-left and option-right, so they move by word.
        let word_move = key.modifiers == KeyModifiers::ALT
            && matches!(key.code, KeyCode::Char('b' | 'f'))
            && in_line(self.ctx());
        if self.keymap.set() == KeySet::Vim
            && key.modifiers.contains(KeyModifiers::ALT)
            && !word_move
        {
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
            | Action::DeleteToStart
            | Action::CursorLeft
            | Action::CursorRight
            | Action::WordLeft
            | Action::WordRight
            | Action::Home
            | Action::End => self.edit(action),
            Action::Submit if self.ctx() == KeyCtx::SearchLine => self.submit_search(),
            Action::Submit if self.in_running() => self.activate_running(),
            Action::Next if self.in_running() => self.step_running(true),
            Action::Prev if self.in_running() => self.step_running(false),
            Action::Next if self.can_fall_into_running() => {
                self.tabs[self.active].running_sel = Some(0);
            }
            Action::Prev if self.can_enter_running() => {
                let tab = &mut self.tabs[self.active];
                tab.running_sel = Some(tab.running.len() - 1);
            }
            Action::Submit if self.slash.is_some() => self.run_slash(),
            Action::Next if self.slash.is_some() => self.step_slash(true),
            Action::Prev if self.slash.is_some() => self.step_slash(false),
            Action::Complete => self.complete_slash(),
            Action::Submit => self.submit(),
            Action::Back => self.back(),
            Action::Interrupt => self.interrupt(),
            Action::EnterInput => {
                self.search = None;
                self.tabs[self.active].running_sel = None;
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
            Action::Palette if self.slash.is_some() => self.step_slash(false),
            Action::Palette => self.push_layer(LayerKind::Jump),
            Action::OpenSearch => self.open_search(),
            Action::SearchNext if self.search.is_some() => self.step_match(true),
            Action::SearchPrev if self.search.is_some() => self.step_match(false),
            Action::NextTab => self.step_tab(1),
            Action::PrevTab => self.step_tab(self.tabs.len() - 1),
            Action::ClosePane => self.close_pane(),
            Action::ToggleSessions => self.toggle_sessions(),
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::NextView => self.next_view_if_shown(),
            Action::PageUp if self.focus == Pane::Agent => self.scroll_agent(true, true),
            Action::PageDown if self.focus == Pane::Agent => self.scroll_agent(false, true),
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
            self.tabs[self.active].running_sel = None;
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
            Action::Insert(_)
                | Action::Backspace
                | Action::Delete
                | Action::DeleteWord
                | Action::DeleteToStart
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
                    self.tabs[self.active].sidebar = true;
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
        } else if self.slash.take().is_some() || self.tabs[self.active].running_sel.take().is_some()
        {
        } else if self.search.is_some() {
            self.back_from_search();
        } else if self.diff.is_some() && self.focus == Pane::Sidebar {
            self.diff = None;
        } else if self.tabs[self.active].parent.is_some()
            && (self.mode == Mode::Normal || self.keymap.set() == KeySet::Emacs)
        {
            self.step_out();
        } else {
            self.mode = Mode::Normal;
        }
    }

    fn send(&mut self) {
        if self.tabs[self.active].input.text().trim().is_empty() || self.says_lost() {
            return;
        }
        if self.agents.is_empty() {
            self.notice = Some(CONNECTING.to_string());
            return;
        }
        let tab = &mut self.tabs[self.active];
        if tab.starting {
            self.notice = Some(STARTING.to_string());
            return;
        }
        let intent = Intent::Send {
            tab: tab.id,
            session: tab.session.clone(),
            agent: tab.agent_id.clone(),
            text: tab.input.text().to_string(),
        };
        tab.starting = tab.session.is_none();
        tab.input.clear();
        tab.scroll = 0;
        self.emit(intent);
    }

    fn interrupt(&mut self) {
        let searching = self.ctx() == KeyCtx::SearchLine;
        let input = match (self.layers.last_mut(), self.search.as_mut()) {
            (Some(top), _) => &mut top.filter,
            (None, Some(search)) if searching => &mut search.query,
            _ => &mut self.tabs[self.active].input,
        };
        if !input.is_empty() {
            input.clear();
            if self.layers.is_empty() {
                self.slash = None;
            }
            if searching {
                self.go_to_match();
            }
            return;
        }
        let tab = &self.tabs[self.active];
        if let (Some(session), Some(turn)) = (&tab.session, &tab.turn) {
            let intent = Intent::Interrupt {
                session: session.clone(),
                agent: tab.agent_id.clone(),
                turn: turn.clone(),
            };
            if self.says_lost() {
                return;
            }
            self.emit(intent);
            self.notice = Some(STOPPING.to_string());
        } else {
            self.notice = Some(self.quit_hint().to_string());
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
        match layout(self.size.0, self.size.1, self.wanted()) {
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
        self.tabs[self.active].running_sel = None;
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
            (Pane::Agent, Dir::Down) if !self.tabs[self.active].running.is_empty() => {
                self.tabs[self.active].running_sel = Some(0);
            }
            (Pane::Agent, Dir::Up) => self.tabs[self.active].running_sel = None,
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
        if index != self.active {
            self.diff = None;
        }
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

    pub fn open_tab_with_agent(&mut self, agent: usize) {
        self.layers.clear();
        let mut tab = Tab::new_session();
        tab.set_agent(&self.agents[agent]);
        tab.agent_picked = true;
        let index = self.push_tab(tab);
        self.activate_tab(index);
    }

    pub fn close_tab(&mut self) {
        self.diff = None;
        self.tabs.remove(self.active);
        if self.tabs.is_empty() {
            self.push_tab(Tab::new_session());
        }
        self.activate_tab(self.active.saturating_sub(1));
    }

    fn close_pane(&mut self) {
        match self.focus {
            Pane::Agent => self.close_tab(),
            Pane::Sessions => self.sessions_wanted = false,
            Pane::Sidebar => self.tabs[self.active].sidebar = false,
        }
        self.recheck_focus();
    }

    /// The untouched starter tab may become the session's. Any other keeps its draft and its
    /// picked agent, because a tab cannot switch agent.
    pub fn open_session(&mut self, row: usize) {
        let RowAction::OpenSession(index) = self.session_rows()[row].action else {
            return;
        };
        self.layers.clear();
        let session = self.sessions[index].clone();
        let shown = self
            .tabs
            .iter()
            .position(|tab| tab.session.as_ref() == Some(&session.id));
        if let Some(index) = shown {
            self.activate_tab(index);
            return;
        }
        if self.says_lost() {
            return;
        }
        let current = &self.tabs[self.active];
        let untouched = current.session.is_none()
            && !current.starting
            && current.lines.is_empty()
            && current.input.is_empty()
            && !current.agent_picked;
        if !untouched {
            self.active = self.push_tab(Tab::new_session());
            self.diff = None;
        }
        let agent = self.agent_name(&session.agent);
        let tab = &mut self.tabs[self.active];
        tab.session = Some(session.id.clone());
        tab.name = session.tab;
        tab.agent = agent;
        tab.agent_id = session.agent.clone();
        self.activate_tab(self.active);
        self.emit(Intent::Open {
            session: session.id,
            agent: session.agent,
        });
    }

    /// An agent the list does not hold shows by its id.
    fn agent_name(&self, id: &str) -> String {
        let found = self.agents.iter().find(|agent| agent.id == id);
        found.map_or(id, |agent| &agent.name).to_string()
    }

    fn toggle_sessions(&mut self) {
        if sessions_can_dock(self.size.0) {
            self.sessions_wanted = !self.sessions_wanted;
            self.recheck_focus();
        } else {
            self.toggle_layer(LayerKind::Sessions);
        }
    }

    fn toggle_sidebar(&mut self) {
        if sidebar_can_dock(self.size.0) {
            let tab = &mut self.tabs[self.active];
            tab.sidebar = !tab.sidebar;
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
            KeySet::Vim => "no right sidebar on screen: ] opens it",
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
            Pane::Agent => self.scroll_agent(!forward, false),
            Pane::Sessions => self.step_session(forward),
            Pane::Sidebar => self.step_sidebar(forward),
        }
    }

    fn step_session(&mut self, forward: bool) {
        self.sessions_sel = step_row(&self.session_rows(), self.sessions_sel, forward);
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

    /// One row, or with `page` a screenful.
    fn scroll_agent(&mut self, up: bool, page: bool) {
        let Some(panes) = self.panes() else {
            return;
        };
        let area = self.visible_rows(Pane::Agent, self.conversation_rect(panes.agent));
        let tab = &mut self.tabs[self.active];
        let lines = tab.wrapped_rows(area.width).len();
        let max = lines.saturating_sub(usize::from(area.height));
        let by = if page { usize::from(area.height) } else { 1 };
        let scroll = tab.scroll.min(max);
        tab.scroll = if up {
            (scroll + by).min(max)
        } else {
            scroll.saturating_sub(by)
        };
    }

    fn activate_session_row(&mut self) {
        match self.session_rows()[self.sessions_sel].action {
            RowAction::NewSession => self.push_layer(LayerKind::Picker),
            RowAction::OpenSession(_) => self.open_session(self.sessions_sel),
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
    use crate::sample::{RUNNING, SUBAGENT_MARK};
    use crate::testing::{
        buffer, find, keys, numbered, press, screen, session_row, type_text, with_right_pane,
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
        assert!(app.session_rows()[app.sessions_sel]
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
            let mut app = with_right_pane(App::new(set, 132, 36));
            type_text(&mut app, "hi");
            press(&mut app, "enter");
            let s = screen(&app);
            assert_eq!(find(&s, "hi"), Some((4, 93)));
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
        assert_eq!(app.tabs[0].lines[0].text, " hi ");
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
    fn wrap_prefers_spaces() {
        assert_eq!(wrap("alpha beta gamma", 10), ["alpha beta", "gamma"]);
        assert_eq!(wrap("alpha beta", 7), ["alpha", "beta"]);
    }

    #[test]
    fn wrap_breaks_a_word_only_when_it_must() {
        assert_eq!(wrap("aaaaaaaaaaaa", 5), ["aaaaa", "aaaaa", "aa"]);
        assert_eq!(wrap("ab cccccccc", 5), ["ab", "ccccc", "ccc"]);
        assert_eq!(wrap("世界世界", 3), ["世", "界", "世", "界"]);
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
    fn a_tab_with_nothing_running_has_no_rows() {
        let s = screen(&App::new(Vim, 132, 36));
        assert_eq!(find(&s, "◆"), None);
        assert_eq!(find(&s, "select"), None);
    }

    #[test]
    fn the_conversation_gives_up_a_row_for_each() {
        let mut app = session_app();
        app.tabs[0].lines = numbered(100);
        assert_eq!(find(&screen(&app), "message 99").map(|at| at.0), Some(26));
    }

    #[test]
    fn up_from_an_empty_box_selects_the_last_row() {
        let mut app = session_app();
        press(&mut app, "i up");
        assert_eq!(app.tabs[0].running_sel, Some(3));
        assert_eq!(app.chip(), "AGENTS");
        let s = screen(&app);
        assert!(find(&s, "enter open · j/k move · esc back").is_some());
    }

    #[test]
    fn up_with_text_in_the_box_does_not() {
        let mut app = session_app();
        press(&mut app, "i");
        type_text(&mut app, "x");
        press(&mut app, "up");
        assert_eq!(app.tabs[0].running_sel, None);
    }

    #[test]
    fn j_falls_from_the_end_of_the_conversation() {
        let mut app = session_app();
        press(&mut app, "j");
        assert_eq!(app.tabs[0].running_sel, Some(0));
    }

    #[test]
    fn j_scrolls_first_when_scrolled_up() {
        let mut app = session_app();
        app.tabs[0].lines = numbered(100);
        press(&mut app, "k k j");
        assert_eq!((app.tabs[0].scroll, app.tabs[0].running_sel), (1, None));
        press(&mut app, "j");
        assert_eq!((app.tabs[0].scroll, app.tabs[0].running_sel), (0, None));
        press(&mut app, "j");
        assert_eq!(app.tabs[0].running_sel, Some(0));
    }

    #[test]
    fn j_stops_on_the_last_row() {
        let mut app = session_app();
        press(&mut app, "j j j j j j j");
        assert_eq!(app.tabs[0].running_sel, Some(3));
    }

    #[test]
    fn k_climbs_out_of_the_top_row() {
        let mut app = session_app();
        press(&mut app, "j k");
        assert_eq!(app.tabs[0].running_sel, None);
        assert_eq!(app.ctx(), KeyCtx::Normal);
    }

    #[test]
    fn up_past_the_top_lands_in_normal_mode() {
        let mut app = session_app();
        press(&mut app, "i up up up up up");
        assert_eq!(app.tabs[0].running_sel, None);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn down_past_the_last_row_returns_to_the_box() {
        let mut app = session_app();
        press(&mut app, "i up down");
        assert_eq!(app.tabs[0].running_sel, None);
        assert_eq!(app.ctx(), KeyCtx::Input);
    }

    #[test]
    fn ctrl_w_j_and_k_enter_and_leave_the_rows() {
        let mut app = session_app();
        press(&mut app, "ctrl-w j");
        assert_eq!(app.tabs[0].running_sel, Some(0));
        press(&mut app, "ctrl-w k");
        assert_eq!(app.tabs[0].running_sel, None);
    }

    #[test]
    fn leaving_the_pane_leaves_the_rows() {
        let mut app = session_app();
        press(&mut app, "j ctrl-w h");
        assert_eq!(app.focus, Pane::Sessions);
        assert_eq!(app.tabs[0].running_sel, None);
    }

    #[test]
    fn enter_steps_in_and_esc_restores_the_parent() {
        let mut app = session_app();
        let before = app.tabs[0].clone();
        press(&mut app, "j");
        let selected = app.tabs[0].clone();
        press(&mut app, "enter");
        let s = screen(&app);
        assert_eq!(app.tabs[0].agent, "Reviewer");
        assert!(find(&s, "packs · Engineer › Reviewer").is_some());
        assert!(find(&s, "Message Reviewer").is_some());
        assert!(find(&s, "◆ Checking the new subcommands").is_some());
        assert_eq!(app.ctx(), KeyCtx::Normal);
        assert!(find(&s, "esc step out").is_some());
        press(&mut app, "esc");
        assert_eq!(app.tabs[0], selected);
        press(&mut app, "esc");
        assert_eq!(app.tabs[0], before);
    }

    #[test]
    fn a_subagents_reply_carries_the_mark() {
        let mut app = session_app();
        press(&mut app, "j enter i");
        type_text(&mut app, "hi");
        press(&mut app, "enter");
        let lines = &app.tabs[0].lines;
        assert!(lines[lines.len() - 2].text.starts_with(SUBAGENT_MARK));
        let (row, column) = find(&screen(&app), "◆ sample reply").expect("the reply");
        assert_eq!(buffer(&app)[(column, row)].fg, theme::ACCENT);
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
        assert_eq!(app.view_sel[View::Workflows as usize], 2);
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
        let s = screen(&app);
        assert!(find(&s, "3 files changed   +58 -12").is_some());
        let lines = &app.tabs[0].lines;
        for file in 0..3 {
            let line = lines
                .iter()
                .find(|line| line.tone == Tone::File(file))
                .expect("a row per file");
            let row = view_rows(View::Changes)
                .iter()
                .find(|row| row.action == RowAction::OpenDiff(file))
                .expect("the same file in the Changes view");
            assert_eq!(line.text.trim(), row.text.trim());
            assert!(find(&s, line.text.trim()).is_some(), "{}", line.text);
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
        assert_eq!(app.view, View::Changes);
        assert_eq!(app.diff, Some(1));
        assert_eq!(app.focus, Pane::Agent);
        let selected = app.view_sel[View::Changes as usize];
        assert_eq!(
            view_rows(View::Changes)[selected].action,
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
        press(&mut app, "enter");
        assert_eq!(app.tabs[0].agent, "Reviewer");
        press(&mut app, "ctrl-g");
        assert_eq!(app.tabs[0].agent, "Engineer");
        press(&mut app, "ctrl-g");
        assert_eq!(app.tabs[0].running_sel, None);
        press(&mut app, "up ctrl-n");
        assert_eq!(
            app.tabs[0].running_sel, None,
            "down past the end is the box"
        );
    }

    #[test]
    fn stepping_in_clears_a_search() {
        let mut app = session_app();
        press(&mut app, "/");
        type_text(&mut app, "Moving");
        press(&mut app, "enter ctrl-w j enter");
        assert!(app.search.is_none());
        assert_eq!(app.tabs[0].agent, "Reviewer");
    }

    #[test]
    fn clear_inside_a_subagent_keeps_the_parent() {
        let mut app = session_app();
        let lines = app.tabs[0].lines.clone();
        press(&mut app, "j enter");
        app.run_effect(Effect::ClearTab);
        assert!(app.tabs[0].lines.is_empty());
        press(&mut app, "esc esc");
        assert_eq!(app.tabs[0].agent, "Engineer");
        assert_eq!(app.tabs[0].lines, lines);
    }

    #[test]
    fn closing_the_tab_inside_a_subagent_closes_the_tab() {
        let mut app = session_app();
        press(&mut app, "j enter ctrl-w q");
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].parent, None);
        assert_eq!(app.tabs[0].agent, "Engineer");
        assert!(app.tabs[0].lines.is_empty());
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
    fn a_subagent_starts_with_its_parents_right_pane() {
        let mut app = session_app();
        press(&mut app, "j enter");
        assert!(!right_is_shown(&app));
        press(&mut app, "esc esc ] j enter");
        assert_eq!(app.tabs[0].agent, "Reviewer");
        assert!(right_is_shown(&app));
        press(&mut app, "] esc");
        assert_eq!(app.tabs[0].agent, "Engineer");
        assert!(right_is_shown(&app), "stepping out restores the parent's");
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
    fn slash_changes_and_workflows_open_the_pane() {
        for (name, view) in [("changes", View::Changes), ("workflows", View::Workflows)] {
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
    fn the_bar_finds_changes_by_diff() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "diff");
        press(&mut app, "enter");
        assert!(right_is_shown(&app));
        assert_eq!(app.view, View::Changes);
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
        let before = app.view_sel[View::Changes as usize];
        press(&mut app, "ctrl-n ctrl-p");
        assert_eq!(app.tabs[0].running_sel, None);
        assert_eq!(app.view_sel[View::Changes as usize], before);
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
    fn clear_on_a_session_tab_leaves_the_rows_clean() {
        for (width, height) in [(132, 36), (60, 16)] {
            let mut app = App::new(Vim, width, height);
            app.open_session(session_row("pack registry"));
            app.run_effect(Effect::ClearTab);
            let s = screen(&app);
            assert_eq!(find(&s, "start typing"), None, "{width}");
            let (row, _) = find(&s, "◆ Reviewer · auth").expect("the row");
            assert!(!s[row as usize].contains("Pick"), "{}", s[row as usize]);
        }
    }

    #[test]
    fn page_keys_scroll_the_conversation_past_the_rows() {
        for set in [Vim, Emacs] {
            let mut app = App::new(set, 132, 36);
            enter_on_session(&mut app, "pack registry");
            app.tabs[0].lines = numbered(100);
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

    #[test]
    fn wrap_drops_every_space_at_a_break() {
        assert_eq!(
            wrap("abcdefghij  klmnopqrst", 10),
            ["abcdefghij", "klmnopqrst"]
        );
        assert_eq!(wrap("one  two", 4), ["one", "two"]);
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
        let mut app = with_right_pane(App::new(Vim, 132, 36));
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
        let files = app.view_sel[View::Changes as usize];
        press(&mut app, "j");
        assert_eq!(app.view_sel[View::Changes as usize], files + 1);
        press(&mut app, "k");
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
        assert_eq!(find(&s, "Changes"), None);
        assert!(find(&s, "enter send").map(|(_, column)| column) > edge);

        press(&mut app, "ctrl-w ]");
        assert!(app.wanted().sidebar);
        assert!(find(&screen(&app), "Changes").is_some());

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
    fn view_cycles_and_opens_on_changes() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        assert_eq!(app.view, View::Changes);
        for view in [View::Workflows, View::Agents, View::Changes] {
            press(&mut app, "esc ctrl-w }");
            assert_eq!(app.view, view);
        }
        let mut app = with_right_pane(App::new(Emacs, 132, 36));
        press(&mut app, "ctrl-x }");
        assert_eq!(app.view, View::Workflows);
        assert!(find(&screen(&app), "2 running").is_some());
    }

    #[test]
    fn file_opens_its_diff_and_back_returns() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
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

        let mut app = with_right_pane(App::new(Emacs, 132, 36));
        press(&mut app, "ctrl-x o enter");
        let s = screen(&app);
        assert!(find(&s, "ctrl-g back to files").is_some());
        assert_eq!(find(&s, "esc back"), None);
        press(&mut app, "ctrl-g");
        assert!(find(&screen(&app), "3 files").is_some());
    }

    #[test]
    fn changing_the_view_leaves_the_diff() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w l ctrl-w l enter ctrl-w }");
        assert_eq!(app.diff, None);
        assert!(find(&screen(&app), "2 running").is_some());
    }

    #[test]
    fn a_tall_diff_scrolls_to_its_last_row_at_the_smallest_height() {
        let mut app = with_right_pane(App::new(Vim, 132, 16));
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
        let mut app = with_right_pane(App::new(Vim, 132, 16));
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
        assert_eq!(app.slash, Some(9));
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
        assert_eq!(app.tabs[0].lines[0].text, "/cx");
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
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        type_text(&mut app, "/");
        assert_eq!(app.slash_matches().len(), 10);
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
        let hint = "no right sidebar on screen: ] opens it";
        for width in [104, 70] {
            let mut app = with_right_pane(App::new(Vim, width, 30));
            press(&mut app, "esc ctrl-w }");
            assert_eq!(app.view, View::Changes);
            assert_eq!(app.notice.as_deref(), Some(hint));
        }

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w l ctrl-w l enter ctrl-w ] ctrl-w }");
        assert_eq!(app.view, View::Changes);
        assert_eq!(app.diff, Some(0));
        assert_eq!(app.notice.as_deref(), Some(hint));

        let mut app = with_right_pane(App::new(Emacs, 70, 24));
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
        let sent = lines.len() - 4;
        assert_eq!(lines[sent].text, "hi");
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

    #[test]
    fn sample_rows_match_the_old_sidebar() {
        let rows = session_rows(&crate::sample::sessions());
        assert_eq!(
            texts(&rows),
            [
                " + New session",
                "",
                " TODAY",
                " ● cli ux redesign",
                " ○ flaky recovery test",
                " ! pack registry login",
                "",
                " YESTERDAY",
                " ○ eval baseline",
                " ○ desktop plugin page",
                " ○ weekly triage",
                " ○ pack review notes",
            ]
        );
        assert_eq!(rows[START_SESSION].text, " ● cli ux redesign");
    }

    fn connected() -> (App, std::sync::mpsc::Receiver<Intent>) {
        let (intents, receiver) = std::sync::mpsc::channel();
        let mut app = App::connected(Vim, 132, 36, intents);
        app.agents = crate::sample::agents();
        (app, receiver)
    }

    fn said(text: &str, tone: Tone, item: &str) -> ChatLine {
        ChatLine {
            text: text.to_string(),
            tone,
            item: Some(item.to_string()),
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
            lines: sample_chat("Reviewer"),
            running: &[],
        });
        assert_eq!(app.tabs[0].lines, sample_chat("Reviewer"));
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
                lines: sample_chat("Engineer"),
                running: RUNNING,
            },
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
        let hint = "esc first, then :q then enter to quit";
        assert_eq!(app.notice.as_deref(), Some(hint));
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
        let (intents, receiver) = std::sync::mpsc::channel();
        let mut app = App::connected(Vim, 132, 36, intents);
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
        let (intents, _receiver) = std::sync::mpsc::channel();
        let mut app = App::connected(Vim, 132, 36, intents);
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
            lines: sample_chat("Engineer"),
            running: &[],
        }
    }

    #[test]
    fn opened_leaves_a_draft_typed_while_it_loaded() {
        let (mut app, _intents) = connected();
        app.sessions = crate::sample::sessions();
        enter_on_session(&mut app, "flaky");
        type_text(&mut app, "dr");
        app.apply(opened("flaky recovery test"));
        assert_eq!(app.tabs[0].lines, sample_chat("Engineer"));
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
        assert_eq!(app.tabs[0].lines, sample_chat("Engineer"));
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

        let (intents, _receiver) = std::sync::mpsc::channel();
        let mut app = App::connected(Vim, 132, 36, intents);
        app.apply(lost());
        type_text(&mut app, "hello");
        press(&mut app, "enter");
        assert_eq!(app.notice.as_deref(), Some(why), "before the agents arrive");
        assert_eq!(app.tabs[0].input.text(), "hello");
    }

    #[test]
    fn prompt_keys_are_not_live() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        enter_on_session(&mut app, "flaky");
        for key in ["y", "n", "a"] {
            press(&mut app, key);
            assert_eq!(app.notice.as_deref(), Some("not wired yet"), "{key}");
            press(&mut app, "x");
            assert_eq!(app.notice, None);
        }

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

        for view in [View::Workflows, View::Agents] {
            let mut app = with_right_pane(App::new(Vim, 132, 36));
            app.view = view;
            press(&mut app, "esc ctrl-w l ctrl-w l enter");
            assert_eq!(app.notice.as_deref(), Some("not wired yet"));
        }
    }
}
