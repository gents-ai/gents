use std::cmp::Reverse;
use std::sync::mpsc::Sender;
use std::time::Duration;

use gents_codex_protocol as codex;
use gents_shell::app::ChatLine;
use gents_shell::sample::Tone;
use gents_shell::source::{Connection, Input, Intent, Session, Update};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use super::client::{Client, ClientEvent};
use super::items;
use crate::commands::codex::relay::agent_route_target;
use crate::commands::codex::{resolve_endpoint, runtime_identity_at, RemoteEndpoint};
use crate::commands::codex_shim::RuntimeIdentity;

/// Feeds the shell from the runtime at `remote`, over one connection per agent, waiting
/// `answer_wait` for each answer. A given `agent` or `prompt` follows the agents.
///
/// Each intent is answered before the next notification is read. The shell drops a tab's
/// updates until `Started` binds its session, and `Opened` replaces a tab's lines, so both
/// must reach it before any notification that arrived while their request was out.
pub(super) async fn run(
    remote: String,
    agent: Option<String>,
    prompt: Option<String>,
    mut intents: mpsc::UnboundedReceiver<Intent>,
    updates: Sender<Input>,
    runtime: Handle,
    answer_wait: Duration,
) {
    let mut source = Source {
        clients: Vec::new(),
        updates,
        open: RapidHashSet::new(),
        parents: RapidHashMap::new(),
        resumed: RapidHashSet::new(),
    };
    let Some(identity) = runtime_identity_at(&remote).await else {
        return source.lost(format!(
            "no runtime: check `gents status`, then reopen the shell (could not read this home's runtime at {remote})"
        ));
    };
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
            Ok(client) => source.clients.push((agent.id.clone(), client)),
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
        }
    }
}

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
    clients: Vec<(String, Client)>,
    updates: Sender<Input>,
    /// Sessions a tab has opened or sent to.
    open: RapidHashSet<String>,
    /// Each listed subagent's parent session.
    parents: RapidHashMap<String, String>,
    /// Threads resumed on their connection, which from then on reports their turns.
    resumed: RapidHashSet<String>,
}

impl Source {
    /// The shell aborts the source when it ends, so an update it no longer reads is dropped.
    fn tell(&self, update: Update) {
        let _ = self.updates.send(Input::Runtime(update));
    }

    fn lost(&self, why: String) {
        self.tell(Update::Connection(Connection::Lost(why)));
    }

    fn client(&self, agent: &str) -> Option<&Client> {
        self.clients
            .iter()
            .find(|(id, _)| id == agent)
            .map(|(_, client)| client)
    }

    async fn handle(&mut self, intent: Intent) {
        match intent {
            Intent::Open { session, agent } => self.open(session, &agent).await,
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
        }
    }

    /// `agent` names the connection that sent `note`. A spawn or a send on an open session
    /// relists its subagents, and so does a turn ending on it or on one of them.
    async fn notified(&mut self, agent: &str, note: codex::ServerNotification) {
        use codex::ServerNotification as Note;
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
        for session in relist {
            self.subagents(&session, agent).await;
        }
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
        self.tell(Update::Running {
            session: session.to_string(),
            rows,
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

    async fn open(&mut self, session: String, agent: &str) {
        let thread = match self.resume(agent, &session).await {
            Ok(thread) => thread,
            Err(message) => return self.tell(Update::Notice(message)),
        };
        self.open.insert(session.clone());
        self.resumed.insert(session.clone());
        let turns = thread.turns;
        let lines = turns
            .iter()
            .flat_map(|turn| &turn.items)
            .flat_map(items::lines)
            .flat_map(|line| [line, blank()])
            .collect();
        self.tell(Update::Opened {
            session: session.clone(),
            lines,
        });
        if let Some(running) = turns
            .iter()
            .rfind(|turn| turn.status == codex::TurnStatus::InProgress)
        {
            self.tell(Update::Turn {
                session: session.clone(),
                turn: running.id.clone(),
            });
        }
        self.subagents(&session, agent).await;
    }

    /// A tab with no session gets one first, and `Started` or `StartFailed` either way.
    async fn send(&mut self, tab: u64, session: Option<String>, agent: &str, text: String) {
        let Some(client) = self.client(agent) else {
            let message = not_connected(agent);
            return self.tell(match session {
                None => Update::StartFailed { tab, message },
                Some(_) => Update::Notice(message),
            });
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
        self.open.insert(session.clone());
        match started {
            Ok(started) => self.tell(Update::Turn {
                session,
                turn: started.turn.id,
            }),
            Err(error) => self.tell(Update::Notice(error.to_string())),
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
    }
}
