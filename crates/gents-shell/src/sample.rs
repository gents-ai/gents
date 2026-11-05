#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Accent,
    Label,
    Warn,
    Add,
    Del,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    None,
    NewSession,
    OpenSession {
        tab: &'static str,
        agent: &'static str,
    },
    OpenDiff(usize),
    NotWired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarRow {
    pub text: &'static str,
    pub tone: Tone,
    pub action: RowAction,
    /// A session's name and day for the jump bar; empty on every other row.
    pub name: &'static str,
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRow {
    pub name: &'static str,
    pub text: &'static str,
    pub tone: Tone,
}

const fn row(text: &'static str, tone: Tone, action: RowAction) -> SidebarRow {
    SidebarRow {
        text,
        tone,
        action,
        name: "",
        date: "",
    }
}

pub const fn note(text: &'static str, tone: Tone) -> SidebarRow {
    row(text, tone, RowAction::None)
}

const fn session(
    text: &'static str,
    tone: Tone,
    tab: &'static str,
    agent: &'static str,
    name: &'static str,
    date: &'static str,
) -> SidebarRow {
    SidebarRow {
        text,
        tone,
        action: RowAction::OpenSession { tab, agent },
        name,
        date,
    }
}

const fn item(text: &'static str, tone: Tone) -> SidebarRow {
    row(text, tone, RowAction::NotWired)
}

const BLANK: SidebarRow = note("", Tone::Plain);

#[rustfmt::skip]
pub const SESSIONS: &[SidebarRow] = &[
    row(" + New session", Tone::Accent, RowAction::NewSession),
    BLANK,
    note(" TODAY", Tone::Label),
    session(" ● cli ux redesign", Tone::Plain, "cli ux", "Engineer", "cli ux redesign", "today"),
    session(" ○ flaky recovery test", Tone::Plain, "flaky", "Reviewer", "flaky recovery test", "today"),
    session(" ! pack registry login", Tone::Warn, "packs", "Engineer", "pack registry login", "today"),
    BLANK,
    note(" YESTERDAY", Tone::Label),
    session(" ○ eval baseline", Tone::Plain, "eval", "Researcher", "eval baseline", "yesterday"),
    session(" ○ desktop plugin page", Tone::Plain, "desktop", "Engineer", "desktop plugin page", "yesterday"),
    session(" ○ weekly triage", Tone::Plain, "triage", "Triage", "weekly triage", "yesterday"),
    session(" ○ pack review notes", Tone::Plain, "review notes", "Reviewer", "pack review notes", "yesterday"),
];

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
    ("you", Tone::Accent),
    ("Group the sign-in commands under one parent.", Tone::Plain),
    ("", Tone::Plain),
    ("Engineer", Tone::Accent),
    (
        "Moving them under `gents auth`, one subcommand per provider,",
        Tone::Plain,
    ),
    ("and folding the probes in beside them.", Tone::Plain),
    ("", Tone::Plain),
    ("  ✓ read  crates/gents-cli/src/cli/args.rs", Tone::Label),
    (
        "  ✓ edit  crates/gents-cli/src/cli/args.rs  +42 -7",
        Tone::Label,
    ),
    ("  … run   cargo check -p gents-cli", Tone::Warn),
    ("", Tone::Plain),
    (
        "! Allow Engineer to run `cargo test -p gents-cli`?",
        Tone::Warn,
    ),
    (PROMPT_ROW, Tone::Label),
    ("", Tone::Plain),
];

pub const AGENTS: &[AgentRow] = &[
    AgentRow {
        name: "Engineer",
        text: " Engineer     default       ChatGPT subscription",
        tone: Tone::Plain,
    },
    AgentRow {
        name: "Reviewer",
        text: " Reviewer                   Claude subscription",
        tone: Tone::Plain,
    },
    AgentRow {
        name: "Researcher",
        text: " Researcher                 Grok subscription",
        tone: Tone::Plain,
    },
    AgentRow {
        name: "Triage",
        text: " Triage       signed out    Grok subscription",
        tone: Tone::Warn,
    },
];

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
    }
}

pub const KEY_ROWS: &[(&str, &str, &str)] = &[
    ("Normal and input modes", "i / esc", "always typing"),
    ("Move between panes", "ctrl-w h/j/k/l", "ctrl-x o"),
    ("Next or previous tab", "gt / gT", "ctrl-x n / ctrl-x p"),
    ("Open a tab", ":tabnew", "ctrl-x t"),
    ("Close the pane or tab", "ctrl-w q", "ctrl-x 0"),
    ("Toggle the sessions sidebar", "ctrl-w [", "ctrl-x ["),
    ("Toggle the right sidebar", "ctrl-w ]", "ctrl-x ]"),
    ("Next sidebar view", "ctrl-w }", "ctrl-x }"),
    ("Quick-jump bar", ":", "alt-x / ctrl-x x"),
    ("Slash commands", "/ while writing", "/"),
    ("Search the focused pane", "/ in normal mode", "ctrl-s"),
    ("Back one layer", "esc", "ctrl-g"),
    ("Keys overlay", "?", "ctrl-x ?"),
    ("Scroll the focused pane", "j / k", "ctrl-n / ctrl-p"),
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
