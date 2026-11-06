use std::borrow::Cow;

use crate::app::{sample_chat, ChatLine};
use crate::source::{Agent, Day, Intent, Session, SessionState, Update};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Accent,
    Label,
    Warn,
    Add,
    Del,
    /// The asker's message: right-aligned on a tinted block.
    You,
    /// An agent's prose: a highlighted row, like a selected one.
    Reply,
    /// A changed file in the conversation, which opens that file's diff.
    File(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    OpenMenu(MenuId),
    None,
    NewSession,
    /// A position in `App.sessions`.
    OpenSession(usize),
    OpenDiff(usize),
    NotWired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub text: Cow<'static, str>,
    pub tone: Tone,
    pub action: RowAction,
    /// A session's name and day for the jump bar; empty on every other row.
    pub name: Cow<'static, str>,
    pub date: &'static str,
}

impl SidebarRow {
    pub fn selectable(&self) -> bool {
        self.action != RowAction::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Changes,
    Workflows,
    Agents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuId {
    Accounts,
    Agents,
    Tools,
    Tasks,
    Mailbox,
    Status,
    NodeSettings,
    AppSettings,
}

impl MenuId {
    pub fn name(self) -> &'static str {
        match self {
            MenuId::Accounts => "Accounts",
            MenuId::Agents => "Agents",
            MenuId::Tools => "Tools",
            MenuId::Tasks => "Tasks",
            MenuId::Mailbox => "Mailbox",
            MenuId::Status => "Status",
            MenuId::NodeSettings => "Node settings",
            MenuId::AppSettings => "App settings",
        }
    }
}

pub const fn row(text: &'static str, tone: Tone, action: RowAction) -> SidebarRow {
    SidebarRow {
        text: Cow::Borrowed(text),
        tone,
        action,
        name: Cow::Borrowed(""),
        date: "",
    }
}

pub const fn note(text: &'static str, tone: Tone) -> SidebarRow {
    row(text, tone, RowAction::None)
}

const fn item(text: &'static str, tone: Tone) -> SidebarRow {
    row(text, tone, RowAction::NotWired)
}

const BLANK: SidebarRow = note("", Tone::Plain);

fn session(name: &str, tab: &str, agent: &str, day: Day, state: SessionState) -> Session {
    Session {
        id: name.to_string(),
        agent: agent.to_string(),
        name: name.to_string(),
        tab: tab.to_string(),
        day,
        state,
    }
}

#[rustfmt::skip]
pub fn sessions() -> Vec<Session> {
    use Day::{Today, Yesterday};
    use SessionState::{Active, Error, Idle};
    vec![
        session("cli ux redesign", "cli ux", "Engineer", Today, Active),
        session("flaky recovery test", "flaky", "Reviewer", Today, Idle),
        session("pack registry login", "packs", "Engineer", Today, Error),
        session("eval baseline", "eval", "Researcher", Yesterday, Idle),
        session("desktop plugin page", "desktop", "Engineer", Yesterday, Idle),
        session("weekly triage", "triage", "Triage", Yesterday, Idle),
        session("pack review notes", "review notes", "Reviewer", Yesterday, Idle),
    ]
}

pub const START_SESSION: usize = 3;

const CHANGES: &[SidebarRow] = &[
    note(" 3 files   +58  -12", Tone::Label),
    BLANK,
    row(
        " M cli/args.rs      +42 -7",
        Tone::Plain,
        RowAction::OpenDiff(0),
    ),
    row(
        " M cli/mod.rs       +11 -5",
        Tone::Plain,
        RowAction::OpenDiff(1),
    ),
    row(
        " A shell/mod.rs      +5",
        Tone::Plain,
        RowAction::OpenDiff(2),
    ),
    BLANK,
    note(" @@ cli/args.rs:85 @@", Tone::Label),
    note(" - GrokLogin(GrokLoginArgs),", Tone::Del),
    note(" + Auth {", Tone::Add),
];

const WORKFLOWS: &[SidebarRow] = &[
    note(" 2 running   1 waiting", Tone::Label),
    BLANK,
    row(" ● change unit: cli ux", Tone::Plain, RowAction::NotWired),
    note("     build · step 3 of 5", Tone::Label),
    row(" ● nightly eval", Tone::Plain, RowAction::NotWired),
    note("     trial 12 of 40", Tone::Label),
    row(" ! pack release", Tone::Warn, RowAction::NotWired),
    note("     waiting on your approval", Tone::Label),
];

const BACKGROUND_AGENTS: &[SidebarRow] = &[
    note(" 3 background   1 done", Tone::Label),
    BLANK,
    row(" ● Reviewer · cli ux", Tone::Plain, RowAction::NotWired),
    note("     reading crates/gents-cli", Tone::Label),
    row(
        " ● Researcher · flaky test",
        Tone::Plain,
        RowAction::NotWired,
    ),
    note("     running for 4m", Tone::Label),
    row(" ● Engineer · pack sync", Tone::Plain, RowAction::NotWired),
    note("     running for 11m", Tone::Label),
    row(" ✓ Triage · inbox", Tone::Accent, RowAction::NotWired),
    note("     finished 2m ago", Tone::Label),
];

pub fn view_rows(view: View) -> &'static [SidebarRow] {
    match view {
        View::Changes => CHANGES,
        View::Workflows => WORKFLOWS,
        View::Agents => BACKGROUND_AGENTS,
    }
}

pub const RAIL: &[SidebarRow] = &[
    note(" 3  ", Tone::Label),
    BLANK,
    note(" +58", Tone::Add),
    note(" -12", Tone::Del),
];

pub const PROMPT_ROW: &str = "  y yes   n no   a always";

pub const CHAT: &[(&str, Tone)] = &[
    ("Group the sign-in commands under one parent.", Tone::You),
    ("", Tone::Plain),
    (
        " Moving them under `gents auth`, one subcommand per provider,",
        Tone::Reply,
    ),
    (" and folding the probes in beside them.", Tone::Reply),
    ("", Tone::Plain),
    ("  ✓ read  crates/gents-cli/src/cli/args.rs", Tone::Label),
    (
        "  ✓ edit  crates/gents-cli/src/cli/args.rs  +42 -7",
        Tone::Label,
    ),
    ("  … run   cargo check -p gents-cli", Tone::Warn),
    ("", Tone::Plain),
    ("  3 files changed   +58 -12", Tone::Label),
    ("  M cli/args.rs      +42 -7", Tone::File(0)),
    ("  M cli/mod.rs       +11 -5", Tone::File(1)),
    ("  A shell/mod.rs      +5", Tone::File(2)),
    ("", Tone::Plain),
    (
        "! Allow Engineer to run `cargo test -p gents-cli`?",
        Tone::Warn,
    ),
    (PROMPT_ROW, Tone::Label),
    ("", Tone::Plain),
];

/// Marks a subagent's reply. Its row in the running list uses the same glyph.
pub const SUBAGENT_MARK: &str = " ◆";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Working,
    Done,
    Failed,
}

/// What `enter` on a running row reaches: a subagent's session, which opens on its parent tab's
/// connection, or a row of the Workflows view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opens {
    Agent(String),
    Workflow(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub name: String,
    pub job: String,
    pub status: String,
    pub state: RunState,
    pub opens: Opens,
}

type Chat = &'static [(&'static str, Tone)];

/// Name, job, status, state and conversation. Each is the session `child:<name>`.
const SUBAGENTS: [(&str, &str, &str, RunState, Chat); 3] = [
    (
        "Reviewer",
        "auth grouping",
        "reading crates/gents-cli",
        RunState::Working,
        &[
            (
                "Review the `gents auth` grouping and report only real problems.",
                Tone::You,
            ),
            ("", Tone::Plain),
            (
                " ◆ Checking the new subcommands against the old login flags.",
                Tone::Reply,
            ),
            ("", Tone::Plain),
            ("  ✓ read  crates/gents-cli/src/cli/args.rs", Tone::Label),
            ("  … read  crates/gents-cli/src/cli/mod.rs", Tone::Warn),
            ("", Tone::Plain),
        ],
    ),
    (
        "Researcher",
        "registry login",
        "running for 4m",
        RunState::Working,
        &[
            (
                "Find out how the pack registry expects clients to sign in.",
                Tone::You,
            ),
            ("", Tone::Plain),
            (
                " ◆ Reading the registry client and its token handling.",
                Tone::Reply,
            ),
            ("", Tone::Plain),
            ("  … search  registry token refresh", Tone::Warn),
            ("", Tone::Plain),
        ],
    ),
    (
        "Triage",
        "inbox",
        "finished 2m ago",
        RunState::Done,
        &[
            ("Sort the new issues by area.", Tone::You),
            ("", Tone::Plain),
            (
                " ◆ Done: 4 for the CLI, 2 for packs, 1 for desktop.",
                Tone::Reply,
            ),
            ("", Tone::Plain),
        ],
    ),
];

pub fn running() -> Vec<Running> {
    let row = |name: &str, job: &str, status: &str, state, opens| Running {
        name: name.to_string(),
        job: job.to_string(),
        status: status.to_string(),
        state,
        opens,
    };
    let workflow = row(
        "change unit",
        "cli ux",
        "build · step 3 of 5",
        RunState::Working,
        Opens::Workflow(2),
    );
    SUBAGENTS
        .iter()
        .map(|&(name, job, status, state, _)| {
            row(
                name,
                job,
                status,
                state,
                Opens::Agent(format!("child:{name}")),
            )
        })
        .chain([workflow])
        .collect()
}

fn agent(name: &str, text: &str, tone: Tone) -> Agent {
    Agent {
        id: name.to_string(),
        name: name.to_string(),
        text: text.to_string(),
        tone,
    }
}

pub fn agents() -> Vec<Agent> {
    vec![
        agent(
            "Engineer",
            " Engineer     default       ChatGPT subscription",
            Tone::Plain,
        ),
        agent(
            "Reviewer",
            " Reviewer                   Claude subscription",
            Tone::Plain,
        ),
        agent(
            "Researcher",
            " Researcher                 Grok subscription",
            Tone::Plain,
        ),
        agent(
            "Triage",
            " Triage       signed out    Grok subscription",
            Tone::Warn,
        ),
    ]
}

const SAMPLE_REPLY: &str = " sample reply: nothing is wired to an agent yet.";

fn chat_lines(chat: Chat) -> Vec<ChatLine> {
    chat.iter()
        .map(|&(text, tone)| ChatLine {
            text: text.to_string(),
            tone,
            item: None,
        })
        .collect()
}

/// What a source would answer, at once. Its lines carry no transcript item, so each appends.
pub fn answer(intent: &Intent) -> Vec<Update> {
    match intent {
        Intent::Open { session, agent } => {
            let child = SUBAGENTS
                .iter()
                .find(|(name, ..)| session.strip_prefix("child:") == Some(name));
            let (lines, rows) = match child {
                Some(&(.., chat)) => (chat_lines(chat), Vec::new()),
                None => (sample_chat(agent), running()),
            };
            vec![
                Update::Opened {
                    session: session.clone(),
                    lines,
                },
                Update::Running {
                    session: session.clone(),
                    rows,
                },
            ]
        }
        Intent::Send {
            tab, session, text, ..
        } => {
            let mut updates = Vec::new();
            let session = match session {
                Some(session) => session.clone(),
                None => {
                    let session = format!("sample:{tab}");
                    updates.push(Update::Started {
                        tab: *tab,
                        session: session.clone(),
                    });
                    session
                }
            };
            for (text, tone) in [(text.as_str(), Tone::You), (SAMPLE_REPLY, Tone::Reply)] {
                let line = ChatLine {
                    text: text.to_string(),
                    tone,
                    item: None,
                };
                updates.push(Update::Item {
                    session: session.clone(),
                    line,
                });
            }
            updates
        }
        Intent::Interrupt { .. } => Vec::new(),
    }
}

pub const PICKER_FOOTER: &str = " enter open in a new tab · esc back";

pub const MENUS: [MenuId; 6] = [
    MenuId::Accounts,
    MenuId::Agents,
    MenuId::Tools,
    MenuId::Tasks,
    MenuId::Mailbox,
    MenuId::Status,
];

/// A row of the Tools menu the jump bar also lists, with its detail.
pub const TOOLS_PACK: (&str, &str) = ("Tools > code review pack", "3 tools");

const ACCOUNTS: &[SidebarRow] = &[
    item(" ● Claude    subscription    signed in", Tone::Plain),
    item(" ● Codex     subscription    signed in", Tone::Plain),
    item(" ○ Grok      not signed in", Tone::Label),
    item(" ○ Cloud     not signed in", Tone::Label),
];

/// The sections of the desktop app's Node page, in its order. Four open the menu the shell
/// already has for them; the rest are not wired.
const NODE_SETTINGS: &[SidebarRow] = &[
    note(" CONFIGURE", Tone::Label),
    item(" Node", Tone::Plain),
    row(" Agents", Tone::Plain, RowAction::OpenMenu(MenuId::Agents)),
    item(" Skills", Tone::Plain),
    row(
        " Providers",
        Tone::Plain,
        RowAction::OpenMenu(MenuId::Accounts),
    ),
    note(" AUTOMATION", Tone::Label),
    row(" Tasks", Tone::Plain, RowAction::OpenMenu(MenuId::Tasks)),
    item(" Triggers", Tone::Plain),
    item(" Schedules", Tone::Plain),
    item(" Event sources", Tone::Plain),
    item(" Workflows", Tone::Plain),
    note(" TOOLS", Tone::Label),
    row(" Tools", Tone::Plain, RowAction::OpenMenu(MenuId::Tools)),
    item(" Remote Tools", Tone::Plain),
    item(" Allowed folders", Tone::Plain),
    note(" PACKS", Tone::Label),
    item(" Packs", Tone::Plain),
    item(" UX Plugins", Tone::Plain),
];

/// The sections of the desktop app's Settings page.
const APP_SETTINGS: &[SidebarRow] = &[
    item(" General", Tone::Plain),
    item(" Developer", Tone::Plain),
    item(" Experimental features", Tone::Plain),
];

const AGENTS_MENU: &[SidebarRow] = &[
    item(" Engineer", Tone::Plain),
    item(" Reviewer", Tone::Plain),
    item(" Researcher", Tone::Plain),
    BLANK,
    item(" + New agent", Tone::Accent),
];

const TOOLS: &[SidebarRow] = &[
    item(" filesystem    12 tools", Tone::Plain),
    item(" git pack       8 tools", Tone::Plain),
    item(" web pack       3 tools", Tone::Plain),
    item(" mcp: linear    off", Tone::Label),
    item(" code review pack   3 tools", Tone::Plain),
];

const TASKS: &[SidebarRow] = &[
    item(" nightly eval     02:00", Tone::Plain),
    item(" triage inbox    hourly", Tone::Plain),
    item(" pack sync         idle", Tone::Label),
];

const MAILBOX: &[SidebarRow] = &[
    item(" ! approve: cargo test", Tone::Warn),
    item(" ! pack registry sign-in", Tone::Warn),
];

const STATUS: &[SidebarRow] = &[
    item(" ○ runtime    not connected", Tone::Label),
    item(" ○ DefraDB    not connected", Tone::Label),
];

pub fn menu(id: MenuId) -> (&'static [SidebarRow], &'static str) {
    match id {
        MenuId::Accounts => (ACCOUNTS, " enter sign in · d disable · esc back"),
        MenuId::Agents => (AGENTS_MENU, " enter open · esc back"),
        MenuId::Tools => (TOOLS, " enter open · esc back"),
        MenuId::Tasks => (TASKS, " enter open · esc back"),
        MenuId::Mailbox => (MAILBOX, " enter open   d dismiss"),
        MenuId::Status => (STATUS, " sample data: nothing here is wired"),
        MenuId::NodeSettings => (NODE_SETTINGS, " enter open · esc back"),
        MenuId::AppSettings => (APP_SETTINGS, " enter open · esc back"),
    }
}

pub const KEY_ROWS: &[(&str, &str, &str)] = &[
    ("Normal and input modes", "i / esc", "always typing"),
    ("Move between panes", "h / l, ctrl-w h/j/k/l", "ctrl-x o"),
    ("Next or previous tab", "gt / gT", "ctrl-x n / ctrl-x p"),
    ("Open a tab", ":tabnew", "ctrl-x t"),
    ("Close the pane or tab", "ctrl-w q", "ctrl-x 0"),
    ("Toggle the sessions sidebar", "[ or ctrl-w [", "ctrl-x ["),
    ("Toggle the right sidebar", "] or ctrl-w ]", "ctrl-x ]"),
    ("Next sidebar view", "ctrl-w }", "ctrl-x }"),
    ("Quick-jump bar", ": / ctrl-p", "alt-x / ctrl-x x"),
    ("Slash commands", "/ while writing", "/"),
    ("Search the focused pane", "/ in normal mode", "ctrl-s"),
    ("Back one layer", "esc", "ctrl-g"),
    ("Keys overlay", "?", "ctrl-x ?"),
    (
        "Scroll the focused pane",
        "j/k · pgup/pgdn",
        "ctrl-n/p · pgup/pgdn",
    ),
    ("Send", "enter", "enter"),
    ("Quit", ":q then enter", "ctrl-x ctrl-c"),
    ("ctrl-c", "clears, or says how to quit", ""),
];

pub const DIFF_BACK_ROW: &str = " esc back to files";

const DIFFS: [&[(&str, Tone)]; 3] = [
    &[
        (" cli/args.rs       +42 -7", Tone::Label),
        ("", Tone::Plain),
        (" @@ -85,8 +85,4 @@", Tone::Label),
        ("   #[derive(Subcommand)]", Tone::Plain),
        (" - CodexLogin(CodexLoginArgs),", Tone::Del),
        (" - GrokLogin(GrokLoginArgs),", Tone::Del),
        (" - ClaudeLogin(ClaudeLoginArgs),", Tone::Del),
        (" + Auth {", Tone::Add),
        (" +     command: AuthCommand,", Tone::Add),
        (" + },", Tone::Add),
        ("", Tone::Plain),
        (DIFF_BACK_ROW, Tone::Label),
    ],
    &[
        (" cli/mod.rs        +11 -5", Tone::Label),
        ("", Tone::Plain),
        (" @@ -12,7 +12,13 @@", Tone::Label),
        ("   mod args;", Tone::Plain),
        (" - mod login;", Tone::Del),
        (" + mod auth;", Tone::Add),
        (" + mod shell;", Tone::Add),
        ("", Tone::Plain),
        (DIFF_BACK_ROW, Tone::Label),
    ],
    &[
        (" shell/mod.rs       +5", Tone::Label),
        ("", Tone::Plain),
        (" @@ -0,0 +1,5 @@", Tone::Label),
        (" + pub fn run() {", Tone::Add),
        (" +     start();", Tone::Add),
        (" + }", Tone::Add),
        ("", Tone::Plain),
        (DIFF_BACK_ROW, Tone::Label),
    ],
];

pub fn diff(file: usize) -> &'static [(&'static str, Tone)] {
    DIFFS[file]
}
