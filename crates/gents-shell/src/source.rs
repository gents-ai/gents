use std::io;

use ratatui::crossterm::event::Event;

use crate::app::ChatLine;
use crate::sample::{Running, Tone};

/// What the shell asks of its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Open {
        session: String,
        agent: String,
    },
    /// With no session, the source starts one and answers `Started` for `tab`.
    Send {
        tab: u64,
        session: Option<String>,
        agent: String,
        text: String,
    },
    Interrupt {
        session: String,
        agent: String,
        turn: String,
    },
    /// The Workflows or Background view is shown.
    Refresh,
    /// Closes the mailbox item a prompt shows.
    Dismiss {
        session: String,
        item: String,
    },
    /// Answers the mailbox item a prompt shows with what the user typed.
    Reply {
        session: String,
        agent: String,
        item: String,
        text: String,
    },
    /// The Diff view shows the working tree: `agent`'s connection reads its tool root's.
    WorkingTree {
        agent: String,
    },
}

/// What the source tells the shell. One for a session no tab shows is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Connection(Connection),
    Agents(Vec<Agent>),
    Sessions(Vec<Session>),
    SessionState {
        session: String,
        state: SessionState,
    },
    Opened {
        session: String,
        lines: Vec<ChatLine>,
    },
    /// The session's running rows, replaced whole.
    Running {
        session: String,
        rows: Vec<Running>,
    },
    Started {
        tab: u64,
        session: String,
    },
    /// The source could not start `tab`'s session or turn.
    StartFailed {
        tab: u64,
        message: String,
    },
    Item {
        session: String,
        line: ChatLine,
    },
    Delta {
        session: String,
        item: String,
        text: String,
        tone: Tone,
    },
    /// The session's turn in progress.
    Turn {
        session: String,
        turn: String,
    },
    /// A tab still showing `turn` as in progress clears it; a newer turn stays.
    TurnEnded {
        session: String,
        turn: String,
    },
    Notice(String),
    /// The session's open questions and approvals, oldest first, replaced whole.
    Prompts {
        session: String,
        prompts: Vec<Prompt>,
    },
    /// The Workflows view's jobs, replaced whole. `as_of` says how current they are, in the
    /// view's header: `live`, `as of 14:05`, or nothing.
    Jobs {
        jobs: Vec<Job>,
        as_of: String,
    },
    /// The Background view's calls, replaced whole, with `as_of` as for `Jobs`.
    Background {
        calls: Vec<BackgroundCall>,
        as_of: String,
    },
    /// What `gents` was opened with: the starter tab takes `agent` and sends `prompt`.
    Begin {
        agent: Option<String>,
        prompt: Option<String>,
    },
    /// The working tree's changes in git's order, replaced whole, or why it could not be read.
    WorkingTree(Result<Vec<Change>, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    Sample,
    Connecting,
    Ready,
    Lost(String),
}

/// The UI blocks on one channel of these.
#[derive(Debug)]
pub enum Input {
    Terminal(Event),
    /// Reading the terminal failed: the shell ends with this error.
    TerminalError(io::Error),
    Runtime(Update),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    /// The id of the agent the session runs on.
    pub agent: String,
    pub name: String,
    pub tab: String,
    pub day: Day,
    pub state: SessionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Day {
    Today,
    Yesterday,
    Older,
}

impl Day {
    pub fn label(self) -> &'static str {
        match self {
            Day::Today => "today",
            Day::Yesterday => "yesterday",
            Day::Older => "older",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Idle,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub address: String,
    pub session: String,
    pub name: String,
    pub title: String,
    pub status: String,
    pub waiting: bool,
    pub ended: Option<JobEnd>,
    /// The phase and active-child lines, already formatted.
    pub detail: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobEnd {
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundCall {
    pub session: String,
    pub name: String,
    pub status: String,
    pub done: bool,
}

/// A mailbox item that asks the user something, shown under the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub item: String,
    pub kind: PromptKind,
    pub title: String,
    pub summary: Option<String>,
    pub answer: Answer,
    pub dismissable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Ask,
    Gate,
}

/// How the item is answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Ack,
    Reply,
    Choose {
        options: Vec<String>,
        free_text: bool,
        skip: bool,
    },
    /// By writing a document of `collection`, which names the item by `key`.
    Document {
        collection: String,
        key: String,
    },
}

/// One file a turn changed, as the runtime recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub turn: String,
    pub path: String,
    pub kind: ChangeKind,
    pub diff: String,
    added: usize,
    removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Add,
    Delete,
    Update,
    Move { to: String },
}

impl Change {
    /// Counts the lines once. An add's lines are all added and a delete's all removed. An
    /// update's `---` and `+++` lines before its first `@@` name the files, not lines.
    pub fn new(turn: String, path: String, kind: ChangeKind, diff: String) -> Change {
        let (added, removed) = match kind {
            ChangeKind::Add => (diff.lines().count(), 0),
            ChangeKind::Delete => (0, diff.lines().count()),
            ChangeKind::Update | ChangeKind::Move { .. } => {
                let (mut added, mut removed, mut hunk) = (0, 0, false);
                for line in diff.lines() {
                    hunk |= line.starts_with("@@");
                    if !hunk && (line.starts_with("---") || line.starts_with("+++")) {
                        continue;
                    }
                    added += usize::from(line.starts_with('+'));
                    removed += usize::from(line.starts_with('-'));
                }
                (added, removed)
            }
        };
        Change {
            turn,
            path,
            kind,
            diff,
            added,
            removed,
        }
    }

    pub fn counts(&self) -> (usize, usize) {
        (self.added, self.removed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// The agent's row in the picker.
    pub text: String,
    pub tone: Tone,
}
