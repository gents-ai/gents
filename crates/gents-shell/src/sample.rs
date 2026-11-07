use std::borrow::Cow;

use crate::app::{sample_chat, ChatLine, NOT_WIRED};
use crate::source::{
    Agent, Answer, BackgroundCall, Change, ChangeKind, Day, Discovered, Intent, Job, MenuAct,
    MenuRow, Prompt, PromptKind, SavedAccount, Session, SessionState, SettingKey, SetupAuth,
    SetupMethod, SetupModel, SetupProvider, SetupSetting, Update,
};

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
    /// A position in `App.jobs`.
    OpenJob(usize),
    /// A position in the menu's rows as the source read them.
    MenuRow(usize),
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
    Diff,
    Workflows,
    Agents,
}

/// What the Diff view lists: this tab's session, or the folder its tools work in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Session,
    Tree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuId {
    Accounts,
    Agents,
    Tools,
    Tasks,
    Mailbox,
    Status,
    NodeSettings,
    AppSettings,
    Workflows,
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
            MenuId::Workflows => "Workflows",
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

/// The session whose row is `START_SESSION`. Each work view lists only the shown session's work,
/// so all of the sample's is here.
const SAMPLE_SESSION: &str = "cli ux redesign";

/// Today's Workflows view: the sample's running row opens the first.
pub fn jobs() -> Vec<Job> {
    let job = |address: &str, name: &str, title: &str, status: &str| Job {
        address: address.to_string(),
        session: SAMPLE_SESSION.to_string(),
        name: name.to_string(),
        title: title.to_string(),
        status: status.to_string(),
        waiting: false,
        ended: None,
        detail: Vec::new(),
    };
    let phases = ["✓ plan", "✓ edit", "● build", "○ test", "○ review"];
    vec![
        Job {
            detail: phases.map(str::to_string).into(),
            ..job(
                "sample:change-unit",
                "change unit",
                "cli ux",
                "build · step 3 of 5",
            )
        },
        job("sample:nightly-eval", "nightly eval", "", "trial 12 of 40"),
        Job {
            waiting: true,
            ..job(
                "sample:pack-release",
                "pack release",
                "",
                "waiting on your approval",
            )
        },
    ]
}

pub fn background() -> Vec<BackgroundCall> {
    let call = |name: &str, status: &str, done| BackgroundCall {
        session: SAMPLE_SESSION.to_string(),
        name: name.to_string(),
        status: status.to_string(),
        done,
    };
    vec![
        call("Reviewer · cli ux", "reading crates/gents-cli", false),
        call("Researcher · flaky test", "running for 4m", false),
        call("Engineer · pack sync", "running for 11m", false),
        call("Triage · inbox", "finished 2m ago", true),
    ]
}

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
    ("  … run   cargo check -p gents-cli", Tone::Warn),
    ("", Tone::Plain),
];

/// The sample turn's edits, as a source shows them.
pub fn changes() -> Vec<ChatLine> {
    let edit = |n: usize, path: &str, kind, diff: &str| ChatLine {
        text: format!("  ✓ edit  {path}"),
        tone: Tone::Label,
        item: Some(format!("sample:edit/{n}")),
        change: Some(Change::new(
            "sample:turn".to_string(),
            path.to_string(),
            kind,
            diff.to_string(),
        )),
    };
    vec![
        edit(
            0,
            "cli/args.rs",
            ChangeKind::Update,
            "@@ -85,8 +85,4 @@\n #[derive(Subcommand)]\n-CodexLogin(CodexLoginArgs),\n-GrokLogin(GrokLoginArgs),\n-ClaudeLogin(ClaudeLoginArgs),\n+Auth {\n+    command: AuthCommand,\n+},\n",
        ),
        edit(
            1,
            "cli/mod.rs",
            ChangeKind::Update,
            "@@ -12,7 +12,13 @@\n mod args;\n-mod login;\n+mod auth;\n+mod shell;\n",
        ),
        edit(
            2,
            "shell/mod.rs",
            ChangeKind::Add,
            "pub fn run() {\n    start();\n}\n",
        ),
    ]
}

/// Marks a subagent's reply. Its row in the running list uses the same glyph.
pub const SUBAGENT_MARK: &str = " ◆";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Working,
    Done,
    Failed,
}

/// What `enter` on a running row reaches: a subagent's session, which opens on its parent tab's
/// connection, or a job of the Workflows view, by its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opens {
    Agent(String),
    Workflow(String),
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
        Opens::Workflow("sample:change-unit".to_string()),
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

/// The sample session's approval, asked as `agent`.
pub(crate) fn approval(agent: &str) -> Prompt {
    Prompt {
        item: "sample:approval".to_string(),
        kind: PromptKind::Gate,
        title: format!("Allow {agent} to run `cargo test -p gents-cli`?"),
        summary: None,
        answer: Answer::Ack,
        dismissable: true,
    }
}

fn chat_lines(chat: Chat) -> Vec<ChatLine> {
    chat.iter()
        .map(|&(text, tone)| ChatLine {
            text: text.to_string(),
            tone,
            item: None,
            change: None,
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
            let (lines, rows, prompts) = match child {
                Some(&(.., chat)) => (chat_lines(chat), Vec::new(), Vec::new()),
                None => (sample_chat(), running(), vec![approval(agent)]),
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
                Update::Prompts {
                    session: session.clone(),
                    prompts,
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
                    change: None,
                };
                updates.push(Update::Item {
                    session: session.clone(),
                    line,
                });
            }
            updates
        }
        Intent::Dismiss { session, .. } => vec![Update::Prompts {
            session: session.clone(),
            prompts: Vec::new(),
        }],
        Intent::Reply { session, text, .. } => vec![
            Update::Prompts {
                session: session.clone(),
                prompts: Vec::new(),
            },
            Update::Item {
                session: session.clone(),
                line: ChatLine {
                    text: text.clone(),
                    tone: Tone::You,
                    item: None,
                    change: None,
                },
            },
        ],
        Intent::WorkingTree { .. } => vec![Update::WorkingTree(Ok(Vec::new()))],
        Intent::ReadMenu(id) => vec![Update::Menu {
            menu: *id,
            rows: Ok(menu_rows(*id)),
        }],
        Intent::ReadSetup => vec![Update::Setup(Ok(setup_catalog()))],
        Intent::SignIn { .. } => vec![
            Update::SignInUrl {
                url: "https://claude.ai/oauth/authorize?sample".to_string(),
            },
            Update::SignedIn(Ok(SavedAccount {
                label: "Claude 2".to_string(),
                account_ref: "sample-ref".to_string(),
            })),
        ],
        Intent::Discover(connection) => vec![Update::Models {
            connection: connection.clone(),
            found: Ok(Discovered {
                models: vec![
                    sample_model("claude-sonnet-4-5", "Claude Sonnet 4.5"),
                    sample_model("claude-opus-4-1", "Claude Opus 4.1"),
                ],
                failure: None,
                manual: false,
            }),
        }],
        Intent::Recommend { connection, model } => vec![Update::Recommended {
            connection: connection.clone(),
            model: Ok(sample_model(model, model)),
        }],
        Intent::SaveBackend { model, .. } => {
            vec![Update::Saved(Ok(format!("saved anthropic-2: {model}")))]
        }
        Intent::Act { .. } => vec![Update::Notice(NOT_WIRED.to_string())],
        Intent::Interrupt { .. } | Intent::Refresh | Intent::CancelSignIn => Vec::new(),
    }
}

fn sample_model(id: &str, name: &str) -> SetupModel {
    let number = |key, value: &str, min: &str, max: &str| SetupSetting {
        key,
        value: value.to_string(),
        min: Some(min.to_string()),
        max: Some(max.to_string()),
        choices: Vec::new(),
    };
    SetupModel {
        id: id.to_string(),
        name: name.to_string(),
        settings: vec![
            number(SettingKey::ContextWindow, "200000", "1024", "200000"),
            number(SettingKey::MaxOutputTokens, "64000", "1", "64000"),
            SetupSetting {
                key: SettingKey::ReasoningEffort,
                value: "medium".to_string(),
                min: None,
                max: None,
                choices: ["low", "medium", "high"].map(str::to_string).to_vec(),
            },
            number(SettingKey::MaxConcurrent, "4", "1", "64"),
        ],
    }
}

/// The desktop's providers, with one Claude account saved.
fn setup_catalog() -> Vec<SetupProvider> {
    use SetupAuth::{Key, OptionalKey, SignIn};
    let method = |id: &str, name: &str, endpoint: &str, auth| SetupMethod {
        id: id.to_string(),
        name: name.to_string(),
        endpoint: endpoint.to_string(),
        endpoint_fixed: false,
        auth,
        accounts: Vec::new(),
    };
    let provider = |id: &str, name: &str, about: &str, methods| SetupProvider {
        id: id.to_string(),
        name: name.to_string(),
        about: about.to_string(),
        methods,
    };
    let claude = SavedAccount {
        label: "Claude".to_string(),
        account_ref: "sample-ref-0".to_string(),
    };
    vec![
        provider(
            "openai",
            "OpenAI",
            "Sign in with ChatGPT or use an OpenAI API key.",
            vec![
                method(
                    "chat_gpt_oauth",
                    "ChatGPT sign-in",
                    "https://chatgpt.com/backend-api/codex",
                    SignIn,
                ),
                method(
                    "api_key",
                    "OpenAI API key",
                    "https://api.openai.com/v1",
                    Key,
                ),
            ],
        ),
        provider(
            "anthropic",
            "Anthropic",
            "Use a Claude Pro or Max subscription, or an Anthropic API key.",
            vec![
                SetupMethod {
                    accounts: vec![claude],
                    ..method(
                        "claude_oauth",
                        "Claude sign-in",
                        "claude-cli://subscription",
                        SignIn,
                    )
                },
                SetupMethod {
                    endpoint_fixed: true,
                    ..method(
                        "api_key",
                        "Anthropic API key",
                        "https://api.anthropic.com/v1",
                        Key,
                    )
                },
            ],
        ),
        provider(
            "grok",
            "Grok",
            "Use SuperGrok, an eligible X Premium+ subscription, or an xAI API key.",
            vec![
                method(
                    "grok_oauth",
                    "Grok sign-in",
                    "https://cli-chat-proxy.grok.com/v1",
                    SignIn,
                ),
                method("api_key", "xAI API key", "https://api.x.ai/v1", Key),
            ],
        ),
        provider(
            "local",
            "Local",
            "Ollama, llama.cpp, vLLM, or another OpenAI-compatible server.",
            vec![method(
                "optional_api_key",
                "Endpoint + optional key",
                "http://127.0.0.1:11434/v1",
                OptionalKey,
            )],
        ),
        provider(
            "openrouter",
            "OpenRouter",
            "Use one API key for OpenRouter's advertised models.",
            vec![method(
                "api_key",
                "OpenRouter API key",
                "https://openrouter.ai/api/v1",
                Key,
            )],
        ),
    ]
}

/// Today's static rows, which take no act but adding a provider, and two installed workflows.
fn menu_rows(id: MenuId) -> Vec<MenuRow> {
    let row = |text: &str, tone, enter| MenuRow {
        text: text.to_string(),
        tone,
        enter,
        d: None,
    };
    if id != MenuId::Workflows {
        let rows = menu(id).0.iter();
        return rows
            .map(|shown| {
                let add = (shown.text == ADD_PROVIDER).then_some(MenuAct::OpenSetup);
                row(&shown.text, shown.tone, add)
            })
            .collect();
    }
    [
        (
            " review_changes   Review a diff with three readers",
            "review_changes",
        ),
        (" release_notes   promoted", "release_notes"),
    ]
    .map(|(text, name)| {
        let prompt = format!("use the {name} workflow: ");
        row(text, Tone::Plain, Some(MenuAct::Insert { text: prompt }))
    })
    .into()
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

const ADD_PROVIDER: &str = " + Add a provider";

const ACCOUNTS: &[SidebarRow] = &[
    item(ADD_PROVIDER, Tone::Accent),
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
    row(
        " Workflows",
        Tone::Plain,
        RowAction::OpenMenu(MenuId::Workflows),
    ),
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
        MenuId::Accounts => (ACCOUNTS, " gents accounts login signs in · esc back"),
        MenuId::Agents => (AGENTS_MENU, " enter make default · esc back"),
        MenuId::Tools => (TOOLS, " esc back"),
        MenuId::Tasks => (TASKS, " enter run now · d on/off · esc back"),
        MenuId::Mailbox => (MAILBOX, " enter open its session · d dismiss · esc back"),
        MenuId::Status => (STATUS, " esc back"),
        MenuId::NodeSettings => (NODE_SETTINGS, " enter open · esc back"),
        MenuId::AppSettings => (APP_SETTINGS, " enter open · esc back"),
        MenuId::Workflows => (&[], " enter use it · esc back"),
    }
}

pub const KEY_ROWS: &[(&str, &str, &str)] = &[
    ("Normal and input modes", "i / esc", "always typing"),
    ("Move between panes", "h / l, ctrl-w h/j/k/l", "ctrl-x o"),
    ("Next or previous tab", "gt / gT", "ctrl-x n / ctrl-x p"),
    ("Open a tab", ":tabnew", "ctrl-x t"),
    ("Close the pane or tab", "ctrl-w q · :close", "ctrl-x 0"),
    ("Toggle the sessions sidebar", "[ or ctrl-w [", "ctrl-x ["),
    ("Toggle the right sidebar", "] or ctrl-w ]", "ctrl-x ]"),
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
    ("Send, or stop the turn", "enter · s", "enter · ctrl-c"),
    ("Quit", ":q then enter", "ctrl-x ctrl-c"),
    ("ctrl-c", "clears, or says how to quit", ""),
];

pub const DIFF_BACK_ROW: &str = " esc back to files";
