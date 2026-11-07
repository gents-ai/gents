use std::fmt;
use std::io;

use ratatui::crossterm::event::Event;

use crate::app::ChatLine;
use crate::keys::KeySet;
use crate::sample::{MenuId, Running, Tone};

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
    /// A menu is shown: the source answers its rows with `Update::Menu`.
    ReadMenu(MenuId),
    /// The profiles `agent` can run on: answered with `Update::Menu` for `MenuId::Profiles`.
    ReadProfiles {
        agent: String,
    },
    /// The key set the user switched to, for the next start.
    SaveKeys(KeySet),
    Act {
        menu: MenuId,
        act: MenuAct,
    },
    /// The Setup layer opens: the source answers its providers with `Update::Setup`.
    ReadSetup,
    /// The source tells the URL to open with `Update::SignInUrl`, then answers `Update::SignedIn`.
    /// With no label, the source picks one.
    SignIn {
        provider: String,
        method: String,
        label: Option<String>,
    },
    CancelSignIn,
    /// Answered with `Update::Models`.
    Discover(SetupConnection),
    /// The recommended settings for a model typed in: answered with `Update::Recommended`.
    Recommend {
        connection: SetupConnection,
        model: String,
    },
    /// Writes one backend and one profile: answered with `Update::Saved`.
    SaveBackend {
        connection: SetupConnection,
        model: String,
        settings: Vec<(SettingKey, String)>,
    },
    /// Answered with `Update::Form`.
    OpenForm(FormRef),
    /// The shown fields' keys and values, in order: answered with `Update::FormSaved`.
    SaveForm {
        form: FormRef,
        values: Vec<(String, String)>,
    },
    /// Runs one of the form's actions on its current values: answered with `Update::FormNote`.
    FormAct {
        form: FormRef,
        act: FormAct,
        values: Vec<(String, String)>,
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
    /// A menu's rows, replaced whole, or why they could not be read.
    Menu {
        menu: MenuId,
        rows: Result<Vec<MenuRow>, String>,
    },
    /// The providers the Setup layer offers, or why they could not be read.
    Setup(Result<Vec<SetupProvider>, String>),
    SignInUrl {
        url: String,
    },
    SignedIn(Result<SavedAccount, String>),
    /// Answers the `Discover` sent with `connection`.
    Models {
        connection: SetupConnection,
        found: Result<Discovered, String>,
    },
    /// Answers the `Recommend` sent with `connection`.
    Recommended {
        connection: SetupConnection,
        model: Result<SetupModel, String>,
    },
    /// The notice for what was saved, or why it was not.
    Saved(Result<String, String>),
    /// Answers `OpenForm`: the form to show, or why it could not be read.
    Form {
        form: FormRef,
        result: Result<Form, String>,
    },
    /// Answers `SaveForm`: the notice for what was saved, or why it was not.
    FormSaved {
        form: FormRef,
        result: Result<String, FormError>,
    },
    /// Answers `FormAct` with what the action did.
    FormNote {
        form: FormRef,
        text: String,
        tone: Tone,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuRow {
    pub text: String,
    pub tone: Tone,
    pub enter: Option<MenuAct>,
    pub d: Option<MenuAct>,
    /// What `.` offers, each with its label.
    pub more: Vec<(String, MenuAct)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAct {
    SetDefaultAgent {
        agent: String,
        name: String,
    },
    RunTask {
        task: String,
    },
    SetEnabled {
        doc: Doc,
        id: String,
        enabled: bool,
    },
    Duplicate(Doc, String),
    /// `access` is `read` or `read_write`.
    SetFolderAccess {
        path: String,
        access: String,
    },
    RemoveFolder(String),
    SetAgentProfile {
        agent: String,
        name: String,
        profile: String,
        label: String,
    },
    Dismiss {
        item: String,
    },
    /// Handled by the shell.
    Open(FormRef),
    /// Handled by the shell: asks for `name` typed before it deletes.
    ConfirmDelete {
        doc: Doc,
        id: String,
        label: String,
        name: String,
    },
    /// Handled by the shell: lists the profiles `agent` can run on.
    ChooseProfile {
        agent: String,
        name: String,
    },
    /// Handled by the shell.
    OpenSession {
        session: String,
    },
    /// Handled by the shell: the active tab's message box gets `text`.
    Insert {
        text: String,
    },
    /// Handled by the shell: opens the Setup layer.
    OpenSetup,
    /// The document of `Doc`'s kind with this id.
    Delete(Doc, String),
}

/// A kind of document a form edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Doc {
    Task,
    Skill,
    Trigger,
    Schedule,
    EventSource,
    RemoteTool,
}

impl Doc {
    /// The desktop's nouns (`ListDetail.tsx:503-516`).
    pub fn noun(self) -> &'static str {
        match self {
            Doc::Task => "task",
            Doc::Skill => "skill",
            Doc::Trigger => "trigger",
            Doc::Schedule => "schedule",
            Doc::EventSource => "event source",
            Doc::RemoteTool => "tool service",
        }
    }

    /// The menu that lists them.
    pub fn menu(self) -> MenuId {
        match self {
            Doc::Task => MenuId::Tasks,
            Doc::Skill => MenuId::Skills,
            Doc::Trigger => MenuId::Triggers,
            Doc::Schedule => MenuId::Schedules,
            Doc::EventSource => MenuId::EventSources,
            Doc::RemoteTool => MenuId::RemoteTools,
        }
    }
}

/// What a form is for. `Delete` is the shell's own confirmation, which the source never reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FormRef {
    New(Doc),
    Edit(Doc, String),
    Node,
    AddFolder,
    Delete(Doc, String),
}

/// A form the source describes for the shell to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub form: FormRef,
    pub title: String,
    pub note: Vec<String>,
    pub fields: Vec<FormField>,
    pub actions: Vec<FormAction>,
}

/// With `when`, the field shows only while the field `.0` holds one of `.1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub value: String,
    pub placeholder: String,
    pub help: Option<String>,
    pub when: Option<(String, Vec<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Lines,
    /// Its `(value, label)` pairs.
    Choice(Vec<(String, String)>),
    Fixed,
    Heading,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormAction {
    pub label: String,
    pub act: FormAct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormAct {
    RunSchedule,
    TestConnection,
    /// Opens the shell's confirmation, titled with `label`, which asks for `name` typed.
    Delete {
        label: String,
        name: String,
    },
}

/// A save's failure: under `field`'s value when it names one, else in the form's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormError {
    pub field: Option<String>,
    pub message: String,
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

/// An API key. Its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// `id` is the provider's serde name, as `openai` or `local`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupProvider {
    pub id: String,
    pub name: String,
    pub about: String,
    pub methods: Vec<SetupMethod>,
}

/// `id` is the method's serde name, as `claude_oauth` or `api_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupMethod {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub endpoint_fixed: bool,
    pub auth: SetupAuth,
    /// A sign-in's saved accounts.
    pub accounts: Vec<SavedAccount>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupAuth {
    SignIn,
    Key,
    OptionalKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAccount {
    pub label: String,
    pub account_ref: String,
}

/// `provider` and `method` are their ids, and `account` a saved account's `account_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupConnection {
    pub provider: String,
    pub method: String,
    pub endpoint: String,
    pub key: Option<Secret>,
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupModel {
    pub id: String,
    pub name: String,
    pub settings: Vec<SetupSetting>,
}

/// Numbers are formatted as text; a reasoning effort's `choices` are its serde names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupSetting {
    pub key: SettingKey,
    pub value: String,
    pub min: Option<String>,
    pub max: Option<String>,
    pub choices: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    ContextWindow,
    MaxOutputTokens,
    Temperature,
    TopP,
    ReasoningEffort,
    MaxConcurrent,
}

impl SettingKey {
    pub fn label(self) -> &'static str {
        match self {
            SettingKey::ContextWindow => "Context window",
            SettingKey::MaxOutputTokens => "Max output tokens",
            SettingKey::Temperature => "Temperature",
            SettingKey::TopP => "Top-p",
            SettingKey::ReasoningEffort => "Reasoning effort",
            SettingKey::MaxConcurrent => "Concurrency",
        }
    }

    pub fn whole(self) -> bool {
        matches!(
            self,
            SettingKey::ContextWindow | SettingKey::MaxOutputTokens | SettingKey::MaxConcurrent
        )
    }
}

/// With `manual`, the user may type a model id the provider did not advertise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub models: Vec<SetupModel>,
    pub failure: Option<String>,
    pub manual: bool,
}
