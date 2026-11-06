use std::cmp::Reverse;
use std::sync::mpsc::Sender;
use std::time::Duration;

use gents_codex_protocol as codex;
use gents_shell::app::ChatLine;
use gents_shell::sample::Tone;
use gents_shell::source::{Connection, Input, Intent, Session, Update};
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
            Some((_, event)) = received.recv() => match event {
                ClientEvent::Notification(note) => source.notified(*note).await,
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

    async fn handle(&self, intent: Intent) {
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

    async fn notified(&self, note: codex::ServerNotification) {
        let relist = matches!(note, codex::ServerNotification::TurnCompleted(_));
        for update in items::updates(note) {
            self.tell(update);
        }
        if relist {
            self.tell(Update::Sessions(self.sessions().await));
        }
    }

    /// Every agent's threads, newest first.
    async fn sessions(&self) -> Vec<Session> {
        let today = chrono::Local::now().date_naive();
        let mut threads = Vec::new();
        for (agent, client) in &self.clients {
            let mut cursor = None;
            loop {
                let params = codex::ThreadListParams {
                    cursor: cursor.take(),
                    limit: Some(100),
                    sort_key: Some(codex::ThreadSortKey::UpdatedAt),
                    sort_direction: None,
                    model_providers: None,
                    source_kinds: None,
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
                        threads.extend(page.data.into_iter().map(|thread| (thread, agent)));
                        cursor = page.next_cursor;
                    }
                    Err(error) => self.tell(Update::Notice(error.to_string())),
                }
                if cursor.is_none() {
                    break;
                }
            }
        }
        threads.sort_by_key(|(thread, _)| Reverse(thread.updated_at));
        threads
            .iter()
            .map(|(thread, agent)| items::session(thread, agent, today))
            .collect()
    }

    /// Resume returns every turn. Asking it to exclude them would also drop the baseline the
    /// runtime continues a running turn from.
    async fn open(&self, session: String, agent: &str) {
        let Some(client) = self.client(agent) else {
            return self.tell(Update::Notice(not_connected(agent)));
        };
        let params = codex::ThreadResumeParams {
            thread_id: session.clone(),
            ..Default::default()
        };
        let resumed = match client
            .request::<_, codex::ThreadResumeResponse>("thread/resume", &params)
            .await
        {
            Ok(resumed) => resumed,
            Err(error) => return self.tell(Update::Notice(error.to_string())),
        };
        let turns = resumed.thread.turns;
        let lines = turns
            .iter()
            .flat_map(|turn| &turn.items)
            .flat_map(items::lines)
            .flat_map(|line| [line, blank()])
            .collect();
        self.tell(Update::Opened {
            session: session.clone(),
            lines,
            running: &[],
        });
        if let Some(running) = turns
            .iter()
            .rfind(|turn| turn.status == codex::TurnStatus::InProgress)
        {
            self.tell(Update::Turn {
                session,
                turn: running.id.clone(),
            });
        }
    }

    /// A tab with no session gets one first, and `Started` or `StartFailed` either way.
    async fn send(&self, tab: u64, session: Option<String>, agent: &str, text: String) {
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
        match client
            .request::<_, codex::TurnStartResponse>("turn/start", &params)
            .await
        {
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
