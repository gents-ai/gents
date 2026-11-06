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
        running: &'static [Running],
    },
    Started {
        tab: u64,
        session: String,
    },
    /// The source could not start a session for `tab`.
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
    /// What `gents` was opened with: the starter tab takes `agent` and sends `prompt`.
    Begin {
        agent: Option<String>,
        prompt: Option<String>,
    },
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
pub struct Agent {
    pub id: String,
    pub name: String,
    /// The agent's row in the picker.
    pub text: String,
    pub tone: Tone,
}
