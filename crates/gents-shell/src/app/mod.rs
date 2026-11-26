pub mod changes;
pub mod fields;
pub mod form;
pub mod input;
pub mod layers;
mod mouse;
mod prompt;
pub mod search;
pub(crate) mod selection;
pub mod setup;
pub mod transcript;
pub mod work;

use std::sync::mpsc::Sender;

use rapidhash::{HashMapExt, RapidHashMap};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;

use crate::commands::{Effect, COMMANDS};
use crate::draw::agent::{slash_area, slash_rows};
use crate::filter::subsequence;
use crate::keys::{in_line, Action, Dir, KeyCtx, KeySet, Keymap};
use crate::layout::{
    conversation_area, layout, sessions_can_dock, sidebar_can_dock, sidebar_rows_area, Panes,
    Right, Screen, Wanted,
};
use crate::sample::{
    answer, note, row, MenuId, Opens, RowAction, Scope, SidebarRow, Tone, View, START_SESSION,
};
use crate::source::{
    Agent, BackgroundCall, Change, Connection, Day, Intent, Job, MenuRow, Session, SessionState,
    Skill, Update,
};
use fields::Field;
use form::FormState;
use layers::{Layer, LayerKind};
use search::Search;
use setup::Setup;
use transcript::{ChatLine, Tab};

pub(crate) const NOT_WIRED: &str = "not wired yet";
const SOURCE_GONE: &str = "the runtime source stopped: reopen the shell";
const CONNECTING: &str = "still connecting to the runtime";
const STOPPING: &str = "stopping the turn";
const NOTHING_RUNNING: &str = "nothing is running";
const CTRL_C_AGAIN: &str = "ctrl-c again to quit";
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

/// What a row of the slash list runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashRun {
    Effect(Effect),
    Skill(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashRow {
    pub name: String,
    pub about: String,
    pub run: SlashRun,
}

/// What `send_as` sends the box's text as.
enum Sending {
    Text,
    Skill(String),
    Goal,
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
    pub scope: Scope,
    pub sessions_sel: usize,
    pub view_sel: [usize; 4],
    pub notice: Option<String>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// The subagent the Subagent view shows, from the active tab's running rows. Held only
    /// while that view is the sidebar's.
    pub subagent: Option<Tab>,
    /// Whether the right pane was wanted, and its view, as the shown subagent opened, for esc
    /// to put back.
    before_subagent: (bool, View),
    pub diff: Option<usize>,
    pub diff_scroll: usize,
    pub layers: Vec<Layer>,
    /// The selected row of the open slash list, as a position in `slash_matches`.
    pub slash: Option<usize>,
    pub search: Option<Search>,
    pub sessions: Vec<Session>,
    pub agents: Vec<Agent>,
    /// The agent they are for, and its skills.
    pub skills: (String, Vec<Skill>),
    pub jobs: Vec<Job>,
    pub jobs_as_of: String,
    /// The address of the job whose detail the Workflows view shows.
    pub job_open: Option<String>,
    pub background: Vec<BackgroundCall>,
    pub background_as_of: String,
    /// Every tab's tools share one root, so its working tree is the app's. `None` until read.
    pub working_tree: Option<Result<Vec<Change>, String>>,
    /// Each menu's rows as last read; a menu not read yet has none.
    pub menus: RapidHashMap<MenuId, Result<Vec<MenuRow>, String>>,
    /// The menu `MenuId::More` acts on, and the row it was opened for as its title.
    more: Option<(MenuId, String)>,
    /// The title of the question `MenuId::Ask` shows.
    asked: String,
    /// The agent `MenuId::Profiles` lists for.
    profiles_of: Option<String>,
    /// The Setup layer's state, from when it last opened.
    pub setup: Option<Setup>,
    /// The Form layer's state, from when it last opened.
    pub form: Option<FormState>,
    /// The number the last form opening took.
    openings: u64,
    /// The openings whose save is out, so its answer shows once its form is closed.
    saving: Vec<u64>,
    /// Without a source, the sample answers each intent at once.
    intents: Option<Sender<Intent>>,
    pub connection: Connection,
    pub branch: Option<String>,
    /// The conversation text a left drag covers; kept after the release until the next press.
    pub(crate) selection: Option<selection::Selection>,
    /// Text the terminal copies after this input.
    clipboard: Option<String>,
    next_tab: u64,
    quit: bool,
}

/// The box's first word after `/`, and the rest trimmed.
fn slash_words(text: &str) -> (&str, &str) {
    let text = text.strip_prefix('/').unwrap_or(text);
    let (name, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    (name, rest.trim())
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

/// How far a scroll moves: a row, a screenful, or to the first or last row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum By {
    Row,
    Page,
    End,
}

/// `scroll`, counted up from the bottom of `total` rows, moved `by` within `height`, and kept
/// where the top row still fills the screen.
fn scrolled(scroll: usize, up: bool, by: By, total: usize, height: u16) -> usize {
    let height = usize::from(height);
    let max = total.saturating_sub(height);
    let by = match by {
        By::Row => 1,
        By::Page => height,
        By::End => max,
    };
    let scroll = scroll.min(max);
    if up {
        (scroll + by).min(max)
    } else {
        scroll.saturating_sub(by)
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
            view: View::Diff,
            scope: Scope::Session,
            sessions_sel: START_SESSION,
            view_sel: [0; 4],
            notice: None,
            tabs: Vec::new(),
            active: 0,
            subagent: None,
            before_subagent: (false, View::Diff),
            diff: None,
            diff_scroll: 0,
            layers: Vec::new(),
            slash: None,
            search: None,
            sessions: crate::sample::sessions(),
            agents: crate::sample::agents(),
            skills: (String::new(), Vec::new()),
            jobs: crate::sample::jobs(),
            jobs_as_of: String::new(),
            job_open: None,
            background: crate::sample::background(),
            background_as_of: String::new(),
            working_tree: None,
            menus: RapidHashMap::new(),
            more: None,
            asked: String::new(),
            profiles_of: None,
            setup: None,
            form: None,
            openings: 0,
            saving: Vec::new(),
            intents: None,
            connection: Connection::Sample,
            branch: None,
            selection: None,
            clipboard: None,
            next_tab: 0,
            quit: false,
        };
        app.push_tab(Tab::new_session());
        app.start_view_rows();
        app
    }

    pub fn connected(set: KeySet, width: u16, height: u16, intents: Sender<Intent>) -> App {
        let mut app = App {
            sessions: Vec::new(),
            agents: Vec::new(),
            jobs: Vec::new(),
            background: Vec::new(),
            sessions_sel: 0,
            intents: Some(intents),
            connection: Connection::Connecting,
            ..App::new(set, width, height)
        };
        app.start_view_rows();
        app
    }

    fn start_view_rows(&mut self) {
        self.view_sel = [View::Diff, View::Workflows, View::Agents, View::Subagent]
            .map(|view| first_selectable(&self.view_rows(view)));
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
            for update in answer(&intent) {
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
            Update::Opened { session, lines } => {
                let file = self.selected_file();
                self.opened(&session, lines);
                self.keep_file_selected(file);
            }
            Update::Running { session, rows } => {
                if let Some(tab) = self.showing(&session) {
                    tab.set_running(rows);
                }
            }
            Update::Started { tab, session } => self.started(tab, session),
            Update::StartFailed { tab, message } => self.start_failed(tab, message),
            Update::Item { session, line } => {
                let file = self.selected_file();
                if let Some(tab) = self.showing(&session) {
                    tab.put(line);
                }
                self.keep_file_selected(file);
            }
            Update::Delta {
                session,
                item,
                text,
                tone,
            } => self.delta(&session, item, &text, tone),
            Update::Turn { session, turn } => {
                if let Some(tab) = self.showing(&session) {
                    tab.turn = Some(turn);
                }
            }
            Update::TurnEnded { session, turn } => {
                if let Some(tab) = self.showing(&session) {
                    if tab.turn.as_ref() == Some(&turn) {
                        tab.turn = None;
                        tab.following = false;
                    }
                }
                // Any turn can change the folder. A hidden pane reads it as it is shown again.
                if self.right_pane_shown() {
                    self.read_tree();
                }
            }
            Update::SessionState { session, state } => {
                if let Some(listed) = self.sessions.iter_mut().find(|listed| listed.id == session) {
                    listed.state = state;
                }
            }
            Update::Sessions(sessions) => self.set_sessions(sessions),
            Update::Agents(agents) => self.set_agents(agents),
            Update::Skills { agent, skills } => self.skills = (agent, skills),
            Update::Connection(connection) => {
                // A shell that opens with no agent to message lands on what connects one.
                let opens_setup = self.connection == Connection::Connecting
                    && matches!(connection, Connection::NoAgent(_))
                    && self.layers.is_empty();
                self.connection = connection;
                self.setup_says_lost();
                self.form_says_lost();
                if opens_setup {
                    self.push_layer(LayerKind::Setup);
                }
            }
            Update::Branch(branch) => self.branch = Some(branch),
            Update::Notice(notice) => self.notice = Some(notice),
            Update::Prompts { session, prompts } => {
                if let Some(tab) = self.showing(&session) {
                    tab.prompts = prompts;
                }
            }
            Update::Jobs { jobs, as_of } => self.set_jobs(jobs, as_of),
            Update::Background { calls, as_of } => self.set_background(calls, as_of),
            Update::Begin { agent, prompt } => self.begin(agent, prompt),
            Update::Continue { session, agent } => self.continue_session(session, agent),
            Update::WorkingTree(tree) => self.set_working_tree(tree),
            Update::Menu { menu, rows } => self.set_menu(menu, rows),
            Update::Ask { title, rows } => self.ask(title, rows),
            update @ (Update::Setup(_)
            | Update::SignInUrl { .. }
            | Update::SignedIn(_)
            | Update::Models { .. }
            | Update::Recommended { .. }
            | Update::Saved(_)) => self.apply_setup(update),
            update @ (Update::Form { .. } | Update::FormSaved { .. } | Update::FormNote { .. }) => {
                self.apply_form(update)
            }
        }
    }

    fn delta(&mut self, session: &str, item: String, text: &str, tone: Tone) {
        let Some(tab) = self.showing(session) else {
            return;
        };
        match tab.item_line(&item) {
            Some(line) => tab.lines[line].text.push_str(text),
            None => tab.put(ChatLine {
                text: text.to_string(),
                tone,
                item: Some(item),
                change: None,
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
        self.sessions_sel = kept.unwrap_or_else(|| work::in_range(&rows, self.sessions_sel));
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
            first.following = true;
            let intent = Intent::Send {
                tab: first.id,
                session: None,
                agent: first.agent_id.clone(),
                text,
            };
            self.emit(intent);
        }
    }

    /// Binds the starter tab to `session` as `begin` gives it an agent, so it needs no list; the
    /// first list that holds the session names the tab.
    fn continue_session(&mut self, session: String, agent: Option<String>) {
        let first = &self.tabs[0];
        if first.session.is_some() || first.starting || first.agent_picked {
            return;
        }
        let agent = agent.unwrap_or_else(|| first.agent_id.clone());
        let name = self.agent_name(&agent);
        let first = &mut self.tabs[0];
        first.session = Some(session.clone());
        first.agent = name;
        first.agent_id = agent.clone();
        self.emit(Intent::Open { session, agent });
    }

    /// The tab, or the subagent the sidebar shows, whose session is `session`.
    fn showing(&mut self, session: &str) -> Option<&mut Tab> {
        let mut shown = self.tabs.iter_mut().chain(self.subagent.as_mut());
        shown.find(|tab| tab.session.as_deref() == Some(session))
    }

    fn opened(&mut self, session: &str, lines: Vec<ChatLine>) {
        let Some(tab) = self.showing(session) else {
            return;
        };
        tab.lines = lines;
        // The source tells the prompts again after each open.
        tab.prompts.clear();
        // Opening left an empty tab in input mode; a user who has acted since keeps their mode.
        let active = &self.tabs[self.active];
        let untouched = active.session.as_deref() == Some(session)
            && self.mode == Mode::Input
            && active.input.is_empty()
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
            tab.following = false;
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
                (KeySet::Regular, _) => "INPUT",
                (KeySet::Emacs, _) => "EMACS",
                (KeySet::Vim, Mode::Input) => "INPUT",
                (KeySet::Vim, Mode::Normal) => "NORMAL",
            },
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// The session the center pane's tab shows, if it has one yet.
    pub fn shown_session(&self) -> Option<&str> {
        self.tabs[self.active].session.as_deref()
    }

    pub fn ctx(&self) -> KeyCtx {
        match self.layers.last().map(|top| top.kind) {
            Some(LayerKind::Jump | LayerKind::Picker | LayerKind::Setup | LayerKind::Form) => {
                KeyCtx::Filter
            }
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
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.selection = None;
                self.handle_key(key);
            }
            Event::Paste(text) => self.paste(&text),
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

    /// A subagent opens on its parent tab's connection, which serves the whole session tree.
    pub(crate) fn activate_running(&mut self) {
        let tab = &self.tabs[self.active];
        let Some(row) = tab
            .running_sel
            .and_then(|row| tab.running.get(row))
            .cloned()
        else {
            return;
        };
        match row.opens {
            Opens::Agent(session) => {
                if self.says_lost() {
                    return;
                }
                let tab = &self.tabs[self.active];
                if self.subagent.is_none() {
                    self.before_subagent = (tab.sidebar, self.view);
                }
                let shown = Tab {
                    id: tab.id,
                    session: Some(session.clone()),
                    agent: row.name,
                    agent_id: tab.agent_id.clone(),
                    ..Tab::new_session()
                };
                let agent = shown.agent_id.clone();
                self.subagent = Some(shown);
                self.show_beside(View::Subagent);
                self.emit(Intent::Open { session, agent });
            }
            Opens::Workflow(address) => {
                self.show_beside(View::Workflows);
                self.select_job(&address);
            }
        }
    }

    /// The sidebar shows what a running row opens, and takes the keys where it docks.
    fn show_beside(&mut self, view: View) {
        self.tabs[self.active].running_sel = None;
        self.run_effect(Effect::ShowSidebar(view));
        if sidebar_can_dock(self.size.0) {
            self.focus_pane(Pane::Sidebar);
        }
    }

    /// Focus stays put, so reading the diff does not cost the place in the conversation. `file`
    /// indexes the session's files, so the session scope shows it.
    pub fn show_file(&mut self, file: usize) {
        self.run_effect(Effect::ShowDiff(Scope::Session));
        self.diff = Some(file);
        self.diff_scroll = 0;
        let rows = self.view_rows(View::Diff);
        if let Some(row) = rows
            .iter()
            .position(|row| row.action == RowAction::OpenDiff(file))
        {
            self.view_sel[View::Diff as usize] = row;
        }
    }

    pub fn conversation_rect(&self, pane: Rect) -> Rect {
        let rows = self.tabs[self.active].running.len() as u16;
        let pane = self.above_message(pane);
        conversation_area(Rect {
            height: pane.height.saturating_sub(rows),
            ..pane
        })
    }

    /// The message box's text rows: one a line, up to six, leaving the conversation a row, and
    /// the open slash list one of its own.
    pub fn message_rows(&self, pane: Rect) -> u16 {
        let tab = &self.tabs[self.active];
        let room = conversation_area(pane)
            .height
            .saturating_sub(tab.running.len() as u16);
        let mut rows = (tab.input.shown().len() as u16).min(room.max(1));
        while self.slash.is_some() && rows > 1 && slash_rows(slash_area(pane, rows)).height == 0 {
            rows -= 1;
        }
        rows
    }

    /// The pane less the rows the message box grew by, as the rows above the box see it.
    pub fn above_message(&self, pane: Rect) -> Rect {
        Rect {
            height: pane.height + 1 - self.message_rows(pane),
            ..pane
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Esc and the next key in one read reach crossterm as alt-<key>
        // (crossterm-0.29.0/src/event/sys/unix/parse.rs:34-42). In a line, `esc b`, `esc f` and
        // `esc backspace` are also what terminals send for option-left, option-right and
        // option-backspace, so they edit it, as alt-left and alt-right do; alt-enter is a line break.
        let ctx = self.ctx();
        let edits = key.modifiers == KeyModifiers::ALT
            && match key.code {
                KeyCode::Char('b' | 'f') | KeyCode::Left | KeyCode::Right | KeyCode::Backspace => {
                    in_line(ctx)
                }
                KeyCode::Enter => ctx == KeyCtx::Input,
                _ => false,
            };
        if self.keymap.set() == KeySet::Vim && key.modifiers.contains(KeyModifiers::ALT) && !edits {
            self.press(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            self.press(KeyEvent::new(key.code, key.modifiers - KeyModifiers::ALT));
        } else {
            self.press(key);
        }
    }

    /// A filter or search takes it as one line. Otherwise it goes into the message box, which
    /// a paste in normal mode or on the running rows enters, as typing would after `i`.
    fn paste(&mut self, text: &str) {
        self.notice = None;
        match self.ctx() {
            KeyCtx::Filter
                if matches!(
                    self.layers.last().map(|top| top.kind),
                    Some(LayerKind::Setup | LayerKind::Form)
                ) =>
            {
                if let Some(field) = self.focused_field() {
                    field.paste(text);
                }
            }
            KeyCtx::Filter => {
                if let Some(top) = self.layers.last_mut() {
                    top.filter.paste_inline(text);
                    top.selected = 0;
                }
            }
            KeyCtx::SearchLine => {
                if let Some(search) = &mut self.search {
                    search.query.paste_inline(text);
                    search.current = 0;
                    self.go_to_match();
                }
            }
            KeyCtx::Menu if !self.layers.is_empty() => {}
            KeyCtx::Menu | KeyCtx::Normal | KeyCtx::Input => {
                if self.ctx() != KeyCtx::Input {
                    self.act(Action::EnterInput);
                }
                self.focus = Pane::Agent;
                let input = &mut self.tabs[self.active].input;
                input.paste_message(text);
                if !input.text().starts_with('/') {
                    self.slash = None;
                }
            }
        }
    }

    fn focused_field(&mut self) -> Option<&mut Field> {
        match self.layers.last()?.kind {
            LayerKind::Setup => self.setup.as_mut()?.field_mut(),
            LayerKind::Form => self.form.as_mut()?.field_mut(),
            _ => None,
        }
    }

    /// A ctrl-c quits only while the previous one's hint still shows, so any key or paste
    /// between them starts over.
    fn press(&mut self, key: KeyEvent) {
        let armed = self
            .notice
            .take()
            .is_some_and(|notice| notice == CTRL_C_AGAIN);
        let action = self.keymap.map(self.ctx(), key);
        if armed && action == Action::Interrupt {
            self.quit = true;
        } else if self.layers.is_empty() {
            self.act(action);
        } else {
            self.act_on_layer(action);
        }
    }

    /// Regular has no key for the keys overlay but `?` before anything is typed, as chat apps do.
    fn question_opens_keys(&self) -> bool {
        self.keymap.set() == KeySet::Regular
            && self.ctx() == KeyCtx::Input
            && self.tabs[self.active].input.is_empty()
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Insert('?') if self.question_opens_keys() => self.push_layer(LayerKind::Keys),
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
            | Action::End
            | Action::NewLine => self.edit(action),
            Action::Submit if self.ctx() == KeyCtx::SearchLine => self.submit_search(),
            Action::Submit if self.in_running() => self.activate_running(),
            Action::Next if self.in_running() => self.step_running(true),
            Action::Prev if self.in_running() => self.step_running(false),
            Action::Next if self.can_step_line(true) => {
                self.tabs[self.active].input.step(true);
            }
            Action::Prev if self.can_step_line(false) => {
                self.tabs[self.active].input.step(false);
            }
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
            Action::Submit if self.enter_accepts() => self.accept(),
            Action::Submit => self.submit(),
            Action::Back => self.back(),
            Action::Interrupt => self.interrupt(),
            Action::Stop => {
                if !self.stop() {
                    self.notice = Some(NOTHING_RUNNING.to_string());
                }
            }
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
            Action::SearchNext if self.shown_prompt().is_some() => self.dismiss(),
            Action::Accept if self.shown_prompt().is_some() => self.accept(),
            Action::NextTab => self.step_tab(1),
            Action::PrevTab => self.step_tab(self.tabs.len() - 1),
            Action::ClosePane => self.close_pane(),
            Action::ToggleSessions => self.toggle_sessions(),
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::PageUp if self.focus == Pane::Agent => self.scroll_agent(true, By::Page),
            Action::PageDown if self.focus == Pane::Agent => self.scroll_agent(false, By::Page),
            Action::PageUp if self.focus == Pane::Sidebar => self.scroll_subagent(true, By::Page),
            Action::PageDown if self.focus == Pane::Sidebar => {
                self.scroll_subagent(false, By::Page);
            }
            Action::Top => self.jump_to_edge(true),
            Action::Bottom => self.jump_to_edge(false),
            Action::Next => self.step(true),
            Action::Prev => self.step(false),
            Action::Quit => self.quit = true,
            _ => {}
        }
    }

    /// Up and Down move between the message box's lines; from its first or last they do what
    /// they do from a box of one.
    fn can_step_line(&self, down: bool) -> bool {
        let input = &self.tabs[self.active].input;
        let edge = if down { input.lines().len() - 1 } else { 0 };
        self.ctx() == KeyCtx::Input && self.focus == Pane::Agent && input.row() != edge
    }

    fn edit(&mut self, action: Action) {
        if self.ctx() == KeyCtx::SearchLine {
            self.edit_search(action);
            return;
        }
        if matches!(action, Action::Insert(_) | Action::NewLine) {
            self.focus = Pane::Agent;
        }
        if self.focus == Pane::Agent {
            self.tabs[self.active].running_sel = None;
            self.edit_message(action);
        }
    }

    fn edit_message(&mut self, action: Action) {
        let tab = &mut self.tabs[self.active];
        tab.input.edit(action);
        let input = &tab.input;
        if matches!(action, Action::Insert(_)) && input.text() == "/" {
            self.slash = Some(0);
            let agent = tab.agent_id.clone();
            if !matches!(self.connection, Connection::Lost(_)) {
                self.emit(Intent::ReadSkills { agent });
            }
        } else if !input.text().starts_with('/') {
            self.slash = None;
        } else if matches!(
            action,
            Action::Insert(_)
                | Action::Backspace
                | Action::Delete
                | Action::DeleteWord
                | Action::DeleteToStart
                | Action::NewLine
        ) {
            self.slash = self.slash.map(|_| 0);
        }
    }

    /// The commands, then the tab agent's skills, that match the box's first word.
    pub fn slash_matches(&self) -> Vec<SlashRow> {
        let tab = &self.tabs[self.active];
        let text = tab.input.text();
        let (query, _) = slash_words(&text);
        let commands = COMMANDS.iter().filter_map(|command| {
            Some(SlashRow {
                name: command.slash?.to_string(),
                about: command.about.to_string(),
                run: SlashRun::Effect(command.effect),
            })
        });
        let (agent, skills) = &self.skills;
        let skills = skills
            .iter()
            .filter(|_| *agent == tab.agent_id)
            .map(|skill| SlashRow {
                name: skill.id.clone(),
                about: skill.about.clone(),
                run: SlashRun::Skill(skill.id.clone()),
            });
        commands
            .chain(skills)
            .filter(|row| subsequence(query, &row.name))
            .collect()
    }

    fn selected_slash(&self) -> Option<SlashRow> {
        self.slash_matches().into_iter().nth(self.slash?)
    }

    /// A command ignores the text after its name.
    fn run_slash(&mut self) {
        let text = self.tabs[self.active].input.text();
        let Some(row) = self.selected_slash() else {
            self.notice = Some(format!("unknown command: {text}"));
            return;
        };
        let sent = self.tabs[self.active].input.sent();
        let argument = slash_words(&sent).1.to_string();
        match row.run {
            SlashRun::Skill(skill) => self.send_as(Sending::Skill(skill), argument),
            SlashRun::Effect(Effect::Goal) if !argument.is_empty() => {
                self.send_as(Sending::Goal, argument)
            }
            SlashRun::Effect(effect) => {
                self.tabs[self.active].input.clear();
                self.slash = None;
                self.run_effect(effect);
            }
        }
    }

    fn step_slash(&mut self, forward: bool) {
        let max = self.slash_matches().len().saturating_sub(1);
        self.slash = self.slash.map(|selected| stepped(selected, forward, max));
    }

    fn complete_slash(&mut self) {
        if let Some(row) = self.selected_slash() {
            self.tabs[self.active].input.set(&format!("/{}", row.name));
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
            Effect::OpenMenu(id) => self.push_layer(LayerKind::Menu(id)),
            Effect::ClearTab => {
                let tab = &self.tabs[self.active];
                if tab.starting {
                    self.notice = Some(STARTING.to_string());
                } else {
                    self.tabs[self.active] = Tab {
                        id: tab.id,
                        agent: tab.agent.clone(),
                        agent_id: tab.agent_id.clone(),
                        agent_picked: tab.agent_picked,
                        sidebar: tab.sidebar,
                        ..Tab::new_session()
                    };
                    if self.subagent.is_some() {
                        self.set_view(View::Diff);
                    }
                    self.activate_tab(self.active);
                }
            }
            Effect::CloseTab => {
                self.close_tab();
                self.recheck_focus();
            }
            Effect::ShowSidebar(view) => {
                self.set_view(view);
                self.open_sidebar();
            }
            Effect::ShowDiff(scope) => {
                self.set_scope(scope);
                self.open_sidebar();
            }
            // The next enter on the list sends what follows as the objective.
            Effect::Goal => {
                self.tabs[self.active].input.set("/goal ");
                self.slash = Some(0);
            }
            Effect::DismissPrompt => self.dismiss(),
            Effect::Quit => self.quit = true,
        }
    }

    fn use_keys(&mut self, set: KeySet) {
        self.keymap = Keymap::new(set);
        self.activate_tab(self.active);
        self.emit(Intent::SaveKeys(set));
    }

    fn open_sidebar(&mut self) {
        if sidebar_can_dock(self.size.0) {
            self.tabs[self.active].sidebar = true;
        } else {
            self.push_layer(LayerKind::Sidebar);
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
        } else if self.focus == Pane::Sidebar && self.subagent.is_some() {
            self.close_subagent();
        } else {
            self.mode = Mode::Normal;
        }
    }

    /// The right pane goes back to how it was before the subagent opened, and the conversation
    /// has the keys.
    fn close_subagent(&mut self) {
        let (wanted, view) = self.before_subagent;
        self.tabs[self.active].sidebar = wanted;
        self.set_view(view);
        self.focus_pane(Pane::Agent);
    }

    fn send(&mut self) {
        let text = self.tabs[self.active].input.sent();
        self.send_as(Sending::Text, text);
    }

    /// A refusal leaves the box as typed.
    fn send_as(&mut self, sending: Sending, text: String) {
        let message = matches!(sending, Sending::Text);
        let typed = !self.tabs[self.active].input.text().trim().is_empty();
        // A reply to a prompt is a mailbox request, not a turn, so it may answer a subagent's,
        // whose thread the runtime refuses turns on.
        if let Some(reply) = self.prompt_reply().filter(|_| message && typed) {
            if self.says_lost() {
                return;
            }
            let tab = &mut self.tabs[self.active];
            tab.input.clear();
            tab.scroll = 0;
            return self.emit(reply);
        }
        // A message has no agent to go to, so enter opens what connects one. The box keeps
        // what was typed.
        if let Connection::NoAgent(why) = &self.connection {
            self.notice = Some(why.clone());
            return self.push_layer(LayerKind::Setup);
        }
        if (message && !typed) || self.says_lost() {
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
        let (id, session, agent) = (tab.id, tab.session.clone(), tab.agent_id.clone());
        // A goal's first request is the runtime's, not a turn this connection started.
        tab.following = !matches!(sending, Sending::Goal);
        let intent = match sending {
            Sending::Text => Intent::Send {
                tab: id,
                session,
                agent,
                text,
            },
            Sending::Skill(skill) => Intent::SendSkill {
                tab: id,
                session,
                agent,
                skill,
                text,
            },
            Sending::Goal => Intent::SetGoal {
                tab: id,
                session,
                agent,
                objective: text,
            },
        };
        tab.starting = tab.session.is_none();
        tab.input.clear();
        tab.scroll = 0;
        self.slash = None;
        self.emit(intent);
    }

    fn interrupt(&mut self) {
        let searching = self.ctx() == KeyCtx::SearchLine;
        let was_empty = match (self.layers.last_mut(), self.search.as_mut()) {
            (Some(top), _) => std::mem::take(&mut top.filter).is_empty(),
            (None, Some(search)) if searching => std::mem::take(&mut search.query).is_empty(),
            _ => std::mem::take(&mut self.tabs[self.active].input).is_empty(),
        };
        if !was_empty {
            if self.layers.is_empty() {
                self.slash = None;
            }
            if searching {
                self.go_to_match();
            }
            return;
        }
        if !self.stop() {
            self.notice = Some(CTRL_C_AGAIN.to_string());
        }
    }

    /// Stops the running turn of the subagent the sidebar shows while it has the keys, else the
    /// active tab's.
    fn stop(&mut self) -> bool {
        let shown = self.subagent.as_ref().filter(|_| self.subagent_has_keys());
        let tab = shown.unwrap_or(&self.tabs[self.active]);
        let (Some(session), Some(turn)) = (&tab.session, &tab.turn) else {
            return false;
        };
        let intent = Intent::Interrupt {
            session: session.clone(),
            agent: tab.agent_id.clone(),
            turn: turn.clone(),
        };
        if !self.says_lost() {
            self.emit(intent);
            self.notice = Some(STOPPING.to_string());
        }
        true
    }

    fn not_wired(&mut self) {
        self.notice = Some(NOT_WIRED.to_string());
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
                self.showing_right_pane(|app| app.push_layer(LayerKind::Sidebar));
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

    /// A subagent shows only beside the tab it was opened from.
    pub fn activate_tab(&mut self, index: usize) {
        if index != self.active {
            self.diff = None;
        }
        self.active = index;
        let left = self.subagent.as_ref().map(|shown| shown.id);
        if left.is_some_and(|parent| parent != self.tabs[index].id) {
            self.set_view(View::Diff);
        }
        self.slash = None;
        self.search = None;
        self.focus = Pane::Agent;
        self.mode = if self.tabs[index].lines.is_empty() {
            Mode::Input
        } else {
            Mode::Normal
        };
        self.keep_file_selected(None);
        for view in [View::Workflows, View::Agents] {
            let rows = self.view_rows(view);
            self.view_sel[view as usize] = work::in_range(&rows, self.view_sel[view as usize]);
        }
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
            // Picking it again reads it again, for what other clients added. A resume under a
            // turn this tab streams could repeat or drop that turn's text, and the sample keeps
            // nothing to reread.
            let tab = &self.tabs[index];
            let (streaming, agent) = (tab.following, tab.agent_id.clone());
            if self.intents.is_some() && !streaming && !self.says_lost() {
                self.emit(Intent::Open {
                    session: session.id,
                    agent,
                });
            }
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
        self.showing_right_pane(|app| {
            if sidebar_can_dock(app.size.0) {
                let tab = &mut app.tabs[app.active];
                tab.sidebar = !tab.sidebar;
                app.recheck_focus();
            } else {
                app.toggle_layer(LayerKind::Sidebar);
            }
        });
    }

    /// A hidden pane skipped the turn ends' rereads, so the working tree it comes back on is
    /// read again.
    fn showing_right_pane<F>(&mut self, show: F)
    where
        F: FnOnce(&mut App),
    {
        let hidden = !self.right_pane_shown();
        show(self);
        if hidden && self.right_pane_shown() {
            self.read_tree();
        }
    }

    /// A diff belongs to the Diff view, and a subagent to the Subagent view, so each closes when
    /// the view changes. The source rereads a work view, or the working tree, as it is shown.
    pub fn set_view(&mut self, view: View) {
        self.view = view;
        self.diff = None;
        if view != View::Subagent {
            self.subagent = None;
        }
        self.search.take_if(|search| search.pane == Pane::Sidebar);
        let lost = matches!(self.connection, Connection::Lost(_));
        match view {
            View::Diff => self.read_tree(),
            View::Workflows | View::Agents if !lost => self.emit(Intent::Refresh),
            _ => {}
        }
    }

    /// The subagent, while the right pane shows it on screen.
    pub(super) fn subagent_shown(&self) -> Option<&Tab> {
        self.subagent.as_ref().filter(|_| self.right_pane_shown())
    }

    /// Whether the shown subagent's view has the keys: docked and focused, or the top layer.
    pub(super) fn subagent_has_keys(&self) -> bool {
        let sidebar = match self.layers.last() {
            Some(top) => top.kind == LayerKind::Sidebar,
            None => self.focus == Pane::Sidebar,
        };
        sidebar && self.subagent_shown().is_some()
    }

    /// Docked or as a layer: the rail shows only the session's totals.
    fn right_pane_shown(&self) -> bool {
        let docked = self.panes().map(|panes| panes.right);
        let docked = matches!(docked, Some(Right::Sidebar(_)));
        docked
            || self
                .layers
                .iter()
                .any(|layer| layer.kind == LayerKind::Sidebar)
    }

    /// While the Diff view lists the working tree, unless the source is lost.
    fn read_tree(&mut self) {
        let lost = matches!(self.connection, Connection::Lost(_));
        if self.view == View::Diff && self.scope == Scope::Tree && !lost {
            let agent = self.tabs[self.active].agent_id.clone();
            self.emit(Intent::WorkingTree { agent });
        }
    }

    fn step(&mut self, forward: bool) {
        match self.focus {
            Pane::Agent => self.scroll_agent(!forward, By::Row),
            Pane::Sessions => self.step_session(forward),
            Pane::Sidebar => self.step_sidebar(forward),
        }
    }

    fn step_session(&mut self, forward: bool) {
        self.sessions_sel = step_row(&self.session_rows(), self.sessions_sel, forward);
    }

    fn step_sidebar(&mut self, forward: bool) {
        if self.view == View::Subagent {
            return self.scroll_subagent(!forward, By::Row);
        }
        let Some(file) = self.diff else {
            let rows = self.view_rows(self.view);
            let selected = &mut self.view_sel[self.view as usize];
            *selected = step_row(&rows, *selected, forward);
            return;
        };
        if let Some(pane) = self.sidebar_pane() {
            let rows = self.visible_rows(Pane::Sidebar, sidebar_rows_area(pane, self.view));
            let height = usize::from(rows.height);
            let max = self.diff_rows(file).len().saturating_sub(height);
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
    /// `gg` and `G`, `alt-<` and `alt->`: the focused pane's first or last row. A diff past its
    /// end is drawn and stepped from its last screenful, as after a resize.
    fn jump_to_edge(&mut self, top: bool) {
        let edge = |rows: &[SidebarRow]| match top {
            true => rows.iter().position(SidebarRow::selectable),
            false => rows.iter().rposition(SidebarRow::selectable),
        };
        match self.focus {
            Pane::Agent => self.scroll_agent(top, By::End),
            Pane::Sessions => {
                if let Some(row) = edge(&self.session_rows()) {
                    self.sessions_sel = row;
                }
            }
            Pane::Sidebar if self.view == View::Subagent => self.scroll_subagent(top, By::End),
            Pane::Sidebar if self.diff.is_some() => {
                self.diff_scroll = if top { 0 } else { usize::MAX };
            }
            Pane::Sidebar => {
                if let Some(row) = edge(&self.view_rows(self.view)) {
                    self.view_sel[self.view as usize] = row;
                }
            }
        }
    }

    fn scroll_agent(&mut self, up: bool, by: By) {
        let Some(panes) = self.panes() else {
            return;
        };
        let area = self.visible_rows(Pane::Agent, self.conversation_rect(panes.agent));
        let lines = self.conversation_rows(area.width).len();
        let tab = &mut self.tabs[self.active];
        tab.scroll = scrolled(tab.scroll, up, by, lines, area.height);
    }

    /// The sidebar's subagent, as `scroll_agent` scrolls the tab.
    pub(super) fn scroll_subagent(&mut self, up: bool, by: By) {
        let Some(pane) = self.sidebar_pane() else {
            return;
        };
        let rows = self.visible_rows(Pane::Sidebar, sidebar_rows_area(pane, self.view));
        let (_, area) = self.subagent_rows(rows);
        let Some(shown) = &mut self.subagent else {
            return;
        };
        let lines = shown.wrapped_rows(area.width).len();
        shown.scroll = scrolled(shown.scroll, up, by, lines, area.height);
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
        let rows = self.view_rows(self.view);
        match rows
            .get(self.view_sel[self.view as usize])
            .map(|row| row.action)
        {
            Some(RowAction::OpenDiff(file)) => {
                self.diff = Some(file);
                self.diff_scroll = 0;
                self.search = None;
            }
            Some(RowAction::OpenJob(job)) => self.toggle_job(job),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
