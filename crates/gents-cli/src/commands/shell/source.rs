use std::cmp::Reverse;
use std::future::Future;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Local, Utc};
use gents::session::mailbox::MailboxItem;
use gents::store::config_client::{ConfigAccess, GraphqlSubscription, SubscriptionEnd};
use gents_codex_protocol as codex;
use gents_shell::app::ChatLine;
use gents_shell::keys::KeySet;
use gents_shell::sample::{MenuId, Opens, RunState, Running, Tone};
use gents_shell::source::{
    BackgroundCall, Change, Connection, Doc, FormRef, FormValues, Input, Intent, Job, MenuAct,
    PackAct, Prompt, SavedAccount, Session, Skill, Update,
};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::client::{Client, ClientError, ClientEvent};
use super::sections::{self, new_id};
use super::tree::git_changes;
use super::work::{self, Work};
use super::{forms, items, menus, packs, prefs, setup, usage};
use crate::commands::accounts::account_rows;
use crate::commands::codex::relay::agent_route_target;
use crate::commands::codex::{resolve_endpoint, runtime_identity_at, RemoteEndpoint};
use crate::commands::codex_shim::{thread_git_info, RuntimeIdentity};
use crate::commands::mailbox;

/// What the shell opens with. `work` is why jobs and the mailbox are off when it is `Err`.
pub(super) struct Opening {
    pub(super) remote: String,
    pub(super) agent: Option<String>,
    pub(super) prompt: Option<String>,
    /// `gents --continue`'s session, which `agent` then names the agent of.
    pub(super) session: Option<String>,
    pub(super) work: Result<Work, String>,
    pub(super) home: PathBuf,
    pub(super) cwd: Option<PathBuf>,
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
        session,
        work,
        home,
        cwd,
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
    // `Begin` or `Continue` must reach the shell before a send the user makes once the agents
    // are known. Both go through one queue, so it is told before the sweep awaits anything.
    match session {
        Some(session) => source.tell(Update::Continue { session, agent }),
        None if agent.is_some() || prompt.is_some() => {
            source.tell(Update::Begin { agent, prompt });
        }
        None => {}
    }
    source.tell(Update::Connection(Connection::Ready));
    // Read beside startup, never ahead of it: git may hang for its timeout. Once only, so a
    // checkout during the session shows at the next start.
    if let Some(cwd) = cwd {
        let updates = source.updates.clone();
        tokio::spawn(async move {
            let branch = thread_git_info(&cwd).await.and_then(|info| {
                info.get("branch")
                    .and_then(|branch| branch.as_str().map(str::to_string))
            });
            if let Some(branch) = branch {
                let _ = updates.send(Input::Runtime(Update::Branch(branch)));
            }
        });
    }
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

type Changes = GraphqlSubscription;

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
    /// The subscription's bearer is due for renewal: subscribe again and reread.
    Renew(Feed),
    Read(Box<Read>),
    Asked(Box<Asked>),
    Tree(Result<Vec<Change>, String>),
    /// A sign-in's answer, with the token `CancelSignIn` cancels for it.
    SignedIn(CancellationToken, Result<SavedAccount, String>),
    /// A pack change succeeded, so Packs is read again with the search shown by then.
    PackChanged,
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

const PICK_AGAIN: &str = "pick this session again to see the answer to your reply";

/// The goal's first request is written to the node, not started as this connection's turn.
pub(super) const GOAL_SET: &str = "pick this session again to follow the goal";

/// A pack change asked for while another runs.
const BUSY: &str = "A pack change is still running";

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
    /// The id each kind's new form last opened with.
    new_ids: RapidHashMap<Doc, String>,
    /// The registry search Packs shows: what was searched for, and the page shown.
    pack_query: String,
    pack_page: u32,
    /// Held through each pack change, so another is refused while it runs.
    pack_changes: Arc<tokio::sync::Mutex<()>>,
    /// Each agent's last listed skills, whose entry a skill turn sends.
    skills: RapidHashMap<String, Vec<codex::SkillMetadata>>,
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
            new_ids: RapidHashMap::new(),
            pack_query: String::new(),
            pack_page: 1,
            pack_changes: Arc::default(),
            skills: RapidHashMap::new(),
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
            } => {
                let text = codex::UserInput::Text {
                    text,
                    text_elements: Vec::new(),
                };
                self.send(tab, session, &agent, vec![text]).await
            }
            Intent::ReadSkills { agent } => self.read_skills(agent).await,
            Intent::SendSkill {
                tab,
                session,
                agent,
                skill,
                text,
            } => {
                let mut listed = self.skills.get(&agent).into_iter().flatten();
                let Some(listed) = listed.find(|listed| skill_id(listed) == skill) else {
                    let message = format!("unknown skill /{skill}");
                    return self.tell(Update::StartFailed { tab, message });
                };
                let mut input = vec![codex::UserInput::Skill {
                    name: listed.name.clone(),
                    path: listed.path.to_path_buf(),
                }];
                if !text.is_empty() {
                    input.push(codex::UserInput::Text {
                        text,
                        text_elements: Vec::new(),
                    });
                }
                self.send(tab, session, &agent, input).await
            }
            Intent::SetGoal {
                tab,
                session,
                agent,
                objective,
            } => self.goal(tab, session, &agent, objective).await,
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
            } => self.answer_setup(Update::Saved, |access, node| async move {
                setup::save(&access, &node, &connection, &model, &settings).await
            }),
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
                self.answer_setup(saved, |access, node| async move {
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
        // A profile is chosen from Profiles, but it shows on the Agents menu.
        let menu = match act {
            MenuAct::SetAgentProfile { .. } => MenuId::Agents,
            _ => menu,
        };
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
        self.runtime.spawn(async move {
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
        self.runtime.spawn(async move {
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
        self.runtime.spawn(async move {
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
        self.runtime.spawn(async move {
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
        self.runtime.spawn(async move {
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
            Watched::Renew(Feed::Work) => {
                self.live = false;
                self.reread_work();
            }
            Watched::Renew(Feed::Mailbox) => {
                self.mailbox_live = false;
                self.reread_prompts();
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
            Watched::PackChanged => self.read_menu(MenuId::Packs),
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
        let dismissed =
            gents::session::mailbox::dismiss_mailbox_item_on(&work.access(), item).await;
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

    /// The endpoint lists every node skill; the context read narrows them to the agent's, as
    /// admission does. With work off it cannot, and admission refuses one outside the context.
    async fn read_skills(&mut self, agent: String) {
        let Some(client) = self.client(&agent) else {
            return self.tell(Update::Notice(not_connected(&agent)));
        };
        let params = codex::SkillsListParams::default();
        let listed = match client
            .request::<_, codex::SkillsListResponse>("skills/list", &params)
            .await
        {
            Ok(listed) => listed.data,
            Err(error) => return self.skills_unread(agent, format!("{error:#}")),
        };
        if let Some(error) = listed.iter().flat_map(|entry| &entry.errors).next() {
            let message = error.message.clone();
            return self.skills_unread(agent, message);
        }
        let mut skills: Vec<_> = listed.into_iter().flat_map(|entry| entry.skills).collect();
        skills.retain(|skill| skill.enabled);
        if let Ok(work) = &self.work {
            match forms::skill::allowed(&work.access(), &self.principal, &agent).await {
                Ok(allowed) => {
                    skills.retain(|skill| allowed.iter().any(|id| id == skill_id(skill)))
                }
                Err(error) => return self.skills_unread(agent, format!("{error:#}")),
            }
        }
        let rows = skills.iter().map(|skill| Skill {
            id: skill_id(skill).to_string(),
            about: match skill.description.as_str() {
                "" => skill.name.clone(),
                description => description.to_string(),
            },
        });
        let rows = rows.collect();
        self.skills.insert(agent.clone(), skills);
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

    /// The session to start on: `session`, or a new one told as `Started`. `None` once a failed
    /// start is told as `StartFailed`.
    async fn session_for(
        &self,
        tab: u64,
        session: Option<String>,
        client: &Client,
    ) -> Option<String> {
        if session.is_some() {
            return session;
        }
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
                Some(session)
            }
            Err(error) => {
                let message = error.to_string();
                self.tell(Update::StartFailed { tab, message });
                None
            }
        }
    }

    /// A tab with no session gets one first, and `Started` or `StartFailed` either way.
    async fn send(
        &mut self,
        tab: u64,
        session: Option<String>,
        agent: &str,
        input: Vec<codex::UserInput>,
    ) {
        let Some(client) = self.client(agent) else {
            let message = not_connected(agent);
            return self.tell(Update::StartFailed { tab, message });
        };
        let Some(session) = self.session_for(tab, session, client).await else {
            return;
        };
        let params = codex::TurnStartParams {
            thread_id: session.clone(),
            input,
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

    /// The goal and its first request, whose content is the objective, commit together through
    /// the goal owner, which refuses replacing a live goal.
    async fn goal(&mut self, tab: u64, session: Option<String>, agent: &str, objective: String) {
        let work = match self.work.clone() {
            Ok(work) => work,
            Err(message) => return self.tell(Update::StartFailed { tab, message }),
        };
        let Some(client) = self.client(agent) else {
            let message = not_connected(agent);
            return self.tell(Update::StartFailed { tab, message });
        };
        let Some(session) = self.session_for(tab, session, client).await else {
            return;
        };
        let principal = &self.principal;
        let submitted = async {
            // The owner requires a retry key, by which it recognises a retried submission.
            let request_id = uuid::Uuid::new_v4().to_string();
            let options = crate::RequestSubmitOptions {
                retry_key: Some(format!("goal-request:{request_id}")),
                ..Default::default()
            };
            let prepared = crate::prepare_agent_request_with_input(
                &work.endpoint,
                principal,
                &objective,
                Some(&session),
                Some(agent),
                Some(request_id),
                options,
            )
            .await?;
            let access = work.access();
            let create = &prepared.create;
            gents::goal::submit_goal_backed_request(
                &access, principal, &session, &objective, None, create,
            )
            .await
        };
        match submitted.await {
            Ok(_) => {
                self.open(session, agent).await;
                self.tell(Update::Sessions(self.sessions().await));
                self.tell(Update::Notice(GOAL_SET.to_string()));
            }
            Err(error) => self.tell(Update::StartFailed {
                tab,
                message: format!("Couldn’t set the goal: {error:#}"),
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

/// The endpoint lists a skill at a path named for its id, which a skill input is read back by.
fn skill_id(skill: &codex::SkillMetadata) -> &str {
    let name = skill.path.file_name().and_then(|name| name.to_str());
    name.unwrap_or_default()
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
    use gents::pack::PackChangeError;
    use serde_json::json;

    use super::super::tests::{
        agents_a_and_b, connect, home_serving, quiet, resumes_of, thread, thread_answer, turn,
        user_message, within, FakeRuntime,
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
    fn without_runtime() -> (
        Source,
        Told,
        mpsc::UnboundedReceiver<Watched>,
        [tempfile::TempDir; 2],
    ) {
        let keys = tempfile::tempdir().unwrap();
        let key = keys.path().join("principal.key");
        let (home, _) = home_serving("http://127.0.0.1:1/api/v0/graphql", Some(&key));
        let work = work::work_for(home.path());
        assert!(work.is_ok(), "the home's node signs: {:?}", work.err());
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
        let (mut source, told, _watched, _dirs) = without_runtime();

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
        let (mut source, told, watched, [home, _]) = without_runtime();

        source.handle(install_git()).await;

        assert_eq!(next(&told).await, installing_git());
        assert_eq!(next(&told).await, Update::Notice(closed(None)));
        assert!(
            !reread_asked(source, watched).await,
            "a failure reads nothing"
        );
        let data = crate::home_state::default_data_dir(home.path());
        assert!(!data.exists(), "a pack call opened the store");
    }

    #[tokio::test]
    async fn a_refusal_asks_and_reads_nothing_again() {
        use gents_shell::source::EditedChoice::Refuse;
        let (source, told, watched, _dirs) = without_runtime();
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
        let (mut source, told, mut watched, _dirs) = without_runtime();
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
        let (mut source, told, _watched, _dirs) = without_runtime();
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
        let (mut source, told, _watched, _dirs) = without_runtime();
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
        let (mut source, told, _watched, _dirs) = without_runtime();
        source.pack_page = 3;

        source
            .handle(pack(PackAct::Keyword("vcs".to_string())))
            .await;

        assert_eq!((source.pack_query.as_str(), source.pack_page), ("vcs", 1));
        let rows = Err(closed(None));
        let menu = MenuId::Packs;
        assert_eq!(next(&told).await, Update::Menu { menu, rows });
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
