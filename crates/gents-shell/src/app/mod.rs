pub mod changes;
pub mod fields;
pub mod form;
pub mod input;
pub mod layers;
mod mouse;
mod prompt;
pub mod search;
pub mod setup;
pub mod work;

use std::cell::RefCell;
use std::fmt;
use std::sync::mpsc::Sender;

use rapidhash::{HashMapExt, RapidHashMap};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::commands::{Effect, COMMANDS};
use crate::draw::agent::{slash_area, slash_rows};
use crate::filter::subsequence;
use crate::keys::{in_line, Action, Dir, KeyCtx, KeySet, Keymap};
use crate::layout::{
    conversation_area, layout, sessions_can_dock, sidebar_can_dock, sidebar_rows_area, Panes,
    Right, Screen, Wanted,
};
use crate::sample::{
    answer, changes, note, row, MenuId, Opens, RowAction, Running, Scope, SidebarRow, Tone, View,
    CHAT, START_SESSION,
};
use crate::source::{
    Agent, BackgroundCall, Change, Connection, Day, Intent, Job, MenuRow, Prompt, Session,
    SessionState, Skill, Update,
};
use fields::Field;
use form::FormState;
use input::Lines;
use layers::{Layer, LayerKind};
use search::Search;
use setup::Setup;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub text: String,
    pub tone: Tone,
    /// The transcript item the line shows.
    pub item: Option<String>,
    /// The file change the line shows.
    pub change: Option<Change>,
}

fn blank() -> ChatLine {
    ChatLine {
        text: String::new(),
        tone: Tone::Plain,
        item: None,
        change: None,
    }
}

/// `scroll` counts lines up from the bottom. No `lines` means no messages yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// Set by `App::push_tab`; the subagent the sidebar shows keeps its parent's.
    pub id: u64,
    /// The id of the session the tab shows.
    pub session: Option<String>,
    pub name: String,
    pub agent: String,
    pub agent_id: String,
    pub lines: Vec<ChatLine>,
    pub scroll: usize,
    pub input: Lines,
    pub agent_picked: bool,
    pub running: Vec<Running>,
    /// The selected running row, while those rows have the keys.
    pub running_sel: Option<usize>,
    /// The session's turn in progress.
    pub turn: Option<String>,
    /// A send without a session is waiting for its `Started`.
    pub starting: bool,
    /// Whether the right pane is wanted while this tab shows. Not saved across runs.
    pub sidebar: bool,
    /// The session's open mailbox questions and approvals, oldest first.
    pub prompts: Vec<Prompt>,
    /// The tab sent a turn it has not seen end. The endpoint streams that turn here, and no
    /// other client's.
    pub following: bool,
    wrapped: Wrapped,
}

/// Each line's rows at the width they were wrapped for. Every frame reads the rows, and wrapping
/// a long transcript costs far more than drawing it, so only a changed line is wrapped again.
#[derive(Default)]
struct Wrapped(RefCell<(u16, Vec<WrappedLine>)>);

type WrappedLine = (ChatLine, Vec<String>);

/// A copy starts empty, and two tabs never differ by what they have cached.
impl Clone for Wrapped {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for Wrapped {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Wrapped {}

impl fmt::Debug for Wrapped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Wrapped")
    }
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
            input: Lines::default(),
            agent_picked: false,
            running: Vec::new(),
            running_sel: None,
            turn: None,
            starting: false,
            sidebar: false,
            prompts: Vec::new(),
            following: false,
            wrapped: Wrapped::default(),
        }
    }

    /// The lines' rows with each ended turn's card in place. Cards are built on each call, from
    /// the lines, and never cached.
    pub fn wrapped_rows(&self, width: u16) -> Vec<(String, Tone)> {
        let mut cards = self.cards(usize::from(width)).into_iter().peekable();
        let mut wrapped = self.wrapped.0.borrow_mut();
        let (wrapped_at, seen) = &mut *wrapped;
        if *wrapped_at != width {
            *wrapped_at = width;
            seen.clear();
        }
        seen.truncate(self.lines.len());
        let mut rows = Vec::new();
        for (at, line) in self.lines.iter().enumerate() {
            while let Some((_, card)) = cards.next_if(|(before, _)| *before == at) {
                rows.extend(card);
            }
            if !matches!(seen.get(at), Some((was, _)) if was == line) {
                let fresh = (line.clone(), line_rows(line, usize::from(width)));
                match seen.get_mut(at) {
                    Some(slot) => *slot = fresh,
                    None => seen.push(fresh),
                }
            }
            rows.extend(seen[at].1.iter().map(|row| (row.clone(), line.tone)));
        }
        rows.extend(cards.flat_map(|(_, card)| card));
        rows
    }

    /// An unseen item appends its line and a blank, and a seen one replaces its line.
    fn put(&mut self, line: ChatLine) {
        let seen = line.item.as_deref().and_then(|item| self.item_line(item));
        match seen {
            Some(index) => self.lines[index] = line,
            None => self.lines.extend([line, blank()]),
        }
    }

    /// The selected row stays selected while it is listed, else its position is kept in range.
    fn set_running(&mut self, rows: Vec<Running>) {
        let selected = self.running_sel.and_then(|row| self.running.get(row));
        let kept =
            selected.and_then(|selected| rows.iter().position(|row| row.opens == selected.opens));
        let last = rows.len().checked_sub(1);
        let clamped = self.running_sel.zip(last).map(|(sel, last)| sel.min(last));
        self.running_sel = kept.or(clamped);
        self.running = rows;
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

fn line_rows(line: &ChatLine, width: usize) -> Vec<String> {
    match line.tone {
        Tone::You => bubble(&line.text, width),
        _ => wrap_lines(&line.text, width),
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
    let widest = rows.iter().map(|row| Span::raw(row.as_str()).width()).max();
    let left = width.saturating_sub(widest.unwrap_or(0) + 1);
    rows.iter()
        .map(|row| format!("{:left$}{row}", ""))
        .collect()
}

/// The sample's changes come after its sixth line, the read.
pub fn sample_chat() -> Vec<ChatLine> {
    let chat = |lines: &[(&str, Tone)]| -> Vec<ChatLine> {
        lines
            .iter()
            .map(|&(text, tone)| ChatLine {
                text: text.to_string(),
                tone,
                item: None,
                change: None,
            })
            .collect()
    };
    let (read, ran) = CHAT.split_at(6);
    [chat(read), changes(), chat(ran)].concat()
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
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
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
mod tests {
    use super::*;
    use crate::draw;
    use crate::draw::theme;
    use crate::keys::KeySet::{Emacs, Regular, Vim};
    use crate::sample::RunState;
    use crate::testing::{
        buffer, find, keys, kinds, numbered, press, right_pane, screen, sent, session_row,
        type_text, with_right_pane, with_sample_session,
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
}
