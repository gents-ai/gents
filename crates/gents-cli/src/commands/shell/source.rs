use std::future::Future;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use chrono::{DateTime, Local, Utc};
use gents_desktop_core::client::canonical_output::CanonicalLiveText;
use gents_desktop_core::client::{
    load_node_snapshot_on, load_session_compaction_entries_on, load_session_live_text_on,
    load_session_transcript_window_on, tool_file_changes_on, ClientStore, TranscriptWindow,
};
use gents_desktop_core::inference::{load_inference_setup_state, AgentView};
use gents_protocol::node_readiness::{
    AgentReadinessState, AgentReadinessUnavailableReason, NodeReadinessSnapshot,
};
use gents_protocol::operations::request::{
    MessageSendInput, RequestInterruptInput, RequestReceipt,
};
use gents_protocol::request_input::RequestInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::session::SessionScope;
use gents_session::session::mailbox::MailboxItem;
use gents_shell::app::ChatLine;
use gents_shell::keys::KeySet;
use gents_shell::sample::{MenuId, Opens, RunState, Running, Tone};
use gents_shell::source::{
    BackgroundCall, Change, Connection, Doc, FormRef, FormValues, Input, Intent, Job, MenuAct,
    PackAct, Prompt, SavedAccount, Session, Skill, Update,
};
use gents_store::config_client::{ConfigAccess, GraphqlSubscription, SubscriptionEnd};
use gents_store::graphql::escape_graphql_string;
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::sections::{self, new_id};
use super::tree::git_changes;
use super::work::{self, Work};
use super::{forms, git, items, menus, packs, prefs, setup, usage};
use crate::commands::accounts::account_rows;
use crate::commands::config::crud::{query_collection_sorted, SKILL_SPEC};
use crate::commands::mailbox;
use gents_session::session::caused::{load_caused_sessions_as, CausedSession};

/// What the shell opens with. `work` is why the shell has no runtime when it is `Err`.
pub(super) struct Opening {
    /// This home's node, from its setup.
    pub(super) node: String,
    pub(super) agent: Option<String>,
    pub(super) prompt: Option<String>,
    /// `gents --continue`'s session, which `agent` then names the agent of.
    pub(super) session: Option<String>,
    pub(super) work: Result<Work, String>,
    pub(super) home: PathBuf,
    /// Where the branch is read.
    pub(super) cwd: Option<PathBuf>,
    /// Where the Working Tree is read: the home's tool root, else `cwd`.
    pub(super) tree: Option<PathBuf>,
}

/// Feeds the shell from this home's runtime over its signed GraphQL endpoint. A given
/// `agent` or `prompt` follows the agents.
///
/// Each intent is answered before the next change is read. The shell drops a tab's updates
/// until `Started` binds its session, and `Opened` replaces a tab's lines, so both must reach
/// it before any change read while their reads were out.
pub(super) async fn run(
    opening: Opening,
    mut intents: mpsc::UnboundedReceiver<Intent>,
    updates: Sender<Input>,
    runtime: Handle,
) {
    let Opening {
        node,
        agent,
        prompt,
        session,
        work,
        home,
        cwd,
        tree,
    } = opening;
    let (watching, mut watched) = mpsc::unbounded_channel();
    let mut source = Source::new(updates, work, home, runtime.clone(), watching);
    source.principal = node;
    source.tree = tree;
    let work = match source.work.clone() {
        Ok(work) => work,
        Err(off) => return source.lost(no_runtime(&off)),
    };
    let AgentsRead { changes, read } = read_agents(work.clone(), &source.principal, true).await;
    let read = match read {
        Ok(read) => read,
        Err(error) => return source.lost(no_runtime(&error)),
    };
    source.readiness_watched(changes);
    // `Begin` or `Continue` must reach the shell before a send the user makes once the agents
    // are known. Both go through one queue, so it is told before the sweep awaits anything.
    source.begin = match session {
        Some(session) => Some(Update::Continue { session, agent }),
        None if agent.is_some() || prompt.is_some() => Some(Update::Begin { agent, prompt }),
        None => None,
    };
    // A tab that opens on another agent than the default says why, unless one was asked for.
    let elsewhere = read.unready.clone().filter(|_| source.begin.is_none());
    source.take_agents(read);
    if let Some(why) = elsewhere.filter(|_| !source.shown_agents.is_empty()) {
        source.tell(Update::Notice(why));
    }
    // Read beside startup, never ahead of it: git may hang for its timeout. Once only, so a
    // checkout during the session shows at the next start.
    if let Some(cwd) = cwd {
        let updates = source.updates.clone();
        source.spawn(async move {
            if let Some(branch) = git::branch(&cwd).await {
                let _ = updates.send(Input::Runtime(Update::Branch(branch)));
            }
        });
    }
    // The node replays nothing, so the feed is open before the first read.
    let mut failed = None;
    for collection in CHAT {
        let changes = watch(&work, &chat_document(collection, &source.principal), true).await;
        if let Some(error) = source.chat_watched(collection, changes) {
            failed.get_or_insert(error);
        }
    }
    if let Some(error) = failed {
        source.tell(Update::Notice(chat_not_live(&error)));
    }
    match load_node_snapshot_on(&work.access(), &source.principal).await {
        Ok(store) => source.store = Arc::new(store),
        Err(error) => source.tell(Update::Notice(format!("{error:#}"))),
    }
    source.listed = items::sessions(&source.store, &source.principal, today());
    source.tell(Update::Sessions(source.listed.clone()));
    source.reread_work();
    loop {
        tokio::select! {
            intent = intents.recv() => match intent {
                Some(intent) => source.handle(intent).await,
                None => break,
            },
            Some(event) = watched.recv() => match event {
                Watched::Ended(Feed::Chat(_), error) => return source.lost(closed(error)),
                event => source.watched(event),
            },
        }
    }
    // The feeds end once nothing reads them, and a waiting sign-in once cancelled; the rest
    // end with their reads.
    drop(watched);
    if let Some(waiting) = source.signing_in.take() {
        waiting.cancel();
    }
    source.tasks.close();
    source.tasks.wait().await;
}

/// The node's enabled agents, and those of them its readiness says are ready, as the shell
/// lists them.
struct Runnable {
    views: Vec<AgentView>,
    agents: Vec<gents_shell::source::Agent>,
    /// Why the node's default agent cannot run, while it cannot.
    unready: Option<String>,
}

async fn runnable(access: &ConfigAccess, node: &str) -> Result<Runnable, String> {
    let setup = load_inference_setup_state(access, node).await;
    let views = setup.map_err(|error| format!("{error:#}"))?.agents;
    let readiness = crate::commands::start::node_readiness(access, node).await;
    let readiness = readiness.map_err(|error| format!("{error:#}"))?;
    let unready = unready(&views, readiness.as_ref());
    let ready = readiness.into_iter().flat_map(|snapshot| snapshot.agents);
    let ready = ready.filter(|entry| entry.state == AgentReadinessState::Ready);
    let ready: Vec<_> = ready.map(|entry| entry.agent_did).collect();
    let agents = items::agents(&views, &ready);
    Ok(Runnable {
        views,
        agents,
        unready,
    })
}

/// Why the node's default agent cannot run, in one line, from the reason its runtime
/// published; nothing once it can. The Setup layer and the Accounts menu connect a model.
pub(super) fn unready(
    views: &[AgentView],
    readiness: Option<&NodeReadinessSnapshot>,
) -> Option<String> {
    use AgentReadinessUnavailableReason::BackendTemporarilyUnavailable as Unreachable;
    let default = match readiness {
        Some(snapshot) => views
            .iter()
            .find(|view| view.agent_did == snapshot.default_agent_did),
        None => views.iter().find(|view| view.is_default),
    };
    let published = readiness.zip(default).and_then(|(snapshot, default)| {
        let entries = snapshot.agents.iter();
        entries
            .into_iter()
            .find(|entry| entry.agent_did == default.agent_did)
    });
    if published.is_some_and(|entry| entry.state == AgentReadinessState::Ready) {
        return None;
    }
    let name = default.map_or_else(|| "The default agent".to_string(), items::agent_name);
    Some(match published.and_then(|entry| entry.reason) {
        Some(Unreachable) => format!(
            "{name}'s model cannot be reached, so it cannot answer. Start it, or add a provider."
        ),
        Some(reason) if crate::commands::start::wants_a_model(reason) => format!(
            "No model is connected yet, so {name} cannot answer. Add a provider to connect one."
        ),
        Some(reason) => format!("{name} cannot answer yet: {}.", reason.public_message()),
        None => format!("{name} cannot answer yet."),
    })
}

/// One read of the agents that can run, done off the chat loop.
struct AgentsRead {
    /// The readiness subscription it opened first, when the source was not live.
    changes: Option<Result<Changes, String>>,
    read: Result<Runnable, String>,
}

/// A runtime publishes which agents can run a little after their configuration changes, so
/// its readiness is watched as the chat's documents are: each event is a hint to read again.
async fn read_agents(work: Work, node: &str, subscribe: bool) -> AgentsRead {
    let document = chat_document("NodeReadiness", node);
    let changes = watch(&work, &document, subscribe).await;
    let read = runnable(&work.access(), node).await;
    AgentsRead { changes, read }
}

/// The status bar clips on the right, so the next step leads and the detail ends.
fn no_runtime(why: &str) -> String {
    format!("no runtime: check `gents status`, then reopen the shell ({why})")
}

fn today() -> chrono::NaiveDate {
    Local::now().date_naive()
}

/// The tool calls that are plugin jobs or background calls. Each event is only a hint to
/// reread: the node drops events when a subscriber falls behind, and a delete reads like an
/// update.
pub(super) const WATCHED: &str = r#"subscription { AgentToolCall(filter: { _or: [{ job_pin: { _ne: null } }, { await_mode: { _eq: "background" } }] }) { _docID } }"#;

/// Any change to an item a prompt shows: an item's kind never changes, so one leaving the open
/// prompts still fires. Each event is only a hint to reread them.
pub(super) const MAILBOX_WATCHED: &str =
    r#"subscription { MailboxItem(filter: { kind: { _in: ["ask", "gate"] } }) { _docID } }"#;

/// The node's documents the open sessions show, each watched as the desktop watches them: an
/// event is only a hint to reread.
pub(super) const CHAT: [&str; 5] = [
    "AgentRequest",
    "AgentSession",
    "AgentMessage",
    "AgentToolCall",
    "AgentOutputSegment",
];

pub(super) fn chat_document(collection: &str, node: &str) -> String {
    let node = escape_graphql_string(node);
    format!(
        r#"subscription {{ {collection}(filter: {{ node_did: {{ _eq: "{node}" }} }}) {{ _docID }} }}"#
    )
}

/// What a change to `collection` makes stale: requests and sessions change the list, the
/// running turns and the subagents, messages and tool calls the transcripts. A running
/// turn's live text is read on every change.
fn stale(collection: &str) -> Stale {
    match collection {
        "AgentRequest" | "AgentSession" => Stale {
            sessions: true,
            transcripts: false,
        },
        "AgentMessage" | "AgentToolCall" => Stale {
            sessions: false,
            transcripts: true,
        },
        _ => Stale::default(),
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Stale {
    sessions: bool,
    transcripts: bool,
}

impl Stale {
    fn and(self, other: Stale) -> Stale {
        Stale {
            sessions: self.sessions || other.sessions,
            transcripts: self.transcripts || other.transcripts,
        }
    }
}

type Changes = GraphqlSubscription;

/// What a subscription watches: the work views' tool calls, the mailbox, the node's readiness,
/// or one of the chat's collections.
#[derive(Clone, Copy)]
enum Feed {
    Work,
    Mailbox,
    Readiness,
    Chat(&'static str),
}

enum Watched {
    Changed(Feed),
    /// With the error, when the stream broke rather than closed.
    Ended(Feed, Option<String>),
    /// The subscription's bearer is due for renewal: subscribe again and reread.
    Renew(Feed),
    Read(Box<Read>),
    Asked(Box<Asked>),
    Chatted(Box<ChatRead>),
    Tree(Result<Vec<Change>, String>),
    /// A sign-in's answer, with the token `CancelSignIn` cancels for it.
    SignedIn(CancellationToken, Result<SavedAccount, String>),
    /// A pack change succeeded, so Packs is read again with the search shown by then.
    PackChanged,
    /// A setup, menu or form action ended that may change which agents can run.
    Configured,
    Agents(Box<AgentsRead>),
}

/// One reread of the work views, done off the chat loop.
struct Read {
    /// The subscription it opened first, when the source was not live.
    changes: Option<Result<Changes, String>>,
    jobs: Result<Vec<Job>, String>,
    background: Result<Vec<BackgroundCall>, String>,
    at: DateTime<Local>,
}

/// One reread of this node's open mailbox items, done off the chat loop.
struct Asked {
    changes: Option<Result<Changes, String>>,
    items: Result<Vec<MailboxItem>, String>,
}

/// The node replays nothing, so a change between a read and the subscription opened after it
/// would be lost: each read subscribes first when asked.
async fn watch(work: &Work, document: &str, subscribe: bool) -> Option<Result<Changes, String>> {
    let changes = match subscribe {
        true => work.access().subscribe(document).await,
        false => return None,
    };
    Some(changes.map_err(|error| format!("{error:#}")))
}

/// The jobs of `sessions` only, and every background call.
async fn read_work(work: Work, subscribe: bool, sessions: Vec<String>) -> Read {
    let changes = watch(&work, WATCHED, subscribe).await;
    let at = Local::now();
    let access = work.access();
    let jobs = work::jobs(&access, &sessions)
        .await
        .map(|views| views.iter().map(work::job).collect())
        .map_err(|error| error.to_string());
    let background = work::background(&access)
        .await
        .map(|rows| rows.iter().map(work::background_call).collect())
        .map_err(|error| format!("{error:#}"));
    Read {
        changes,
        jobs,
        background,
        at,
    }
}

async fn read_mailbox(work: Work, subscribe: bool) -> Asked {
    let changes = watch(&work, MAILBOX_WATCHED, subscribe).await;
    let items = gents_session::operations::mailbox::list(&work.access(), &Default::default())
        .await
        .map_err(|error| format!("{:#}", mailbox::failure(error)));
    Asked { changes, items }
}

/// A view's last good rows, when they were read, and whether the latest read failed.
struct Shown<T> {
    rows: Vec<T>,
    read: Option<DateTime<Local>>,
    failed: bool,
}

impl<T> Shown<T> {
    fn new() -> Self {
        Shown {
            rows: Vec::new(),
            read: None,
            failed: false,
        }
    }

    /// A failed read keeps the last good rows, and returns its error.
    fn take(&mut self, read: Result<Vec<T>, String>, at: DateTime<Local>) -> Option<String> {
        match read {
            Ok(rows) => {
                (self.rows, self.read, self.failed) = (rows, Some(at), false);
                None
            }
            Err(error) => {
                self.failed = true;
                Some(error)
            }
        }
    }
}

/// Ends when the source does, which drops the subscription and its connection.
async fn forward(mut changes: Changes, feed: Feed, watching: mpsc::UnboundedSender<Watched>) {
    let ended = loop {
        tokio::select! {
            change = changes.recv() => match change {
                Ok(Some(_)) => {
                    let _ = watching.send(Watched::Changed(feed));
                }
                Ok(None) if changes.end() == Some(SubscriptionEnd::RenewalDue) => {
                    let _ = watching.send(Watched::Renew(feed));
                    return;
                }
                Ok(None) => break None,
                Err(error) => break Some(format!("{error:#}")),
            },
            () = watching.closed() => return,
        }
    };
    let _ = watching.send(Watched::Ended(feed, ended));
}

fn not_live(error: &str) -> String {
    format!("jobs are not live: show the Workflows view to read them again ({error})")
}

/// Prompts have no header to show their age, so the end of their feed is told.
fn prompts_not_live(error: Option<&str>) -> String {
    let error = error.map_or(String::new(), |error| format!(" ({error})"));
    format!("prompts are not live: the next turn, or opening a session, reads them again{error}")
}

/// Each native file edit's recorded changes, by its tool call: `None` once read and found
/// unrecorded. A recorded edit never changes, so each is read once.
type Edits = RapidHashMap<String, Option<Vec<Change>>>;

/// One reread of the open sessions, done off the chat loop.
struct ChatRead {
    /// The chat subscriptions it opened first, those that were not live.
    changes: Vec<(&'static str, Option<Result<Changes, String>>)>,
    /// The node's sessions and requests, when they were stale.
    store: Option<Result<ClientStore, String>>,
    sessions: Vec<SessionRead>,
}

/// What a reread asks of one open session.
struct Asking {
    session: String,
    requester: Option<String>,
    /// Its tab's opening the read is for: a read that lands after the tab opened again is
    /// older than what the opening told.
    opened: u64,
    /// The sequence its next message takes, when its transcript is not read again.
    next: u32,
}

struct SessionRead {
    session: String,
    opened: u64,
    transcript: Option<Result<items::Transcript, String>>,
    /// The edits the transcript read found, to remember.
    edits: Vec<(String, Option<Vec<Change>>)>,
    live: Result<Vec<ChatLine>, String>,
    children: Option<Result<Vec<CausedSession>, String>>,
}

async fn read_chat(
    work: Work,
    node: String,
    stale: Stale,
    subscribe: Vec<&'static str>,
    observed: Arc<ClientStore>,
    asking: Vec<Asking>,
    edits: Edits,
) -> ChatRead {
    let mut changes = Vec::with_capacity(subscribe.len());
    for collection in subscribe {
        let document = chat_document(collection, &node);
        changes.push((collection, watch(&work, &document, true).await));
    }
    let access = work.access();
    let store = match stale.sessions {
        true => Some(load_node_snapshot_on(&access, &node).await),
        false => None,
    };
    let store = store.map(|read| read.map_err(|error| format!("{error:#}")));
    let observed = match &store {
        Some(Ok(store)) => store,
        _ => observed.as_ref(),
    };
    let mut sessions = Vec::with_capacity(asking.len());
    for asked in asking {
        sessions.push(read_session(&access, &node, observed, asked, stale, &edits).await);
    }
    ChatRead {
        changes,
        store,
        sessions,
    }
}

/// `stale` says what to read beside the live text of a running turn: the transcript, and the
/// subagents with the sessions.
async fn read_session(
    access: &ConfigAccess,
    node: &str,
    observed: &ClientStore,
    asked: Asking,
    stale: Stale,
    edits: &Edits,
) -> SessionRead {
    let Asking {
        session,
        requester,
        opened,
        mut next,
    } = asked;
    let requester = requester.as_deref();
    let (transcript, edits) = match stale.transcripts {
        true => match read_transcript(access, node, &session, requester, edits).await {
            Ok((transcript, edits)) => {
                next = transcript.next;
                (Some(Ok(transcript)), edits)
            }
            Err(error) => (Some(Err(error)), Vec::new()),
        },
        false => (None, Vec::new()),
    };
    let running = observed.derive_turn_for_node(&session, node);
    let live = match running.is_some_and(|state| !state.is_terminal()) {
        true => load_session_live_text_on(access, observed, &session, node).await,
        false => Ok(None),
    };
    let live = live.map_err(|error| format!("{error:#}")).map(|live| {
        let shown = live.as_ref().map(
            |CanonicalLiveText {
                 reasoning, content, ..
             }| { items::live(next, reasoning, content) },
        );
        shown.unwrap_or_default()
    });
    let children = match stale.sessions {
        true => {
            let scope = SessionScope {
                node_did: node.to_string(),
                session_id: session.clone(),
                requester_did: requester.map(str::to_string),
            };
            let caused = load_caused_sessions_as(access, &[scope]).await;
            Some(caused.map_err(|error| format!("{error:#}")))
        }
        false => None,
    };
    SessionRead {
        session,
        opened,
        transcript,
        edits,
        live,
        children,
    }
}

/// The session's newest page, with each finished file edit's recorded changes read once.
async fn read_transcript(
    access: &ConfigAccess,
    node: &str,
    session: &str,
    requester: Option<&str>,
    known: &Edits,
) -> Result<(items::Transcript, Vec<(String, Option<Vec<Change>>)>), String> {
    let failed = |error: anyhow::Error| format!("{error:#}");
    let (node_did, window) = (Some(node), TranscriptWindow::Tip);
    let page =
        load_session_transcript_window_on(access, session, node_did, requester, window, None);
    let page = page.await.map_err(failed)?;
    let compactions = load_session_compaction_entries_on(access, session, node, requester);
    let compactions = compactions.await.map_err(failed)?;
    let mut read = Vec::new();
    for (tool, request) in items::finished_edits(&page, session) {
        if known.contains_key(&tool) {
            continue;
        }
        // A failed read is read again with the next change.
        if let Ok(recorded) = tool_file_changes_on(access, node, session, &request, &tool).await {
            let changes = recorded.map(|recorded| items::changes(&recorded, &request));
            read.push((tool, changes));
        }
    }
    let mut edits = known.clone();
    edits.extend(read.iter().cloned());
    let transcript = items::transcript(&page, &compactions, session, &edits);
    Ok((transcript, read))
}

/// A pack change asked for while another runs.
const BUSY: &str = "A pack change is still running";

/// The status bar clips on the right, so the next step leads and the detail ends.
pub(super) fn closed(error: Option<String>) -> String {
    let error = error.map_or(String::new(), |error| format!(" ({error})"));
    format!("runtime closed: check `gents status`, then reopen the shell{error}")
}

/// What an open session's tab was last told.
#[derive(Default)]
struct Chat {
    /// Each line by its item id.
    lines: RapidHashMap<String, ChatLine>,
    /// The sequence its next message takes, which a running turn's live text is shown under.
    next: u32,
    /// The running request the tab shows as its turn.
    turn: Option<String>,
    /// The session's requester, which scopes its transcript.
    requester: Option<String>,
    /// Counts the tab's openings: each replaces its lines.
    opened: u64,
}

struct Source {
    updates: Sender<Input>,
    /// Sessions a tab has opened or sent to.
    open: RapidHashSet<String>,
    /// What each of `open` was last told.
    chats: RapidHashMap<String, Chat>,
    /// This node's agents as setup lists them, which names subagents.
    agents: Vec<AgentView>,
    /// Every agent the shell was ever told, which a turn may go to: its runtime answers a
    /// turn it can no longer run with why.
    shown_agents: Vec<String>,
    /// The agents the shell was last told, in order: its picker lists exactly these, so it
    /// is told again whenever those that can run change, not only when one is new.
    told_agents: Vec<String>,
    /// Why no agent can run, as the shell was last told, while none is listed.
    unready: Option<String>,
    /// What `gents` was opened with, told once after the first agents.
    begin: Option<Update>,
    /// Whether the readiness subscription is open.
    readiness_live: bool,
    /// A read of the agents is out, and `agents_again` asks for one more when it lands.
    reading_agents: bool,
    agents_again: bool,
    /// The node's sessions and requests as last read, which the running turns are found in.
    store: Arc<ClientStore>,
    /// The sessions list last told.
    listed: Vec<Session>,
    /// The chat collections whose subscription is open.
    chat_live: RapidHashSet<&'static str>,
    /// A chat reread is out, and `chat_again` is what one more asked for meanwhile reads.
    chatting: bool,
    chat_again: Option<Stale>,
    edits: Edits,
    /// Requests the user stopped: one still running until the runtime stops it is not shown
    /// as the tab's turn again.
    interrupted: RapidHashSet<String>,
    /// Each listed subagent's parent session.
    parents: RapidHashMap<String, String>,
    /// Each listed session's subagent rows, which its running rows lead with.
    children: RapidHashMap<String, Vec<Running>>,
    work: Result<Work, String>,
    /// This home, whose installed workflows a menu lists even when `work` is off.
    home: PathBuf,
    /// Where the Working Tree is read.
    tree: Option<PathBuf>,
    runtime: Handle,
    /// Everything the source starts, which the source's end waits for.
    tasks: TaskTracker,
    watching: mpsc::UnboundedSender<Watched>,
    /// Whether the tool-call subscription is open, so the views need no reread until it says.
    live: bool,
    /// A reread is out, and `again` asks for one more when it lands.
    reading: bool,
    again: bool,
    jobs: Shown<Job>,
    background: Shown<BackgroundCall>,
    /// This home's node: the sessions' node, the mailbox's requester and replier.
    principal: String,
    /// Whether the mailbox subscription is open, so the prompts need no reread until it says.
    mailbox_live: bool,
    /// A mailbox read is out, and `ask_again` asks for one more when it lands.
    asking: bool,
    ask_again: bool,
    /// A working-tree read is out, and `tree_again` asks for one more meanwhile.
    reading_tree: bool,
    tree_again: bool,
    /// The prompts each open session was last told.
    told: RapidHashMap<String, Vec<Prompt>>,
    /// Listed subagents with an open question or approval, which their parent's row names.
    waiting: RapidHashSet<String>,
    /// The waiting sign-in's token, which `CancelSignIn` takes and cancels. Its answer clears it.
    signing_in: Option<CancellationToken>,
    /// The id each kind's new form last opened with.
    new_ids: RapidHashMap<Doc, String>,
    /// The registry search Packs shows: what was searched for, and the page shown.
    pack_query: String,
    pack_page: u32,
    /// Held through each pack change, so another is refused while it runs.
    pack_changes: Arc<tokio::sync::Mutex<()>>,
    /// Each agent's last listed skills, whose ids a skill turn may send.
    skills: RapidHashMap<String, Vec<String>>,
}

impl Source {
    fn new(
        updates: Sender<Input>,
        work: Result<Work, String>,
        home: PathBuf,
        runtime: Handle,
        watching: mpsc::UnboundedSender<Watched>,
    ) -> Source {
        Source {
            updates,
            open: RapidHashSet::new(),
            chats: RapidHashMap::new(),
            agents: Vec::new(),
            shown_agents: Vec::new(),
            told_agents: Vec::new(),
            unready: None,
            begin: None,
            readiness_live: false,
            reading_agents: false,
            agents_again: false,
            store: Arc::default(),
            listed: Vec::new(),
            chat_live: RapidHashSet::new(),
            chatting: false,
            chat_again: None,
            edits: RapidHashMap::new(),
            interrupted: RapidHashSet::new(),
            parents: RapidHashMap::new(),
            children: RapidHashMap::new(),
            work,
            home,
            tree: None,
            runtime,
            tasks: TaskTracker::new(),
            watching,
            live: false,
            reading: false,
            again: false,
            jobs: Shown::new(),
            background: Shown::new(),
            principal: String::new(),
            mailbox_live: false,
            asking: false,
            ask_again: false,
            reading_tree: false,
            tree_again: false,
            told: RapidHashMap::new(),
            waiting: RapidHashSet::new(),
            signing_in: None,
            new_ids: RapidHashMap::new(),
            pack_query: String::new(),
            pack_page: 1,
            pack_changes: Arc::default(),
            skills: RapidHashMap::new(),
        }
    }

    fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.tasks.spawn_on(task, &self.runtime);
    }

    /// The shell aborts the source when it ends, so an update it no longer reads is dropped.
    fn tell(&self, update: Update) {
        let _ = self.updates.send(Input::Runtime(update));
    }

    fn lost(&self, why: String) {
        self.tell(Update::Connection(Connection::Lost(why)));
    }

    fn readiness_watched(&mut self, changes: Option<Result<Changes, String>>) {
        // A subscription that failed is opened again by the next read.
        if let Some(Ok(changes)) = changes {
            self.readiness_live = true;
            let watching = self.watching.clone();
            self.spawn(forward(changes, Feed::Readiness, watching));
        }
    }

    /// With none that can run, the shell is told why, and again when the reason changes. With
    /// some, it is told them whenever they are not the ones it was last told; the first are
    /// followed by what `gents` was opened with and `Ready`.
    fn take_agents(&mut self, read: Runnable) {
        let Runnable {
            views,
            agents,
            unready,
        } = read;
        self.agents = views;
        if agents.is_empty() {
            let why = unready.unwrap_or_else(|| "No agent can run yet on this node.".to_string());
            if self.shown_agents.is_empty() && self.unready.as_ref() != Some(&why) {
                self.unready = Some(why.clone());
                self.tell(Update::Connection(Connection::NoAgent(why)));
            }
            return;
        }
        let ids: Vec<String> = agents.iter().map(|agent| agent.id.clone()).collect();
        if ids == self.told_agents {
            return;
        }
        let first = self.shown_agents.is_empty();
        for id in &ids {
            if !self.shown_agents.contains(id) {
                self.shown_agents.push(id.clone());
            }
        }
        self.told_agents = ids;
        let woke = self.unready.take().map(|_| agents[0].name.clone());
        self.tell(Update::Agents(agents));
        if first {
            if let Some(begin) = self.begin.take() {
                self.tell(begin);
            }
            self.tell(Update::Connection(Connection::Ready));
        }
        if let Some(name) = woke {
            self.tell(Update::Notice(format!("{name} is ready. Type a message.")));
        }
    }

    /// One read at a time, off the chat loop, as `reread_work` reads.
    fn reread_agents(&mut self) {
        let Ok(work) = self.work.clone() else {
            return;
        };
        if self.reading_agents {
            self.agents_again = true;
            return;
        }
        self.reading_agents = true;
        let (node, subscribe) = (self.principal.clone(), !self.readiness_live);
        let done = self.watching.clone();
        self.spawn(async move {
            let read = read_agents(work, &node, subscribe).await;
            let _ = done.send(Watched::Agents(Box::new(read)));
        });
    }

    /// A read that failed keeps what the shell lists; the next change reads again.
    fn agents_read(&mut self, read: AgentsRead) {
        self.reading_agents = false;
        self.readiness_watched(read.changes);
        if let Ok(read) = read.read {
            self.take_agents(read);
        }
        if std::mem::take(&mut self.agents_again) {
            self.reread_agents();
        }
    }

    async fn handle(&mut self, intent: Intent) {
        match intent {
            Intent::Open { session, .. } => self.open(session).await,
            Intent::Send {
                tab,
                session,
                agent,
                text,
            } => self.send(tab, session, &agent, text, Vec::new()).await,
            Intent::ReadSkills { agent } => self.read_skills(agent).await,
            Intent::SendSkill {
                tab,
                session,
                agent,
                skill,
                text,
            } => {
                let listed = self.skills.get(&agent).into_iter().flatten();
                if !listed.into_iter().any(|listed| *listed == skill) {
                    let message = format!("unknown skill /{skill}");
                    return self.tell(Update::StartFailed { tab, message });
                }
                self.send(tab, session, &agent, text, vec![skill]).await
            }
            Intent::SetGoal {
                tab,
                session,
                agent,
                objective,
            } => self.goal(tab, session, &agent, objective).await,
            Intent::Interrupt { session, turn, .. } => self.interrupt(session, turn).await,
            Intent::Refresh => self.reread_work(),
            Intent::Dismiss { item, .. } => self.dismiss(&item).await,
            Intent::Reply { item, text, .. } => self.reply(&item, &text).await,
            Intent::WorkingTree { .. } => self.read_tree(),
            Intent::ReadMenu(menu) => self.read_menu(menu),
            Intent::ReadProfiles { agent } => self.read_profiles(agent),
            Intent::SaveKeys(keys) => self.save_keys(keys),
            Intent::Act {
                act: MenuAct::Pack(act),
                ..
            } => self.pack_act(act),
            Intent::Act {
                act: MenuAct::RefreshUsage,
                ..
            } => self.refresh_usage(),
            Intent::Act { menu, act } => self.act(menu, act),
            Intent::ReadSetup => self.answer_setup(Update::Setup, |access, node| async move {
                let rows = account_rows(&access, &node, None, Utc::now()).await;
                rows.map(|rows| setup::catalog(&rows))
                    .map_err(|error| format!("{error:#}"))
            }),
            Intent::SignIn {
                provider,
                method,
                label,
            } => self.sign_in(provider, method, label),
            Intent::CancelSignIn => {
                if let Some(waiting) = self.signing_in.take() {
                    waiting.cancel();
                }
            }
            Intent::Discover(connection) => {
                let asked = connection.clone();
                let models = move |found| Update::Models {
                    connection: asked,
                    found,
                };
                self.answer_setup(models, |access, node| async move {
                    setup::discover(&access, &node, &connection).await
                });
            }
            Intent::Recommend { connection, model } => {
                let model = setup::recommend(&connection, &model);
                self.tell(Update::Recommended { connection, model });
            }
            Intent::SaveBackend {
                connection,
                model,
                settings,
            } => {
                // A save binds no agent, so while none can run the default agent takes the
                // model it adds: nothing else would wake the shell.
                let default = self.agents.iter().find(|view| view.is_default);
                let waiting = default.filter(|_| self.shown_agents.is_empty());
                let waiting = waiting.map(|view| (view.agent_did.clone(), items::agent_name(view)));
                let (work, home) = (self.work.clone(), self.home.clone());
                self.answer_change(Update::Saved, |access, node| async move {
                    let saved = setup::save(&access, &node, &connection, &model, &settings).await?;
                    let (Some((agent, name)), Ok(work)) = (waiting, work) else {
                        return Ok(saved.notice);
                    };
                    let notice = saved.notice;
                    let bind = MenuAct::SetAgentProfile {
                        agent,
                        name: name.clone(),
                        profile: saved.profile,
                        label: String::new(),
                    };
                    Ok(match menus::act(&work, &home, &node, &bind).await {
                        Ok(_) => format!("{notice}, now {name}'s model"),
                        Err(why) => format!("{notice}. Choose it for {name} under Agents ({why})"),
                    })
                });
            }
            Intent::OpenForm {
                form: FormRef::PackSearch,
                opening,
            } => self.tell(Update::Form {
                form: FormRef::PackSearch,
                opening,
                result: Ok(packs::search_form(&self.pack_query)),
            }),
            // The desktop says nothing of a search, and the shell rereads Packs on its save.
            Intent::SaveForm {
                form: FormRef::PackSearch,
                opening,
                values,
            } => {
                self.pack_query = forms::value(&values.0, "query").trim().to_string();
                self.pack_page = 1;
                self.tell(Update::FormSaved {
                    form: FormRef::PackSearch,
                    opening,
                    result: Ok(String::new()),
                });
            }
            Intent::OpenForm {
                form: FormRef::PackSignIn,
                opening,
            } => self.tell(Update::Form {
                form: FormRef::PackSignIn,
                opening,
                result: Ok(packs::sign_in_form()),
            }),
            Intent::SaveForm {
                form: FormRef::PackSignIn,
                opening,
                values,
            } => self.registry_sign_in(opening, &values),
            Intent::OpenForm { form, opening } => {
                if let FormRef::New(doc) = form {
                    self.new_ids.insert(doc, new_id(sections::prefix(doc)));
                }
                let (id, asked) = (self.form_id(&form), form.clone());
                let opened = move |result| Update::Form {
                    form: asked,
                    opening,
                    result,
                };
                self.answer_setup(opened, |access, node| async move {
                    let opened = forms::open(&access, &node, &form, &id).await;
                    opened.map_err(|error| format!("{error:#}"))
                });
            }
            Intent::SaveForm {
                form,
                opening,
                values,
            } => {
                let (id, asked) = (self.form_id(&form), form.clone());
                let home = self.home.clone();
                let saved = move |saved: Result<_, String>| Update::FormSaved {
                    form: asked,
                    opening,
                    result: saved.unwrap_or_else(|off| Err(forms::refused(off))),
                };
                self.answer_change(saved, |access, node| async move {
                    Ok(forms::save(&access, &home, &node, &form, &values.0, &id).await)
                });
            }
            Intent::FormAct {
                form,
                opening,
                act,
                values,
            } => {
                let (id, home, asked) = (self.form_id(&form), self.home.clone(), form.clone());
                let noted = move |noted: Result<_, String>| {
                    let (text, tone) = noted.unwrap_or_else(|off| (off, Tone::Warn));
                    Update::FormNote {
                        form: asked,
                        opening,
                        text,
                        tone,
                    }
                };
                self.answer_setup(noted, |access, node| async move {
                    Ok(forms::act(&access, &home, &node, &form, &act, &values.0, &id).await)
                });
            }
        }
    }

    /// The id `form`'s document saves under. A new document's is made when its form opens and
    /// kept for its save, since the shell sends no fixed field's value.
    fn form_id(&self, form: &FormRef) -> String {
        match form {
            FormRef::Edit(_, id) => id.clone(),
            FormRef::New(doc) => self.new_ids.get(doc).cloned().unwrap_or_default(),
            FormRef::Node
            | FormRef::AddFolder
            | FormRef::Delete(..)
            | FormRef::PackSearch
            | FormRef::PackSignIn => String::new(),
        }
    }

    /// Off the chat loop, as `read_menu` reads: `answer` gets the node's access and DID, and
    /// with `work` off the update says why.
    fn answer_setup<T, U, A, F>(&self, update: U, answer: A)
    where
        T: 'static,
        U: FnOnce(Result<T, String>) -> Update + Send + 'static,
        A: FnOnce(ConfigAccess, String) -> F + Send + 'static,
        F: Future<Output = Result<T, String>> + Send + 'static,
    {
        let (work, node) = (self.work.clone(), self.principal.clone());
        self.tell_off_loop(async move {
            let answered = match work {
                Ok(work) => answer(work.access(), node).await,
                Err(off) => Err(off),
            };
            update(answered)
        });
    }

    /// As `answer_setup`, for a write that may change which agents can run: they are read
    /// again once its answer is told.
    fn answer_change<T, U, A, F>(&self, update: U, answer: A)
    where
        T: 'static,
        U: FnOnce(Result<T, String>) -> Update + Send + 'static,
        A: FnOnce(ConfigAccess, String) -> F + Send + 'static,
        F: Future<Output = Result<T, String>> + Send + 'static,
    {
        let configured = self.watching.clone();
        let told = move |answered| {
            let update = update(answered);
            let _ = configured.send(Watched::Configured);
            update
        };
        self.answer_setup(told, answer);
    }

    /// One sign-in waits at a time. Its URL is told from its run.
    fn sign_in(&mut self, provider: String, method: String, label: Option<String>) {
        if self.signing_in.is_some() {
            let refused = setup::ALREADY_WAITING.to_string();
            return self.tell(Update::SignedIn(Err(refused)));
        }
        let work = match &self.work {
            Ok(work) => work.clone(),
            Err(off) => return self.tell(Update::SignedIn(Err(off.clone()))),
        };
        let (node, told) = (self.principal.clone(), self.updates.clone());
        self.start_sign_in(move |cancel, run| async move {
            let on_url = move |url| {
                if !cancel.is_cancelled() {
                    let _ = told.send(Input::Runtime(Update::SignInUrl { url }));
                }
            };
            let access = work.access();
            setup::sign_in(&access, &node, &provider, &method, label, on_url, &run).await
        });
    }

    /// `login` gets the token `CancelSignIn` cancels and `run`, its child, which the login and
    /// its timer stop on: a timeout stops the login and its answer is still told. The answer
    /// reaches the shell through `watched`, on the loop `CancelSignIn` runs on, so a cancelled
    /// run's late answer is never taken for a newer sign-in's.
    fn start_sign_in<L, F>(&mut self, login: L)
    where
        L: FnOnce(CancellationToken, CancellationToken) -> F + Send + 'static,
        F: Future<Output = Result<SavedAccount, String>> + Send + 'static,
    {
        let cancel = CancellationToken::new();
        self.signing_in = Some(cancel.clone());
        let done = self.watching.clone();
        self.spawn(async move {
            let signed = login(cancel.clone(), cancel.child_token()).await;
            let _ = done.send(Watched::SignedIn(cancel, signed));
        });
    }

    /// Off the chat loop, as `read_menu` reads, and then `menu` is reread to show what changed.
    fn act(&self, menu: MenuId, act: MenuAct) {
        // A profile is chosen from Profiles, but it shows on the Agents menu.
        let menu = match act {
            MenuAct::SetAgentProfile { .. } => MenuId::Agents,
            _ => menu,
        };
        let (work, home) = (self.work.clone(), self.home.clone());
        let (principal, updates) = (self.principal.clone(), self.updates.clone());
        let configured = self.watching.clone();
        self.spawn(async move {
            let done = match &work {
                Ok(live) => menus::act(live, &home, &principal, &act).await,
                Err(message) => Err(message.clone()),
            };
            let (Ok(notice) | Err(notice)) = done;
            let _ = updates.send(Input::Runtime(Update::Notice(notice)));
            let _ = configured.send(Watched::Configured);
            let rows = menus::read_menu(&work, &home, &principal, menu).await;
            let _ = updates.send(Input::Runtime(Update::Menu { menu, rows }));
        });
    }

    /// A remove asks first, and a change answers its notice or the desktop's question. Reads, the
    /// questions before a change among them, run while a change does.
    fn pack_act(&mut self, act: PackAct) {
        let (runtime, home) = (self.runtime.clone(), self.home.clone());
        match act {
            PackAct::Page(page) => {
                self.pack_page = page;
                self.read_menu(MenuId::Packs);
            }
            PackAct::Keyword(word) => {
                self.pack_query = word;
                self.pack_page = 1;
                self.read_menu(MenuId::Packs);
            }
            PackAct::Install {
                pack,
                edited,
                grant,
                slots,
            } => self.pack_change(
                Some(format!("Installing {pack}…")),
                packs::install(runtime, home, pack.clone(), edited, grant, slots.clone()),
                move |done| packs::installed(pack, edited, grant, slots, done),
            ),
            PackAct::Update { pack, edited } => self.pack_change(
                Some(format!("Updating {pack}…")),
                packs::update(runtime, home, pack.clone(), edited),
                move |done| packs::updated(pack, done),
            ),
            PackAct::Remove {
                pack,
                confirmed: false,
            } => self.tell(packs::confirm_remove(pack)),
            PackAct::Remove {
                pack,
                confirmed: true,
            } => self.pack_change(
                Some(format!("Removing {pack}…")),
                packs::remove(runtime, home, pack.clone()),
                move |done| packs::removed(&pack, done),
            ),
            PackAct::ChooseSlot { plugin } => {
                self.tell_off_loop(packs::choose_slot(runtime, home, plugin));
            }
            PackAct::BindSlot { plugin, profile } => self.pack_change(
                None,
                packs::bind(runtime, home, plugin, profile),
                packs::quiet,
            ),
            PackAct::SignOut => {
                self.pack_change(None, packs::sign_out(runtime, home), packs::quiet);
            }
            PackAct::Info { pack, installed } => {
                self.tell_off_loop(packs::page(runtime, home, pack, installed));
            }
        }
    }

    /// One at a time, as the desktop's `busy` disables every pack button (`PacksPanel.tsx:136`):
    /// one asked for while another runs is refused. It says it `started`, when the desktop shows
    /// it waiting, runs off the chat loop, and `answer` says what it did, if anything. Packs is
    /// read again only once it succeeds, as the desktop refreshes (`:141`, `:168`, `:189`, `:201`,
    /// `:226`, `:257`).
    fn pack_change<E, C, A, U>(&self, started: Option<String>, change: C, answer: A)
    where
        C: Future<Output = Result<Value, E>> + Send + 'static,
        A: FnOnce(Result<Value, E>) -> U + Send + 'static,
        U: Into<Option<Update>>,
    {
        let Ok(one) = self.pack_changes.clone().try_lock_owned() else {
            return self.tell(Update::Notice(BUSY.to_string()));
        };
        if let Some(started) = started {
            self.tell(Update::Notice(started));
        }
        let (updates, ended) = (self.updates.clone(), self.watching.clone());
        self.spawn(async move {
            let done = change.await;
            // Released before the answer, so the question it asks can be answered at once.
            drop(one);
            let succeeded = done.is_ok();
            if let Some(answer) = answer(done).into() {
                let _ = updates.send(Input::Runtime(answer));
            }
            if succeeded {
                let _ = ended.send(Watched::PackChanged);
            }
        });
    }

    /// An answer off the chat loop, told as it lands.
    fn tell_off_loop<R>(&self, read: R)
    where
        R: Future<Output = Update> + Send + 'static,
    {
        let updates = self.updates.clone();
        self.spawn(async move {
            let _ = updates.send(Input::Runtime(read.await));
        });
    }

    /// A pack change, refused as the others are while one runs. Its form shows it saving, so it
    /// says nothing it started, and the shell reads Packs again once it succeeds.
    fn registry_sign_in(&self, opening: u64, values: &FormValues) {
        let saved = move |result| Update::FormSaved {
            form: FormRef::PackSignIn,
            opening,
            result,
        };
        let token = forms::value(&values.0, "token").trim().to_string();
        if token.is_empty() {
            let required = forms::problem("token", "Registry token is required");
            return self.tell(saved(Err(required)));
        }
        let Ok(one) = self.pack_changes.clone().try_lock_owned() else {
            return self.tell(saved(Err(forms::refused(BUSY))));
        };
        let (runtime, home) = (self.runtime.clone(), self.home.clone());
        let updates = self.updates.clone();
        self.spawn(async move {
            let done = packs::sign_in(runtime, home, token).await;
            drop(one);
            let _ = updates.send(Input::Runtime(saved(packs::signed_in(done))));
        });
    }

    /// Every account's limits asked of its provider again, off the chat loop, as the desktop's
    /// refresh button says "Refreshing…" until they land.
    fn refresh_usage(&self) {
        self.tell(Update::Notice("Refreshing…".to_string()));
        let work = self.work.clone();
        self.tell_off_loop(async move {
            let rows = usage::read(&work, true).await;
            let menu = MenuId::Usage;
            Update::Menu { menu, rows }
        });
    }

    /// Off the chat loop, so a slow owner holds up no other intent. A notice the read answers
    /// follows its rows.
    fn read_menu(&self, menu: MenuId) {
        let (work, home) = (self.work.clone(), self.home.clone());
        let (principal, updates) = (self.principal.clone(), self.updates.clone());
        let (runtime, query, page) = (
            self.runtime.clone(),
            self.pack_query.clone(),
            self.pack_page,
        );
        self.spawn(async move {
            let (rows, notice) = match menu {
                MenuId::Packs => {
                    match packs::read(&work, &home, &principal, &runtime, &query, page).await {
                        Ok((rows, notice)) => (Ok(rows), notice),
                        Err(off) => (Err(off), None),
                    }
                }
                MenuId::Usage => (usage::read(&work, false).await, None),
                _ => (menus::read_menu(&work, &home, &principal, menu).await, None),
            };
            let _ = updates.send(Input::Runtime(Update::Menu { menu, rows }));
            if let Some(notice) = notice {
                let _ = updates.send(Input::Runtime(Update::Notice(notice)));
            }
        });
    }

    fn read_profiles(&self, agent: String) {
        let (work, principal) = (self.work.clone(), self.principal.clone());
        self.tell_off_loop(async move {
            let rows = menus::read_profiles(&work, &principal, &agent).await;
            let menu = MenuId::Profiles;
            Update::Menu { menu, rows }
        });
    }

    fn save_keys(&self, keys: KeySet) {
        let (home, updates) = (self.home.clone(), self.updates.clone());
        self.spawn(async move {
            let saved = tokio::task::spawn_blocking(move || prefs::save_keys(&home, keys)).await;
            let failed = saved.map_or_else(
                |error| Some(error.to_string()),
                |saved| saved.err().map(|e| e.to_string()),
            );
            if let Some(error) = failed {
                let notice = format!("could not save the key set: {error}");
                let _ = updates.send(Input::Runtime(Update::Notice(notice)));
            }
        });
    }

    /// One read at a time, off the chat loop, as `reread_work` reads: a slow git diff holds up
    /// no other intent, and one asked for while a read is out runs after it lands, so the
    /// newest answer lands last.
    fn read_tree(&mut self) {
        if self.reading_tree {
            self.tree_again = true;
            return;
        }
        let Some(tree) = self.tree.clone() else {
            let unread = "no working directory to read".to_string();
            return self.tell(Update::WorkingTree(Err(unread)));
        };
        self.reading_tree = true;
        let done = self.watching.clone();
        self.spawn(async move {
            let read = git::diff_to_remote(&tree).await;
            let _ = done.send(Watched::Tree(read.map(|diff| git_changes(&diff))));
        });
    }

    /// One reread at a time, off the chat loop: a change while one is out asks for one more.
    fn reread_work(&mut self) {
        let Ok(work) = &self.work else {
            return;
        };
        if self.reading {
            self.again = true;
            return;
        }
        self.reading = true;
        let (work, subscribe, done) = (work.clone(), !self.live, self.watching.clone());
        let sessions = self.open.iter().cloned().collect();
        self.spawn(async move {
            let read = read_work(work, subscribe, sessions).await;
            let _ = done.send(Watched::Read(Box::new(read)));
        });
    }

    /// The open sessions' prompts, read as `reread_work` reads the views.
    fn reread_prompts(&mut self) {
        let Some(work) = self.work.clone().ok().filter(|_| !self.open.is_empty()) else {
            return;
        };
        if self.asking {
            self.ask_again = true;
            return;
        }
        self.asking = true;
        let subscribe = !self.mailbox_live;
        let done = self.watching.clone();
        self.spawn(async move {
            let asked = read_mailbox(work, subscribe).await;
            let _ = done.send(Watched::Asked(Box::new(asked)));
        });
    }

    fn watched(&mut self, event: Watched) {
        match event {
            Watched::Changed(Feed::Work) => self.reread_work(),
            Watched::Changed(Feed::Mailbox) => self.reread_prompts(),
            Watched::Changed(Feed::Chat(collection)) => {
                // A turn starting or ending, or a session starting, subscribes the views again
                // once their feed has ended.
                if matches!(collection, "AgentRequest" | "AgentSession") {
                    if !self.live {
                        self.reread_work();
                    }
                    if !self.mailbox_live {
                        self.reread_prompts();
                    }
                }
                self.reread_chat(stale(collection));
            }
            Watched::Ended(Feed::Work, error) => {
                self.live = false;
                if let Some(error) = error {
                    self.tell(Update::Notice(not_live(&error)));
                }
                self.tell_jobs();
                self.tell_background();
            }
            Watched::Ended(Feed::Mailbox, error) => {
                self.mailbox_live = false;
                self.tell(Update::Notice(prompts_not_live(error.as_deref())));
            }
            // The next action that may change the agents reads them and subscribes again.
            Watched::Ended(Feed::Readiness, _) => self.readiness_live = false,
            Watched::Renew(Feed::Readiness) => {
                self.readiness_live = false;
                self.reread_agents();
            }
            Watched::Changed(Feed::Readiness) | Watched::Configured => self.reread_agents(),
            Watched::Agents(read) => self.agents_read(*read),
            // The loop ends the source on it: the shell has no chat without its feed.
            Watched::Ended(Feed::Chat(collection), _) => {
                self.chat_live.remove(collection);
            }
            Watched::Renew(Feed::Work) => {
                self.live = false;
                self.reread_work();
            }
            Watched::Renew(Feed::Mailbox) => {
                self.mailbox_live = false;
                self.reread_prompts();
            }
            Watched::Renew(Feed::Chat(collection)) => {
                self.chat_live.remove(collection);
                self.reread_chat(stale(collection));
            }
            Watched::Read(read) => self.landed(*read),
            Watched::Asked(asked) => self.asked(*asked),
            Watched::Chatted(read) => self.chatted(*read),
            Watched::Tree(tree) => {
                self.reading_tree = false;
                self.tell(Update::WorkingTree(tree));
                if std::mem::take(&mut self.tree_again) {
                    self.read_tree();
                }
            }
            // Uncancelled, it is the waiting sign-in's: only its answer or a cancel frees the
            // slot a newer one would need.
            Watched::SignedIn(cancel, signed) => {
                if !cancel.is_cancelled() {
                    self.signing_in = None;
                    self.tell(Update::SignedIn(signed));
                }
            }
            Watched::PackChanged => {
                self.read_menu(MenuId::Packs);
                self.reread_agents();
            }
        }
    }

    /// A session is told its prompts when they differ from what it was last told.
    fn asked(&mut self, asked: Asked) {
        self.asking = false;
        match asked.changes {
            Some(Ok(changes)) => {
                self.mailbox_live = true;
                let watching = self.watching.clone();
                self.runtime
                    .spawn(forward(changes, Feed::Mailbox, watching));
            }
            Some(Err(error)) => self.tell(Update::Notice(prompts_not_live(Some(&error)))),
            None => {}
        }
        match asked.items {
            Ok(items) => {
                for session in &self.open {
                    let prompts = work::prompts(&items, &self.principal, session);
                    let told = self.told.get(session).map_or(&[][..], Vec::as_slice);
                    if told != prompts.as_slice() {
                        self.told.insert(session.clone(), prompts.clone());
                        self.tell(Update::Prompts {
                            session: session.clone(),
                            prompts,
                        });
                    }
                }
                let asking =
                    |child: &&String| !work::prompts(&items, &self.principal, child).is_empty();
                let waiting = self.parents.keys().filter(asking).cloned().collect();
                if waiting != self.waiting {
                    self.waiting = waiting;
                    for session in self
                        .open
                        .iter()
                        .filter(|open| self.children.contains_key(*open))
                    {
                        self.tell(Update::Running {
                            session: session.clone(),
                            rows: self.running(session),
                        });
                    }
                }
            }
            Err(error) => self.tell(Update::Notice(format!("mailbox: {error}"))),
        }
        if std::mem::take(&mut self.ask_again) {
            self.reread_prompts();
        }
    }

    fn landed(&mut self, read: Read) {
        self.reading = false;
        match read.changes {
            Some(Ok(changes)) => {
                self.live = true;
                self.runtime
                    .spawn(forward(changes, Feed::Work, self.watching.clone()));
            }
            Some(Err(error)) => self.tell(Update::Notice(not_live(&error))),
            None => {}
        }
        if let Some(error) = self.jobs.take(read.jobs, read.at) {
            self.tell(Update::Notice(format!("jobs: {error}")));
        }
        self.tell_jobs();
        if let Some(error) = self.background.take(read.background, read.at) {
            self.tell(Update::Notice(format!("background calls: {error}")));
        }
        self.tell_background();
        for session in &self.open {
            self.tell(Update::Running {
                session: session.clone(),
                rows: self.running(session),
            });
        }
        if std::mem::take(&mut self.again) {
            self.reread_work();
        }
    }

    /// `live` while subscribed and the view's last read held, else when its rows were read.
    fn as_of<T>(&self, shown: &Shown<T>) -> String {
        match shown.read {
            None => "off".to_string(),
            Some(_) if self.live && !shown.failed => "live".to_string(),
            Some(read) => format!("as of {}", work::clock(read)),
        }
    }

    fn tell_jobs(&self) {
        self.tell(Update::Jobs {
            jobs: self.jobs.rows.clone(),
            as_of: self.as_of(&self.jobs),
        });
    }

    fn tell_background(&self) {
        self.tell(Update::Background {
            calls: self.background.rows.clone(),
            as_of: self.as_of(&self.background),
        });
    }

    /// The session's subagents, each waiting on the user saying so, then its jobs.
    fn running(&self, session: &str) -> Vec<Running> {
        let children = self.children.get(session).into_iter().flatten();
        let children = children.map(|row| match &row.opens {
            Opens::Agent(child) if self.waiting.contains(child) => Running {
                status: "waiting on you".to_string(),
                ..row.clone()
            },
            _ => row.clone(),
        });
        let jobs = self.jobs.rows.iter();
        let jobs = jobs.filter(|job| job.session == session && job.ended.is_none());
        let jobs = jobs.map(|job| Running {
            name: job.name.clone(),
            job: job.title.clone(),
            status: job.status.clone(),
            state: RunState::Working,
            opens: Opens::Workflow(job.address.clone()),
        });
        children.chain(jobs).collect()
    }

    /// A chat subscription opened for `collection` goes live; why one failed is returned to tell.
    fn chat_watched(
        &mut self,
        collection: &'static str,
        changes: Option<Result<Changes, String>>,
    ) -> Option<String> {
        match changes? {
            Ok(changes) => {
                self.chat_live.insert(collection);
                let watching = self.watching.clone();
                self.runtime
                    .spawn(forward(changes, Feed::Chat(collection), watching));
                None
            }
            Err(error) => Some(error),
        }
    }

    /// One reread at a time, off the chat loop, as `reread_work` reads: a change while one is
    /// out asks for one more, which reads all that the changes since made stale.
    fn reread_chat(&mut self, stale: Stale) {
        let Ok(work) = self.work.clone() else {
            return;
        };
        if self.chatting {
            self.chat_again = Some(self.chat_again.unwrap_or_default().and(stale));
            return;
        }
        self.chatting = true;
        let subscribe = CHAT
            .into_iter()
            .filter(|collection| !self.chat_live.contains(collection));
        let subscribe = subscribe.collect();
        let asking = self.chats.iter().map(|(session, chat)| Asking {
            session: session.clone(),
            requester: chat.requester.clone(),
            opened: chat.opened,
            next: chat.next,
        });
        let asking = asking.collect();
        let (node, observed) = (self.principal.clone(), self.store.clone());
        let (edits, done) = (self.edits.clone(), self.watching.clone());
        self.spawn(async move {
            let read = read_chat(work, node, stale, subscribe, observed, asking, edits).await;
            let _ = done.send(Watched::Chatted(Box::new(read)));
        });
    }

    fn chatted(&mut self, read: ChatRead) {
        self.chatting = false;
        let mut failed = None;
        for (collection, changes) in read.changes {
            if let Some(error) = self.chat_watched(collection, changes) {
                failed.get_or_insert(error);
            }
        }
        if let Some(error) = failed {
            self.tell(Update::Notice(chat_not_live(&error)));
        }
        match read.store {
            Some(Ok(store)) => {
                self.store = Arc::new(store);
                self.tell_sessions();
                self.tell_turns();
            }
            Some(Err(error)) => self.tell(Update::Notice(error)),
            None => {}
        }
        for session in read.sessions {
            self.chatted_session(session);
        }
        if let Some(stale) = self.chat_again.take() {
            self.reread_chat(stale);
        }
    }

    /// A read for an earlier opening of its tab is older than that opening told, so it tells
    /// nothing.
    fn chatted_session(&mut self, read: SessionRead) {
        let SessionRead {
            session,
            opened,
            transcript,
            edits,
            live,
            children,
        } = read;
        let Some(chat) = self.chats.get_mut(&session) else {
            return;
        };
        if chat.opened != opened {
            return;
        }
        self.edits.extend(edits);
        match transcript {
            Some(Ok(transcript)) => {
                chat.next = transcript.next;
                self.tell_lines(&session, transcript.lines);
            }
            Some(Err(error)) => self.tell(Update::Notice(error)),
            None => {}
        }
        match live {
            Ok(lines) => self.tell_live(&session, lines),
            Err(error) => self.tell(Update::Notice(error)),
        }
        match children {
            Some(Ok(children)) => self.tell_children(&session, &children, false),
            Some(Err(error)) => self.tell(Update::Notice(error)),
            None => {}
        }
    }

    /// The sessions list, when it changed.
    fn tell_sessions(&mut self) {
        let listed = items::sessions(&self.store, &self.principal, today());
        if listed != self.listed {
            self.listed = listed.clone();
            self.tell(Update::Sessions(listed));
        }
    }

    /// Each open session's turn that ended, with why when it failed, then any that started.
    fn tell_turns(&mut self) {
        let mut told = Vec::new();
        for (session, chat) in &mut self.chats {
            let now = turn_of(&self.store, &self.principal, &self.interrupted, session);
            if chat.turn == now {
                continue;
            }
            if let Some(turn) = chat.turn.take() {
                let failed = failure(&self.store, &turn);
                let session = session.clone();
                told.push(Update::TurnEnded { session, turn });
                told.extend(failed.map(Update::Notice));
            }
            if let Some(turn) = now.clone() {
                let session = session.clone();
                told.push(Update::Turn { session, turn });
            }
            chat.turn = now;
        }
        for update in told {
            self.tell(update);
        }
    }

    /// Each line that is new or differs from the one its tab shows.
    fn tell_lines(&mut self, session: &str, lines: Vec<ChatLine>) {
        let Some(chat) = self.chats.get_mut(session) else {
            return;
        };
        let mut told = Vec::new();
        for line in lines {
            let id = line.item.clone().unwrap_or_default();
            if chat.lines.get(&id) == Some(&line) {
                continue;
            }
            chat.lines.insert(id, line.clone());
            let session = session.to_string();
            told.push(Update::Item { session, line });
        }
        for update in told {
            self.tell(update);
        }
    }

    /// A running turn's text arrives whole: what it added to the line its tab shows is told
    /// as a delta, and anything else replaces the line.
    fn tell_live(&mut self, session: &str, lines: Vec<ChatLine>) {
        let Some(chat) = self.chats.get_mut(session) else {
            return;
        };
        let mut told = Vec::new();
        for line in lines {
            let id = line.item.clone().unwrap_or_default();
            let shown = chat.lines.get(&id).filter(|shown| shown.tone == line.tone);
            let added = shown.and_then(|shown| line.text.strip_prefix(shown.text.as_str()));
            let session = session.to_string();
            match added {
                Some("") => {}
                Some(added) => told.push(Update::Delta {
                    session,
                    item: id.clone(),
                    text: added.to_string(),
                    tone: line.tone,
                }),
                None => told.push(Update::Item {
                    session,
                    line: line.clone(),
                }),
            }
            chat.lines.insert(id, line);
        }
        for update in told {
            self.tell(update);
        }
    }

    /// The session's own subagents, oldest first; its rows are told when they changed, or
    /// `always`.
    fn tell_children(&mut self, session: &str, caused: &[CausedSession], always: bool) {
        let own = caused
            .iter()
            .filter(|child| child.caused_by_scope.session_id == session);
        let mut rows = Vec::new();
        for child in own {
            let parent = session.to_string();
            self.parents.insert(child.scope.session_id.clone(), parent);
            rows.push(items::subagent(child, &self.agents));
        }
        let changed = self.children.get(session) != Some(&rows);
        self.children.insert(session.to_string(), rows);
        if changed || always {
            self.tell(Update::Running {
                session: session.to_string(),
                rows: self.running(session),
            });
        }
    }

    /// Reads the session afresh: its running turn, which is told first so the shell never
    /// draws its card, then its transcript, the turn's text so far and its subagents.
    async fn open(&mut self, session: String) {
        let Ok(work) = self.work.clone() else {
            return;
        };
        let access = work.access();
        match load_node_snapshot_on(&access, &self.principal).await {
            Ok(store) => self.store = Arc::new(store),
            Err(error) => return self.tell(Update::Notice(format!("{error:#}"))),
        }
        let listed = self.store.sessions.iter();
        let mut listed = listed.filter(|row| row.node_did == self.principal);
        let found = listed.find(|row| row.session_id == session);
        let kept = self
            .chats
            .get(&session)
            .and_then(|chat| chat.requester.clone());
        let requester = found.and_then(|row| row.requester_did.clone()).or(kept);
        let asked = Asking {
            session: session.clone(),
            requester: requester.clone(),
            opened: 0,
            next: 0,
        };
        let every = Stale {
            sessions: true,
            transcripts: true,
        };
        let node = &self.principal;
        let read = read_session(&access, node, &self.store, asked, every, &self.edits).await;
        let transcript = match read.transcript {
            Some(Ok(transcript)) => transcript,
            Some(Err(error)) => return self.tell(Update::Notice(error)),
            None => items::Transcript::default(),
        };
        self.edits.extend(read.edits);
        if self.open.insert(session.clone()) {
            self.reread_work();
        }
        let turn = turn_of(&self.store, &self.principal, &self.interrupted, &session);
        let chat = self.chats.entry(session.clone()).or_default();
        chat.opened += 1;
        chat.requester = requester;
        chat.next = transcript.next;
        chat.turn = turn.clone();
        let shown = transcript.lines.iter();
        let shown = shown.map(|line| (line.item.clone().unwrap_or_default(), line.clone()));
        chat.lines = shown.collect();
        if let Some(turn) = turn {
            let session = session.clone();
            self.tell(Update::Turn { session, turn });
        }
        let lines = transcript.lines.into_iter();
        let lines = lines.flat_map(|line| [line, blank()]).collect();
        let opened = session.clone();
        self.tell(Update::Opened {
            session: opened,
            lines,
        });
        match read.live {
            Ok(lines) => self.tell_live(&session, lines),
            Err(error) => self.tell(Update::Notice(error)),
        }
        match read.children {
            Some(Ok(children)) => self.tell_children(&session, &children, true),
            Some(Err(error)) => {
                self.tell(Update::Notice(error));
                let rows = self.running(&session);
                let session = session.clone();
                self.tell(Update::Running { session, rows });
            }
            None => {}
        }
        // A new tab shows no prompts until it is told them.
        self.told.remove(&session);
        self.reread_prompts();
    }

    /// Through the served mailbox owner, which refuses a question that needs an answer.
    async fn dismiss(&mut self, item: &str) {
        let Ok(work) = &self.work else {
            return;
        };
        let dismissed = gents_session::operations::mailbox::dismiss(&work.access(), item).await;
        if let Err(error) = dismissed {
            self.tell(Update::Notice(format!("{:#}", mailbox::failure(error))));
        }
        self.reread_prompts();
    }

    /// The reply is a request caused by the item, which the session's feed then shows.
    async fn reply(&mut self, item: &str, text: &str) {
        let Ok(work) = self.work.clone() else {
            return;
        };
        let replied = async {
            use gents_protocol::operations::mailbox::MailboxReply;
            use gents_session::operations::mailbox as operations;
            let item = operations::get(&work.access(), item)
                .await
                .map_err(mailbox::failure)?;
            let reply = match work::reply_from_text(text, &item).map_err(anyhow::Error::msg)? {
                MailboxReply::Answer(answer) => MailboxReply::Answer(
                    operations::answer_observing((&item).into(), answer)
                        .map_err(mailbox::failure)?,
                ),
                text => text,
            };
            let prepared = operations::prepare_reply((&item).into(), reply, &Default::default())
                .map_err(mailbox::failure)?;
            mailbox::send_reply(&work.access(), prepared).await
        };
        if let Err(error) = replied.await {
            self.tell(Update::Notice(format!("{error:#}")));
        }
        self.reread_prompts();
    }

    /// The node's enabled skills, sorted by name, narrowed to the agent's context as admission
    /// narrows them.
    async fn read_skills(&mut self, agent: String) {
        let work = match self.work.clone() {
            Ok(work) => work,
            Err(off) => return self.skills_unread(agent, off),
        };
        let (access, node) = (work.access(), self.principal.clone());
        let read = async {
            let rows = query_collection_sorted(&access, SKILL_SPEC, &node).await?;
            let allowed = forms::skill::allowed(&access, &node, &agent).await?;
            anyhow::Ok((rows, allowed))
        };
        let (rows, allowed) = match read.await {
            Ok(read) => read,
            Err(error) => return self.skills_unread(agent, format!("{error:#}")),
        };
        let text = |row: &Value, field: &str| {
            let text = row[field].as_str().map(str::trim).unwrap_or_default();
            text.to_string()
        };
        let mut skills: Vec<_> = rows
            .iter()
            .filter(|row| row["enabled"] != Value::Bool(false))
            .map(|row| {
                (
                    text(row, "skill_id"),
                    text(row, "name"),
                    text(row, "description"),
                )
            })
            .filter(|(id, ..)| !id.is_empty() && allowed.contains(id))
            .map(|(id, name, about)| {
                let name = if name.is_empty() { id.clone() } else { name };
                (name, id, about)
            })
            .collect();
        skills.sort();
        let rows = skills.iter().map(|(name, id, about)| Skill {
            id: id.clone(),
            about: if about.is_empty() { name } else { about }.clone(),
        });
        let rows = rows.collect();
        let ids = skills.into_iter().map(|(_, id, _)| id).collect();
        self.skills.insert(agent.clone(), ids);
        self.tell(Update::Skills {
            agent,
            skills: rows,
        });
    }

    /// An earlier read's rows are dropped, so a stale skill is neither shown nor sent.
    fn skills_unread(&mut self, agent: String, message: String) {
        self.skills.remove(&agent);
        self.tell(Update::Notice(message));
        self.tell(Update::Skills {
            agent,
            skills: Vec::new(),
        });
    }

    /// The session to start on: `session`, or a new one told as `Started`, which its first
    /// request makes.
    fn session_for(&self, tab: u64, session: Option<String>) -> String {
        session.unwrap_or_else(|| {
            let session = uuid::Uuid::new_v4().to_string();
            self.tell(Update::Started {
                tab,
                session: session.clone(),
            });
            session
        })
    }

    /// A tab with no session gets one first, and `Started` either way. A refused turn is told
    /// as `StartFailed`.
    async fn send(
        &mut self,
        tab: u64,
        session: Option<String>,
        agent: &str,
        text: String,
        skills: Vec<String>,
    ) {
        let work = match self.work.clone() {
            Ok(work) => work,
            Err(message) => return self.tell(Update::StartFailed { tab, message }),
        };
        if !self.shown_agents.iter().any(|shown| shown == agent) {
            let message = not_connected(agent);
            return self.tell(Update::StartFailed { tab, message });
        }
        let session = self.session_for(tab, session);
        let submitted = submit(&work, &self.principal, &session, agent, &text, skills).await;
        if self.open.insert(session.clone()) {
            self.reread_work();
        }
        match submitted {
            Ok(submitted) => {
                let chat = self.chats.entry(session.clone()).or_default();
                chat.requester = chat.requester.take().or(submitted.requester_did);
                chat.turn = Some(submitted.request_id.clone());
                let turn = submitted.request_id;
                self.tell(Update::Turn { session, turn });
            }
            Err(error) => self.tell(Update::StartFailed {
                tab,
                message: format!("{error:#}"),
            }),
        }
    }

    /// The goal and its first request, whose content is the objective, commit together through
    /// the goal owner, which refuses replacing a live goal.
    async fn goal(&mut self, tab: u64, session: Option<String>, agent: &str, objective: String) {
        let work = match self.work.clone() {
            Ok(work) => work,
            Err(message) => return self.tell(Update::StartFailed { tab, message }),
        };
        if !self.shown_agents.iter().any(|shown| shown == agent) {
            let message = not_connected(agent);
            return self.tell(Update::StartFailed { tab, message });
        }
        let session = self.session_for(tab, session);
        let principal = &self.principal;
        let submitted = async {
            let access = work.access();
            let create = crate::request_helpers::goal_request(
                &access, principal, &session, agent, &objective,
            )
            .await?;
            let input = gents_protocol::operations::goal::GoalStartInput {
                session_id: session.clone(),
                objective: objective.clone(),
                token_budget: None,
            };
            gents_automation::operations::goal::start(&access, principal, &input, &create)
                .await
                .map_err(anyhow::Error::from)
        };
        match submitted.await {
            Ok(_) => {
                self.open(session).await;
                self.tell_sessions();
            }
            Err(error) => self.tell(Update::StartFailed {
                tab,
                message: format!("Couldn’t set the goal: {error:#}"),
            }),
        }
    }

    /// Stops exactly the turn the tab shows. One the node already shows ended is only
    /// cleared.
    async fn interrupt(&mut self, session: String, turn: String) {
        let Ok(work) = self.work.clone() else {
            return;
        };
        let request = crate::request_helpers::request_ref(&self.principal, &turn);
        let input = RequestInterruptInput { request };
        match gents::operations::request::interrupt(&work.access(), input).await {
            Ok(_) => {}
            Err(error) => {
                let failure = crate::operation_failure::OperationFailure::from(error);
                return self.tell(Update::Notice(failure.to_string()));
            }
        }
        self.interrupted.insert(turn.clone());
        if let Some(chat) = self.chats.get_mut(&session) {
            if chat.turn.as_ref() == Some(&turn) {
                chat.turn = None;
            }
        }
        self.tell(Update::TurnEnded { session, turn });
    }
}

/// The user's message. A turn still running in the session finishes first: the shell has no
/// per-message choice to steer it.
async fn submit(
    work: &Work,
    node: &str,
    session: &str,
    agent: &str,
    text: &str,
    skills: Vec<String>,
) -> anyhow::Result<RequestReceipt> {
    let access = work.access();
    let signer = crate::request_helpers::client_signer(&access)?;
    let input = MessageSendInput {
        node_did: node.to_owned(),
        session_id: Some(session.to_owned()),
        agent_did: Some(agent.to_owned()),
        content: text.to_owned(),
        input: RequestInput {
            selected_skill_ids: skills,
            ..Default::default()
        },
        ..Default::default()
    };
    let sent = gents::operations::request::send(&access, &signer, input).await;
    Ok(sent.map_err(crate::operation_failure::OperationFailure::from)?)
}

/// The goal's first request, signed by this CLI's User. The goal owner commits it with the goal
/// and recognises a retried submission by its retry key, so it is built here rather than sent.
/// The session's running request, unless the user stopped it.
fn turn_of(
    store: &ClientStore,
    node: &str,
    stopped: &RapidHashSet<String>,
    session: &str,
) -> Option<String> {
    let request = store.turn_request_id_for_session_for_node(session, node)?;
    let state = store.derive_turn_for_request_for_node(&request, node)?;
    (!state.is_terminal() && !stopped.contains(&request)).then_some(request)
}

/// Why `request` failed, when it did.
fn failure(store: &ClientStore, request: &str) -> Option<String> {
    use RequestLifecycleState::{Dead, Failed};
    let row = store.request_row(request)?;
    let failed = matches!(row.lifecycle_state, Some(Failed | Dead));
    let reason = row.failure_reason.as_deref().map(str::trim);
    let reason = reason.filter(|reason| failed && !reason.is_empty())?;
    Some(items::clean(reason))
}

fn chat_not_live(error: &str) -> String {
    format!("sessions are not live: reopen the shell to follow them ({error})")
}

fn not_connected(agent: &str) -> String {
    format!("agent {agent} is not open in this shell")
}

/// The shell puts a blank after each new item's line; an opened transcript matches it.
fn blank() -> ChatLine {
    ChatLine {
        text: String::new(),
        tone: Tone::Plain,
        item: None,
        change: None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gents_pack::pack::PackChangeError;

    use super::super::tests::{home_serving, within};
    use super::*;
    use crate::commands::eval::testing::test_runtime_handle;

    const OFF: &str = "jobs are off";

    /// A source with `work` off, so no intent can start a real sign-in, and the updates it
    /// tells and the events its loop would read.
    fn idle_source() -> (
        Source,
        std::sync::mpsc::Receiver<Input>,
        mpsc::UnboundedReceiver<Watched>,
    ) {
        let (updates, told) = std::sync::mpsc::channel();
        let (watching, watched) = mpsc::unbounded_channel();
        let off = Err(OFF.to_string());
        let runtime = test_runtime_handle();
        let source = Source::new(updates, off, PathBuf::new(), runtime, watching);
        (source, told, watched)
    }

    fn sign_in() -> Intent {
        Intent::SignIn {
            provider: "anthropic".to_string(),
            method: "claude_oauth".to_string(),
            label: None,
        }
    }

    fn signed_in(told: &std::sync::mpsc::Receiver<Input>) -> Vec<Result<SavedAccount, String>> {
        let told = told.try_iter().filter_map(|input| match input {
            Input::Runtime(Update::SignedIn(signed)) => Some(signed),
            _ => None,
        });
        told.collect()
    }

    /// The picker lists what it was last told, so an agent that stops being runnable and
    /// recovers is told again, though it is not new.
    #[tokio::test]
    async fn an_agent_that_recovers_is_listed_again() {
        let (mut source, told, _watched) = idle_source();
        let runnable = |names: &[&str]| Runnable {
            views: Vec::new(),
            agents: names
                .iter()
                .map(|name| gents_shell::source::Agent {
                    id: name.to_string(),
                    name: name.to_string(),
                    text: name.to_string(),
                    tone: Tone::Plain,
                })
                .collect(),
            unready: None,
        };

        source.take_agents(runnable(&["a"]));
        source.take_agents(runnable(&["b"]));
        source.take_agents(runnable(&["a", "b"]));
        source.take_agents(runnable(&["a", "b"]));

        let listed: Vec<Vec<String>> = told
            .try_iter()
            .filter_map(|input| match input {
                Input::Runtime(Update::Agents(agents)) => {
                    Some(agents.into_iter().map(|agent| agent.id).collect())
                }
                _ => None,
            })
            .collect();
        assert_eq!(listed, [vec!["a"], vec!["b"], vec!["a", "b"]]);
    }

    #[tokio::test]
    async fn a_second_sign_in_waits_its_turn() {
        let (mut source, told, _watched) = idle_source();
        let waiting = CancellationToken::new();
        source.signing_in = Some(waiting.clone());

        source.handle(sign_in()).await;

        let refused = "A sign-in is already waiting. Finish or cancel it first.";
        assert_eq!(signed_in(&told), [Err(refused.to_string())]);
        assert!(!waiting.is_cancelled(), "the waiting sign-in is left alone");
    }

    #[tokio::test]
    async fn a_sign_in_right_after_a_cancel_starts() {
        let (mut source, told, _watched) = idle_source();
        let waiting = CancellationToken::new();
        source.signing_in = Some(waiting.clone());

        source.handle(Intent::CancelSignIn).await;
        source.handle(sign_in()).await;

        assert!(waiting.is_cancelled());
        // Taken, not refused: with `work` off it answers why it cannot sign in.
        assert_eq!(signed_in(&told), [Err(OFF.to_string())]);
    }

    #[tokio::test]
    async fn a_cancelled_sign_in_never_answers_for_the_next() {
        let (mut source, told, mut watched) = idle_source();
        let old = SavedAccount {
            label: "Old".to_string(),
            account_ref: "old".to_string(),
        };
        let (returned, drained) = tokio::sync::oneshot::channel();
        // As a login draining after its stop, whose tokens were already issued.
        source.start_sign_in(move |_, run| async move {
            run.cancelled().await;
            let _ = returned.send(());
            Ok(old)
        });
        source.handle(Intent::CancelSignIn).await;
        source.start_sign_in(|_, _| std::future::pending());

        // The run answers in the poll its login returns in, so by now it has.
        within(drained).await.expect("the old login returned");
        while let Ok(answered) = watched.try_recv() {
            source.watched(answered);
        }

        assert_eq!(signed_in(&told), []);
        let newer = source.signing_in.as_ref().expect("the newer sign-in waits");
        assert!(!newer.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn a_sign_in_past_its_wait_says_it_timed_out() {
        let (mut source, told, mut watched) = idle_source();
        let (returned, stopped) = tokio::sync::oneshot::channel();
        source.start_sign_in(|_, run| async move {
            let login = async {
                run.cancelled().await;
                let _ = returned.send(());
                Err::<SavedAccount, _>(anyhow::anyhow!("Sign-in cancelled."))
            };
            setup::waited(login, &run, "Grok").await
        });

        // Paused, the clock jumps to the wait's end; the run answers as its login returns.
        stopped.await.expect("the timer stopped the login");
        while let Ok(answered) = watched.try_recv() {
            source.watched(answered);
        }

        let timed_out = "Grok sign-in timed out waiting for the browser.";
        assert_eq!(signed_in(&told), [Err(timed_out.to_string())]);
        assert!(source.signing_in.is_none(), "the next sign-in may start");
    }

    #[tokio::test]
    async fn a_pack_search_is_kept_and_starts_at_page_one() {
        use gents_shell::source::PackAct;
        let (mut source, told, _watched) = idle_source();
        let page = |page| Intent::Act {
            menu: MenuId::Packs,
            act: MenuAct::Pack(PackAct::Page(page)),
        };
        let typed = vec![("query".to_string(), " git ".to_string())];
        let save = Intent::SaveForm {
            form: FormRef::PackSearch,
            opening: 7,
            values: FormValues(typed),
        };
        let open = Intent::OpenForm {
            form: FormRef::PackSearch,
            opening: 8,
        };

        source.handle(page(3)).await;
        let paged = source.pack_page;
        source.handle(save).await;
        source.handle(open).await;

        assert_eq!(paged, 3);
        assert_eq!((source.pack_query.as_str(), source.pack_page), ("git", 1));
        let told: Vec<_> = told
            .try_iter()
            .filter_map(|input| match input {
                Input::Runtime(update @ (Update::Form { .. } | Update::FormSaved { .. })) => {
                    Some(update)
                }
                _ => None,
            })
            .collect();
        let saved = Update::FormSaved {
            form: FormRef::PackSearch,
            opening: 7,
            result: Ok(String::new()),
        };
        assert_eq!(told[0], saved);
        let Update::Form {
            opening: 8,
            result: Ok(form),
            ..
        } = &told[1]
        else {
            panic!("{told:?}");
        };
        assert_eq!(form.fields[0].value, "git");
    }

    type Told = Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Input>>>;

    /// A source with `work` on, over a home whose runtime does not answer, and the home and its
    /// key's folder.
    async fn without_runtime() -> (
        Source,
        Told,
        mpsc::UnboundedReceiver<Watched>,
        [tempfile::TempDir; 2],
    ) {
        let keys = tempfile::tempdir().unwrap();
        let key = keys.path().join("principal.key");
        let (home, _) = home_serving("http://127.0.0.1:1/api/v0/graphql", Some(&key));
        gents_host::test_support::cli_user().await;
        let work = work::work_for(home.path()).await;
        assert!(work.is_ok(), "this CLI's User signs: {:?}", work.err());
        let (updates, told) = std::sync::mpsc::channel();
        let (watching, watched) = mpsc::unbounded_channel();
        let runtime = test_runtime_handle();
        let source = Source::new(updates, work, home.path().into(), runtime, watching);
        let told = Arc::new(std::sync::Mutex::new(told));
        (source, told, watched, [home, keys])
    }

    async fn next(told: &Told) -> Update {
        let told = told.clone();
        let input = test_runtime_handle()
            .spawn_blocking(move || told.lock().unwrap().recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap()
            .expect("the source should have told an update by now");
        match input {
            Input::Runtime(update) => update,
            other => panic!("{other:?}"),
        }
    }

    /// Packs is read by its own owner, which waits for the runtime, not by `menus::read_menu`,
    /// which answers no rows for it.
    #[tokio::test]
    async fn packs_are_read_by_their_owner() {
        let (mut source, told, _watched, _dirs) = without_runtime().await;

        source.handle(Intent::ReadMenu(MenuId::Packs)).await;

        let rows = Err(closed(None));
        let menu = MenuId::Packs;
        assert_eq!(next(&told).await, Update::Menu { menu, rows });
    }

    fn install_git() -> Intent {
        let install = PackAct::Install {
            pack: "acme/git".to_string(),
            edited: gents_shell::source::EditedChoice::Refuse,
            grant: false,
            slots: Vec::new(),
        };
        let act = MenuAct::Pack(install);
        let menu = MenuId::Packs;
        Intent::Act { menu, act }
    }

    fn installing_git() -> Update {
        Update::Notice("Installing acme/git…".to_string())
    }

    /// Once the source ends, `watched` closes when the change's task does: `None` means the
    /// change asked for no reread.
    async fn reread_asked(source: Source, mut watched: mpsc::UnboundedReceiver<Watched>) -> bool {
        drop(source);
        within(watched.recv()).await.is_some()
    }

    #[tokio::test]
    async fn a_pack_change_waits_for_the_runtime() {
        let (mut source, told, watched, [home, _]) = without_runtime().await;

        source.handle(install_git()).await;

        assert_eq!(next(&told).await, installing_git());
        assert_eq!(next(&told).await, Update::Notice(closed(None)));
        assert!(
            !reread_asked(source, watched).await,
            "a failure reads nothing"
        );
        let data = gents_service::home_state::default_data_dir(home.path());
        assert!(!data.exists(), "a pack call opened the store");
    }

    #[tokio::test]
    async fn a_refusal_asks_and_reads_nothing_again() {
        use gents_shell::source::EditedChoice::Refuse;
        let (source, told, watched, _dirs) = without_runtime().await;
        let refused = PackChangeError::Edited {
            coordinate: "acme/git".to_string(),
            edited: vec!["skill review".to_string()],
        };
        let answer =
            |done| packs::installed("acme/git".to_string(), Refuse, false, Vec::new(), done);

        source.pack_change(
            Some("Installing acme/git…".into()),
            async { Err(refused) },
            answer,
        );

        assert_eq!(next(&told).await, installing_git());
        let asked = next(&told).await;
        assert!(matches!(&asked, Update::Ask { title, .. } if title == "Install acme/git"));
        assert!(
            !reread_asked(source, watched).await,
            "a refusal reads nothing"
        );
        let menus = told.lock().unwrap().try_iter().count();
        assert_eq!(menus, 0, "no Update::Menu follows the Ask");
    }

    #[tokio::test]
    async fn a_success_reads_packs_again() {
        use gents_shell::source::EditedChoice::Refuse;
        let (mut source, told, mut watched, _dirs) = without_runtime().await;
        let answer =
            |done| packs::installed("acme/git".to_string(), Refuse, false, Vec::new(), done);

        let done = async { Ok(serde_json::json!({})) };
        source.pack_change(Some("Installing acme/git…".into()), done, answer);
        assert_eq!(next(&told).await, installing_git());
        let installed = Update::Notice("Installed acme/git".to_string());
        assert_eq!(next(&told).await, installed);
        let ended = within(watched.recv())
            .await
            .expect("the change asked for a reread");
        source.watched(ended);

        let rows = Err(closed(None));
        let menu = MenuId::Packs;
        assert_eq!(
            next(&told).await,
            Update::Menu { menu, rows },
            "through packs::read"
        );
    }

    #[tokio::test]
    async fn a_second_pack_change_is_refused_while_one_runs() {
        let (mut source, told, _watched) = idle_source();
        let running = source.pack_changes.clone().try_lock_owned().unwrap();

        source.handle(install_git()).await;

        let told: Vec<_> = told.try_iter().collect();
        let refused = "A pack change is still running".to_string();
        assert!(
            matches!(&told[..], [Input::Runtime(Update::Notice(notice))] if *notice == refused),
            "{told:?}"
        );
        assert_eq!(
            Arc::strong_count(&source.pack_changes),
            2,
            "nothing else runs"
        );
        drop(running);
    }

    #[tokio::test]
    async fn remove_confirms_first() {
        let (mut source, told, _watched) = idle_source();
        let remove = |confirmed| {
            let pack = "acme/git".to_string();
            MenuAct::Pack(PackAct::Remove { pack, confirmed })
        };

        let act = remove(false);
        source
            .handle(Intent::Act {
                menu: MenuId::Packs,
                act,
            })
            .await;

        let told: Vec<_> = told.try_iter().collect();
        let [Input::Runtime(Update::Ask { title, rows })] = &told[..] else {
            panic!("{told:?}");
        };
        assert_eq!(title, "Remove acme/git and everything it installed?");
        let rows: Vec<_> = rows.iter().map(|row| (&row.text[..], &row.enter)).collect();
        assert_eq!(rows, [(" Remove", &Some(remove(true)))]);
        let started = Arc::strong_count(&source.pack_changes) > 1;
        assert!(!started, "packs::remove is not called");
    }

    fn pack(act: PackAct) -> Intent {
        let act = MenuAct::Pack(act);
        let menu = MenuId::Packs;
        Intent::Act { menu, act }
    }

    #[tokio::test]
    async fn sign_in_saves_the_token_and_refuses_an_empty_one() {
        use gents_shell::source::FormError;
        let (mut source, told, _watched, _dirs) = without_runtime().await;
        let save = |token: &str| Intent::SaveForm {
            form: FormRef::PackSignIn,
            opening: 1,
            values: FormValues(vec![("token".to_string(), token.to_string())]),
        };
        let saved = |field: Option<&str>, message: &str| Update::FormSaved {
            form: FormRef::PackSignIn,
            opening: 1,
            result: Err(FormError {
                field: field.map(str::to_string),
                message: message.to_string(),
            }),
        };
        let open = Intent::OpenForm {
            form: FormRef::PackSignIn,
            opening: 1,
        };

        source.handle(open).await;
        let form = Update::Form {
            form: FormRef::PackSignIn,
            opening: 1,
            result: Ok(packs::sign_in_form()),
        };
        assert_eq!(next(&told).await, form);
        source.handle(save("  ")).await;
        let required = saved(Some("token"), "Registry token is required");
        assert_eq!(next(&told).await, required);

        let running = source.pack_changes.clone().try_lock_owned().unwrap();
        source.handle(save("tok")).await;
        assert_eq!(next(&told).await, saved(None, BUSY));
        drop(running);
        // The login waits for the runtime, which does not answer, and frees the lock after.
        source.handle(save(" tok ")).await;
        assert_eq!(next(&told).await, saved(None, &closed(None)));
        assert!(source.pack_changes.clone().try_lock_owned().is_ok());
    }

    /// Binding and signing out wait for a change that runs, as install does; the reads that ask
    /// do not.
    #[tokio::test]
    async fn pack_writes_wait_their_turn_and_reads_do_not() {
        let (mut source, told, _watched, _dirs) = without_runtime().await;
        let running = source.pack_changes.clone().try_lock_owned().unwrap();
        let plugin = || "acme/git".to_string();
        let writes = [
            PackAct::BindSlot {
                plugin: plugin(),
                profile: None,
            },
            PackAct::SignOut,
        ];

        for write in writes {
            source.handle(pack(write)).await;
            assert_eq!(next(&told).await, Update::Notice(BUSY.to_string()));
        }
        source
            .handle(pack(PackAct::ChooseSlot { plugin: plugin() }))
            .await;
        assert_eq!(next(&told).await, Update::Notice(closed(None)));
        drop(running);
    }

    #[tokio::test]
    async fn a_keyword_searches_from_page_one() {
        let (mut source, told, _watched, _dirs) = without_runtime().await;
        source.pack_page = 3;

        source
            .handle(pack(PackAct::Keyword("vcs".to_string())))
            .await;

        assert_eq!((source.pack_query.as_str(), source.pack_page), ("vcs", 1));
        let rows = Err(closed(None));
        let menu = MenuId::Packs;
        assert_eq!(next(&told).await, Update::Menu { menu, rows });
    }
}
