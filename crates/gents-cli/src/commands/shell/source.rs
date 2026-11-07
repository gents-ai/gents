use std::cmp::Reverse;
use std::future::Future;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Local, Utc};
use futures_util::stream::BoxStream;
use futures_util::{Stream, StreamExt};
use gents::session::mailbox::MailboxItem;
use gents::store::config_client::{ConfigAccess, SubscribeError};
use gents_codex_protocol as codex;
use gents_shell::app::ChatLine;
use gents_shell::sample::{MenuId, Opens, RunState, Running, Tone};
use gents_shell::source::{
    BackgroundCall, Change, Connection, Input, Intent, Job, MenuAct, Prompt, SavedAccount, Session,
    Update,
};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::client::{Client, ClientError, ClientEvent};
use super::tree::git_changes;
use super::work::{self, Work};
use super::{items, menus, setup};
use crate::commands::accounts::account_rows;
use crate::commands::codex::relay::agent_route_target;
use crate::commands::codex::{resolve_endpoint, runtime_identity_at, RemoteEndpoint};
use crate::commands::codex_shim::RuntimeIdentity;
use crate::commands::mailbox;

/// What the shell opens with. `work` is why jobs and the mailbox are off when it is `Err`.
pub(super) struct Opening {
    pub(super) remote: String,
    pub(super) agent: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) work: Result<Work, String>,
    pub(super) home: PathBuf,
}

/// Feeds the shell from the runtime at `remote`, over one connection per agent, waiting
/// `answer_wait` for each answer. A given `agent` or `prompt` follows the agents.
///
/// Each intent is answered before the next notification is read. The shell drops a tab's
/// updates until `Started` binds its session, and `Opened` replaces a tab's lines, so both
/// must reach it before any notification that arrived while their request was out.
pub(super) async fn run(
    opening: Opening,
    mut intents: mpsc::UnboundedReceiver<Intent>,
    updates: Sender<Input>,
    runtime: Handle,
    answer_wait: Duration,
) {
    let Opening {
        remote,
        agent,
        prompt,
        work,
        home,
    } = opening;
    let off = work.as_ref().err().cloned();
    let (watching, mut watched) = mpsc::unbounded_channel();
    let mut source = Source::new(updates, work, home, runtime.clone(), watching);
    let Some(identity) = runtime_identity_at(&remote).await else {
        return source.lost(format!(
            "no runtime: check `gents status`, then reopen the shell (could not read this home's runtime at {remote})"
        ));
    };
    source.principal = identity.node_did.clone();
    let (events, mut received) = mpsc::unbounded_channel();
    let agents = items::agents(&identity);
    for agent in &agents {
        let url = agent_url(&remote, &identity, &agent.id);
        let connected = Client::connect(
            &url,
            agent.id.clone(),
            events.clone(),
            &runtime,
            answer_wait,
        )
        .await;
        match connected {
            Ok(client) => source.clients.push((agent.id.clone(), Arc::new(client))),
            Err(error) => source.tell(Update::Notice(format!(
                "could not open agent {}: {error}",
                agent.name
            ))),
        }
    }
    drop(events);
    if source.clients.is_empty() {
        return source.lost(format!(
            "no runtime: check `gents status`, then reopen the shell (could not connect to this home's runtime at {remote})"
        ));
    }
    let connected = agents
        .into_iter()
        .filter(|agent| source.client(&agent.id).is_some())
        .collect();
    source.tell(Update::Agents(connected));
    // `Begin` must reach the shell before a send the user makes once the agents are known.
    // Both go through one queue, so it is told before the sweep awaits anything.
    if agent.is_some() || prompt.is_some() {
        source.tell(Update::Begin { agent, prompt });
    }
    source.tell(Update::Connection(Connection::Ready));
    source.tell(Update::Sessions(source.sessions().await));
    match off {
        Some(off) => {
            source.tell(Update::Notice(off));
            source.tell_jobs();
            source.tell_background();
        }
        None => source.reread_work(),
    }
    loop {
        tokio::select! {
            intent = intents.recv() => match intent {
                Some(intent) => source.handle(intent).await,
                None => return,
            },
            Some((agent, event)) = received.recv() => match event {
                ClientEvent::Notification(note) => source.notified(&agent, *note).await,
                ClientEvent::Closed(error) => return source.lost(closed(error)),
            },
            Some(event) = watched.recv() => source.watched(event),
        }
    }
}

/// The tool calls that are plugin jobs or background calls. Each event is only a hint to
/// reread: the node drops events when a subscriber falls behind, and a delete reads like an
/// update.
pub(super) const WATCHED: &str = r#"subscription { AgentToolCall(filter: { _or: [{ job_pin: { _ne: null } }, { await_mode: { _eq: "background" } }] }) { _docID } }"#;

/// Any change to an item a prompt shows: an item's kind never changes, so one leaving the open
/// prompts still fires. Each event is only a hint to reread them.
pub(super) const MAILBOX_WATCHED: &str =
    r#"subscription { MailboxItem(filter: { kind: { _in: ["ask", "gate"] } }) { _docID } }"#;

type Changes = BoxStream<'static, Result<Value, SubscribeError>>;

/// What a subscription watches: the work views' tool calls, or the mailbox.
#[derive(Clone, Copy)]
enum Feed {
    Work,
    Mailbox,
}

enum Watched {
    Changed(Feed),
    /// With the error, when the stream broke rather than closed.
    Ended(Feed, Option<String>),
    Read(Box<Read>),
    Asked(Box<Asked>),
    Tree(Result<Vec<Change>, String>),
    /// A sign-in's answer, with the token `CancelSignIn` cancels for it.
    SignedIn(CancellationToken, Result<SavedAccount, String>),
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
        true => work.endpoint.subscribe(document).await,
        false => return None,
    };
    Some(
        changes
            .map(StreamExt::boxed)
            .map_err(|error| error.to_string()),
    )
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
    let items = mailbox::served_items(&work.access(), false)
        .await
        .map_err(|error| format!("{error:#}"));
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

/// Ends when the source does, which drops the stream and its connection.
async fn forward<S>(changes: S, feed: Feed, watching: mpsc::UnboundedSender<Watched>)
where
    S: Stream<Item = Result<Value, SubscribeError>>,
{
    let mut changes = std::pin::pin!(changes);
    let ended = loop {
        tokio::select! {
            change = changes.next() => match change {
                Some(Ok(_)) => {
                    let _ = watching.send(Watched::Changed(feed));
                }
                Some(Err(error)) => break Some(error.to_string()),
                None => break None,
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

const PICK_AGAIN: &str = "pick this session again to see the answer to your reply";

/// The status bar clips on the right, so the next step leads and the detail ends.
pub(super) fn closed(error: Option<String>) -> String {
    let error = error.map_or(String::new(), |error| format!(" ({error})"));
    format!("runtime closed: check `gents status`, then reopen the shell{error}")
}

/// A runtime that publishes its runnable agents serves each on its own route. A pinned one
/// publishes none and serves only its bound agent, at `remote`.
fn agent_url(remote: &str, identity: &RuntimeIdentity, agent: &str) -> String {
    match (&identity.runnable_agents, resolve_endpoint(remote)) {
        (Some(_), Ok(RemoteEndpoint::WebSocket { authority, .. })) => {
            format!("ws://{authority}{}", agent_route_target(agent))
        }
        _ => remote.to_string(),
    }
}

struct Source {
    /// In the order the shell lists the agents.
    clients: Vec<(String, Arc<Client>)>,
    updates: Sender<Input>,
    /// Sessions a tab has opened or sent to.
    open: RapidHashSet<String>,
    /// Each listed subagent's parent session.
    parents: RapidHashMap<String, String>,
    /// Threads resumed on their connection, which from then on reports their turns.
    resumed: RapidHashSet<String>,
    /// Each listed session's subagent rows, which its running rows lead with.
    children: RapidHashMap<String, Vec<Running>>,
    work: Result<Work, String>,
    /// This home, whose installed workflows a menu lists even when `work` is off.
    home: PathBuf,
    runtime: Handle,
    watching: mpsc::UnboundedSender<Watched>,
    /// Whether the tool-call subscription is open, so the views need no reread until it says.
    live: bool,
    /// A reread is out, and `again` asks for one more when it lands.
    reading: bool,
    again: bool,
    jobs: Shown<Job>,
    background: Shown<BackgroundCall>,
    /// This home's node, as the runtime names it: the mailbox's requester and replier.
    principal: String,
    /// Whether the mailbox subscription is open, so the prompts need no reread until it says.
    mailbox_live: bool,
    /// A mailbox read is out, and `ask_again` asks for one more when it lands.
    asking: bool,
    ask_again: bool,
    /// A working-tree read is out, and `tree_again` names the agent of one more asked for
    /// meanwhile.
    reading_tree: bool,
    tree_again: Option<String>,
    /// The prompts each open session was last told.
    told: RapidHashMap<String, Vec<Prompt>>,
    /// Listed subagents with an open question or approval, which their parent's row names.
    waiting: RapidHashSet<String>,
    /// The waiting sign-in's token, which `CancelSignIn` takes and cancels. Its answer clears it.
    signing_in: Option<CancellationToken>,
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
            clients: Vec::new(),
            updates,
            open: RapidHashSet::new(),
            parents: RapidHashMap::new(),
            resumed: RapidHashSet::new(),
            children: RapidHashMap::new(),
            work,
            home,
            runtime,
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
            tree_again: None,
            told: RapidHashMap::new(),
            waiting: RapidHashSet::new(),
            signing_in: None,
        }
    }

    /// The shell aborts the source when it ends, so an update it no longer reads is dropped.
    fn tell(&self, update: Update) {
        let _ = self.updates.send(Input::Runtime(update));
    }

    fn lost(&self, why: String) {
        self.tell(Update::Connection(Connection::Lost(why)));
    }

    fn client(&self, agent: &str) -> Option<&Arc<Client>> {
        self.clients
            .iter()
            .find(|(id, _)| id == agent)
            .map(|(_, client)| client)
    }

    async fn handle(&mut self, intent: Intent) {
        match intent {
            Intent::Open { session, agent } => {
                self.open(session, &agent).await;
            }
            Intent::Send {
                tab,
                session,
                agent,
                text,
            } => self.send(tab, session, &agent, text).await,
            Intent::Interrupt {
                session,
                agent,
                turn,
            } => self.interrupt(session, &agent, turn).await,
            Intent::Refresh => self.reread_work(),
            Intent::Dismiss { item, .. } => self.dismiss(&item).await,
            Intent::Reply {
                session,
                agent,
                item,
                text,
            } => self.reply(session, &agent, &item, &text).await,
            Intent::WorkingTree { agent } => self.read_tree(agent),
            Intent::ReadMenu(menu) => self.read_menu(menu),
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
            } => self.answer_setup(Update::Saved, |access, node| async move {
                setup::save(&access, &node, &connection, &model, &settings).await
            }),
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
        let updates = self.updates.clone();
        self.runtime.spawn(async move {
            let answered = match work {
                Ok(work) => answer(work.access(), node).await,
                Err(off) => Err(off),
            };
            let _ = updates.send(Input::Runtime(update(answered)));
        });
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
        self.runtime.spawn(async move {
            let signed = login(cancel.clone(), cancel.child_token()).await;
            let _ = done.send(Watched::SignedIn(cancel, signed));
        });
    }

    /// Off the chat loop, as `read_menu` reads, and then `menu` is reread to show what changed.
    fn act(&self, menu: MenuId, act: MenuAct) {
        let (work, home) = (self.work.clone(), self.home.clone());
        let (principal, updates) = (self.principal.clone(), self.updates.clone());
        self.runtime.spawn(async move {
            let done = match &work {
                Ok(live) => menus::act(live, &home, &principal, &act).await,
                Err(message) => Err(message.clone()),
            };
            let (Ok(notice) | Err(notice)) = done;
            let _ = updates.send(Input::Runtime(Update::Notice(notice)));
            let rows = menus::read_menu(&work, &home, &principal, menu).await;
            let _ = updates.send(Input::Runtime(Update::Menu { menu, rows }));
        });
    }

    /// Off the chat loop, so a slow owner holds up no other intent.
    fn read_menu(&self, menu: MenuId) {
        let (work, home) = (self.work.clone(), self.home.clone());
        let (principal, updates) = (self.principal.clone(), self.updates.clone());
        self.runtime.spawn(async move {
            let rows = menus::read_menu(&work, &home, &principal, menu).await;
            let _ = updates.send(Input::Runtime(Update::Menu { menu, rows }));
        });
    }

    /// One read at a time, off the chat loop, as `reread_work` reads: a slow git diff holds up
    /// no other intent, and one asked for while a read is out runs after it lands, so the
    /// newest answer lands last.
    fn read_tree(&mut self, agent: String) {
        if self.reading_tree {
            self.tree_again = Some(agent);
            return;
        }
        let Some(client) = self.client(&agent).cloned() else {
            return self.tell(Update::WorkingTree(Err("not connected".to_string())));
        };
        self.reading_tree = true;
        let done = self.watching.clone();
        self.runtime.spawn(async move {
            // The runtime resolves a relative folder against its tool root.
            let params = codex::GitDiffToRemoteParams { cwd: ".".into() };
            let read = client
                .request::<_, codex::GitDiffToRemoteResponse>("gitDiffToRemote", &params)
                .await;
            let tree = match read {
                Ok(answer) => Ok(git_changes(&answer.diff)),
                Err(ClientError::Rpc { message, .. }) => Err(message),
                Err(error) => Err(error.to_string()),
            };
            let _ = done.send(Watched::Tree(tree));
        });
    }

    /// `agent` names the connection that sent `note`. A spawn or a send on an open session
    /// relists its subagents, and so does a turn ending on it or on one of them.
    /// After the subscription ends, a turn or subagent change subscribes again.
    async fn notified(&mut self, agent: &str, note: codex::ServerNotification) {
        use codex::ServerNotification as Note;
        let turned = matches!(note, Note::TurnStarted(_) | Note::TurnCompleted(_));
        let mut relist = Vec::new();
        match &note {
            Note::TurnCompleted(completed) => {
                let thread = &completed.thread_id;
                relist.extend(self.open.get(thread).cloned());
                relist.extend(self.parents.get(thread).cloned());
            }
            Note::ItemStarted(codex::ItemStartedNotification {
                thread_id, item, ..
            })
            | Note::ItemCompleted(codex::ItemCompletedNotification {
                thread_id, item, ..
            }) if matches!(item, codex::ThreadItem::CollabAgentToolCall { .. })
                && self.open.contains(thread_id) =>
            {
                relist.push(thread_id.clone());
            }
            _ => {}
        }
        let ended = matches!(note, Note::TurnCompleted(_));
        for update in items::updates(note) {
            self.tell(update);
        }
        if ended {
            self.tell(Update::Sessions(self.sessions().await));
        }
        let relisted = !relist.is_empty();
        for session in relist {
            self.subagents(&session, agent).await;
        }
        if !self.live && (turned || relisted) {
            self.reread_work();
        }
        if !self.mailbox_live && turned {
            self.reread_prompts();
        }
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
        self.runtime.spawn(async move {
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
        self.runtime.spawn(async move {
            let asked = read_mailbox(work, subscribe).await;
            let _ = done.send(Watched::Asked(Box::new(asked)));
        });
    }

    fn watched(&mut self, event: Watched) {
        match event {
            Watched::Changed(Feed::Work) => self.reread_work(),
            Watched::Changed(Feed::Mailbox) => self.reread_prompts(),
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
            Watched::Read(read) => self.landed(*read),
            Watched::Asked(asked) => self.asked(*asked),
            Watched::Tree(tree) => {
                self.reading_tree = false;
                self.tell(Update::WorkingTree(tree));
                if let Some(agent) = self.tree_again.take() {
                    self.read_tree(agent);
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

    /// Every agent's threads, newest first.
    async fn sessions(&self) -> Vec<Session> {
        let today = chrono::Local::now().date_naive();
        let mut threads = Vec::new();
        for (agent, client) in &self.clients {
            let listed = self.threads(client, None).await;
            threads.extend(listed.into_iter().map(|thread| (thread, agent)));
        }
        threads.sort_by_key(|(thread, _)| Reverse(thread.updated_at));
        threads
            .iter()
            .map(|(thread, agent)| items::session(thread, agent, today))
            .collect()
    }

    /// Every page of the connection's threads of `source_kinds`. A failed page becomes a notice.
    async fn threads(
        &self,
        client: &Client,
        source_kinds: Option<Vec<codex::ThreadSourceKind>>,
    ) -> Vec<codex::Thread> {
        let mut threads = Vec::new();
        let mut cursor = None;
        loop {
            let params = codex::ThreadListParams {
                cursor: cursor.take(),
                limit: Some(100),
                sort_key: Some(codex::ThreadSortKey::UpdatedAt),
                sort_direction: None,
                model_providers: None,
                source_kinds: source_kinds.clone(),
                archived: None,
                cwd: None,
                use_state_db_only: false,
                search_term: None,
            };
            let page = client
                .request::<_, codex::ThreadListResponse>("thread/list", &params)
                .await;
            match page {
                Ok(page) => {
                    threads.extend(page.data);
                    cursor = page.next_cursor;
                }
                Err(error) => self.tell(Update::Notice(error.to_string())),
            }
            if cursor.is_none() {
                return threads;
            }
        }
    }

    /// The endpoint lists and resumes a subagent only on the connection of its root session's
    /// agent, whatever agent the subagent runs, so the parent's connection serves it. Its rows
    /// come oldest first. Each working subagent is resumed once: only a connection that has
    /// resumed a subagent reports its turns, so that is how its end is heard.
    async fn subagents(&mut self, session: &str, agent: &str) {
        let Some(client) = self.client(agent) else {
            return;
        };
        let kinds = vec![
            codex::ThreadSourceKind::SubAgent,
            codex::ThreadSourceKind::SubAgentThreadSpawn,
        ];
        let mut children = self.threads(client, Some(kinds)).await;
        children.retain(|thread| items::parent(thread) == Some(session));
        children.sort_by_key(|thread| thread.created_at);
        let mut rows = Vec::with_capacity(children.len());
        for listed in children {
            let working = matches!(listed.status, codex::ThreadStatus::Active { .. });
            let thread = if working && !self.resumed.contains(&listed.id) {
                match self.resume(agent, &listed.id).await {
                    Ok(resumed) => {
                        self.resumed.insert(resumed.id.clone());
                        resumed
                    }
                    Err(message) => {
                        self.tell(Update::Notice(message));
                        listed
                    }
                }
            } else {
                listed
            };
            self.parents.insert(thread.id.clone(), session.to_string());
            rows.push(items::subagent(&thread));
        }
        self.children.insert(session.to_string(), rows);
        self.tell(Update::Running {
            session: session.to_string(),
            rows: self.running(session),
        });
    }

    /// Resume returns every turn. Asking it to exclude them would also drop the baseline the
    /// runtime continues a running turn from.
    async fn resume(&self, agent: &str, thread: &str) -> Result<codex::Thread, String> {
        let client = self.client(agent).ok_or_else(|| not_connected(agent))?;
        let params = codex::ThreadResumeParams {
            thread_id: thread.to_string(),
            ..Default::default()
        };
        client
            .request::<_, codex::ThreadResumeResponse>("thread/resume", &params)
            .await
            .map(|resumed| resumed.thread)
            .map_err(|error| error.to_string())
    }

    /// Whether the session has a turn in progress.
    async fn open(&mut self, session: String, agent: &str) -> bool {
        let thread = match self.resume(agent, &session).await {
            Ok(thread) => thread,
            Err(message) => {
                self.tell(Update::Notice(message));
                return false;
            }
        };
        if self.open.insert(session.clone()) {
            self.reread_work();
        }
        self.resumed.insert(session.clone());
        let turns = thread.turns;
        let running = turns
            .iter()
            .rfind(|turn| turn.status == codex::TurnStatus::InProgress);
        // Told before the transcript, so the shell never draws the running turn's card.
        if let Some(running) = running {
            self.tell(Update::Turn {
                session: session.clone(),
                turn: running.id.clone(),
            });
        }
        let lines = turns
            .iter()
            .flat_map(|turn| {
                turn.items
                    .iter()
                    .flat_map(|item| items::lines(item, &turn.id))
            })
            .flat_map(|line| [line, blank()])
            .collect();
        self.tell(Update::Opened {
            session: session.clone(),
            lines,
        });
        self.subagents(&session, agent).await;
        // A new tab shows no prompts until it is told them.
        self.told.remove(&session);
        self.reread_prompts();
        running.is_some()
    }

    /// Through the served mailbox owner, which refuses a question that needs an answer.
    async fn dismiss(&mut self, item: &str) {
        let Ok(work) = &self.work else {
            return;
        };
        let dismissed = gents::session::mailbox::dismiss_mailbox_item_on(&work.access(), item).await;
        if let Err(error) = dismissed {
            self.tell(Update::Notice(format!("{error:#}")));
        }
        self.reread_prompts();
    }

    /// The reply is a request caused by the item. The endpoint streams no other client's
    /// request, so the session is resumed once to show the reply, and its answer when it is
    /// already there.
    async fn reply(&mut self, session: String, agent: &str, item: &str, text: &str) {
        let Ok(work) = self.work.clone() else {
            return;
        };
        let replied = async {
            let item = mailbox::load_item(&work.access(), item)
                .await?
                .context("MailboxItem not found")?;
            let reply = work::reply_from_text(text, &item).map_err(anyhow::Error::msg)?;
            mailbox::reply_to(&work.endpoint, &item, reply).await
        };
        match replied.await {
            Ok(_) => self.replied(session, agent).await,
            Err(error) => {
                self.tell(Update::Notice(format!("{error:#}")));
                self.reread_prompts();
            }
        }
    }

    async fn replied(&mut self, session: String, agent: &str) {
        if self.open(session, agent).await {
            self.tell(Update::Notice(PICK_AGAIN.to_string()));
        }
    }

    /// A tab with no session gets one first, and `Started` or `StartFailed` either way.
    async fn send(&mut self, tab: u64, session: Option<String>, agent: &str, text: String) {
        let Some(client) = self.client(agent) else {
            let message = not_connected(agent);
            return self.tell(Update::StartFailed { tab, message });
        };
        let session = match session {
            Some(session) => session,
            None => {
                let params = codex::ThreadStartParams::default();
                match client
                    .request::<_, codex::ThreadStartResponse>("thread/start", &params)
                    .await
                {
                    Ok(started) => {
                        let session = started.thread.id;
                        self.tell(Update::Started {
                            tab,
                            session: session.clone(),
                        });
                        session
                    }
                    Err(error) => {
                        let message = error.to_string();
                        return self.tell(Update::StartFailed { tab, message });
                    }
                }
            }
        };
        let params = codex::TurnStartParams {
            thread_id: session.clone(),
            input: vec![codex::UserInput::Text {
                text,
                text_elements: Vec::new(),
            }],
            ..Default::default()
        };
        let started = client
            .request::<_, codex::TurnStartResponse>("turn/start", &params)
            .await;
        if self.open.insert(session.clone()) {
            self.reread_work();
        }
        match started {
            Ok(started) => self.tell(Update::Turn {
                session,
                turn: started.turn.id,
            }),
            Err(error) => self.tell(Update::StartFailed {
                tab,
                message: error.to_string(),
            }),
        }
    }

    /// The endpoint interrupts the thread's active turn whatever id it is given, so a turn
    /// that has ended is only cleared: interrupting it would stop another client's newer turn.
    async fn interrupt(&self, session: String, agent: &str, turn: String) {
        let Some(client) = self.client(agent) else {
            return self.tell(Update::Notice(not_connected(agent)));
        };
        let params = codex::ThreadReadParams {
            thread_id: session.clone(),
            include_turns: true,
        };
        let read = match client
            .request::<_, codex::ThreadReadResponse>("thread/read", &params)
            .await
        {
            Ok(read) => read,
            Err(error) => return self.tell(Update::Notice(error.to_string())),
        };
        let running = read
            .thread
            .turns
            .iter()
            .any(|shown| shown.id == turn && shown.status == codex::TurnStatus::InProgress);
        if running {
            let params = codex::TurnInterruptParams {
                thread_id: session.clone(),
                turn_id: turn.clone(),
            };
            if let Err(error) = client
                .request::<_, codex::TurnInterruptResponse>("turn/interrupt", &params)
                .await
            {
                return self.tell(Update::Notice(error.to_string()));
            }
        }
        self.tell(Update::TurnEnded { session, turn });
    }
}

fn not_connected(agent: &str) -> String {
    format!("agent {agent} is not connected to this shell")
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
    use serde_json::json;

    use super::super::tests::{
        agents_a_and_b, connect, quiet, resumes_of, thread, thread_answer, turn, user_message,
        within, FakeRuntime,
    };
    use super::*;
    use crate::commands::eval::testing::test_runtime_handle;

    /// The source after a reply to `s1` was submitted, on agent `a`, whose resume shows the reply
    /// as the turn `r9` in `status`.
    async fn after_a_reply(status: &'static str) -> (FakeRuntime, Vec<Update>) {
        let fake = FakeRuntime::start(agents_a_and_b(), move |_, method, params| match method {
            "thread/resume" => {
                let mut shown = thread(params["threadId"].as_str().unwrap(), 100);
                let reply = user_message("u9", "Decision on Release channel: Stable (stable)");
                shown["turns"] = json!([turn("r9", status, json!([reply]))]);
                Some(Ok(thread_answer(shown)))
            }
            _ => quiet(method),
        })
        .await;
        let (events, _received) = mpsc::unbounded_channel();
        let client = connect(&fake, "a", events).await;
        let (updates, told) = std::sync::mpsc::channel();
        let (watching, _watched) = mpsc::unbounded_channel();
        let off = Err(String::new());
        let mut source = Source::new(
            updates,
            off,
            PathBuf::new(),
            test_runtime_handle(),
            watching,
        );
        source.clients.push(("a".to_string(), Arc::new(client)));

        source.replied("s1".to_string(), "a").await;

        let told = told.try_iter().map(|input| match input {
            Input::Runtime(update) => update,
            other => panic!("{other:?}"),
        });
        (fake, told.collect())
    }

    #[tokio::test]
    async fn a_reply_resumes_its_session_once_and_says_to_pick_it_again_while_it_runs() {
        let (fake, told) = after_a_reply("inProgress").await;

        assert_eq!(resumes_of(&fake, "s1"), 1);
        assert!(
            matches!(&told[1], Update::Opened { session, lines } if session == "s1" && lines[0].text.starts_with("Decision on")),
            "{told:?}"
        );
        let running = Update::Turn {
            session: "s1".to_string(),
            turn: "r9".to_string(),
        };
        assert_eq!(told[0], running, "ctrl-c can stop the answer: {told:?}");
        let pick = Update::Notice(PICK_AGAIN.to_string());
        assert_eq!(told.last(), Some(&pick), "{told:?}");
    }

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
    async fn an_answered_reply_says_nothing_more() {
        let (_, told) = after_a_reply("completed").await;

        assert!(
            !told
                .iter()
                .any(|update| matches!(update, Update::Notice(_))),
            "{told:?}"
        );
    }
}
