use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gents::pack::PackChangeError;
use gents::plugin::authority::GrantError;
use gents::store::config_client::GraphqlEndpoint;
use gents_codex_protocol as codex;
use gents_shell::app::{App, ChatLine};
use gents_shell::keys::KeySet;
use gents_shell::sample::{MenuId, Opens, RunState, Running, Tone, View};
use gents_shell::source::{
    Answer as PromptAnswer, BackgroundCall, Change, ChangeKind, Connection, Doc, EditedChoice,
    FormError, FormRef, FormValues, Input, Intent, Job, JobEnd, MenuAct, MenuRow, PackAct, Prompt,
    PromptKind, Session, Skill, Update,
};
use rapidhash::RapidHashMap;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use super::client::{Client, ClientError, ClientEvent, ANSWER_WAIT};
use super::source::{self, Opening};
use super::testing::{question, shown_rows};
use super::work::{self, Work};
use crate::commands::codex::tests::identity;
use crate::commands::codex_shim::RuntimeIdentity;
use crate::commands::config::crud::{
    load_one, query_collection, query_collection_sorted, MCP_SPEC, NODE_SPEC, SCHEDULE_SPEC,
    SKILL_SPEC, TRIGGER_SPEC,
};
use crate::commands::eval::testing::test_runtime_handle;

const WAIT: Duration = Duration::from_secs(10);

/// `None` leaves the request unanswered.
pub(super) type Answer = Option<Result<Value, (i64, String)>>;
type Script = Box<dyn Fn(&str, &str, &Value) -> Answer + Send + Sync>;
/// Answers a GraphQL query, given its text.
type Graphql = Box<dyn Fn(&str) -> Value + Send + Sync>;
type Chunks = mpsc::UnboundedSender<Result<Bytes, std::io::Error>>;

#[derive(Debug, Clone)]
pub(super) struct Call {
    pub(super) agent: String,
    /// `None` on a notification.
    pub(super) id: Option<Value>,
    pub(super) method: String,
    pub(super) params: Value,
    pub(super) headers: HeaderMap,
}

struct Shared {
    identity: Option<RuntimeIdentity>,
    script: Script,
    calls: watch::Sender<Vec<Call>>,
    sockets: Mutex<RapidHashMap<String, mpsc::UnboundedSender<Message>>>,
    graphql: Graphql,
    /// Every GraphQL document posted, subscriptions included, in order.
    queries: watch::Sender<Vec<String>>,
    /// The open subscriptions' documents and event streams.
    streams: Mutex<Vec<(String, Chunks)>>,
    /// While true, every query waits before it is answered.
    held: watch::Sender<bool>,
}

/// `/runtime` answers `identity`, or fails without one. `/agents/{id}` serves that agent and
/// `/` the bound one, whose calls it records under the agent `/`. Each answers `initialize`
/// itself and every other request from `script`, given the agent, the method and the params.
/// `/api/v0/graphql` answers each query from a second script and holds each subscription open.
pub(super) struct FakeRuntime {
    shared: Arc<Shared>,
    authority: String,
}

/// No jobs and no background calls.
fn no_work(_: &str) -> Value {
    json!({ "data": { "AgentToolCall": [] } })
}

impl FakeRuntime {
    pub(super) async fn start<F>(identity: Option<RuntimeIdentity>, script: F) -> FakeRuntime
    where
        F: Fn(&str, &str, &Value) -> Answer + Send + Sync + 'static,
    {
        Self::start_with(identity, script, no_work).await
    }

    pub(super) async fn start_with<F, G>(
        identity: Option<RuntimeIdentity>,
        script: F,
        graphql: G,
    ) -> FakeRuntime
    where
        F: Fn(&str, &str, &Value) -> Answer + Send + Sync + 'static,
        G: Fn(&str) -> Value + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        let shared = Arc::new(Shared {
            identity,
            script: Box::new(script),
            calls: watch::channel(Vec::new()).0,
            sockets: Mutex::default(),
            graphql: Box::new(graphql),
            queries: watch::channel(Vec::new()).0,
            streams: Mutex::default(),
            held: watch::channel(false).0,
        });
        let app = Router::new()
            .route("/runtime", get(runtime))
            .route("/", get(root))
            .route("/agents/{id}", get(routed))
            .route("/api/v0/graphql", post(serve_graphql))
            .route("/api/v0/tx", post(|| async { Json(json!({ "id": 1 })) }))
            .route(
                "/api/v0/tx/{id}",
                post(|| async { "" }).delete(|| async { "" }),
            )
            .with_state(shared.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        FakeRuntime { shared, authority }
    }

    pub(super) fn remote(&self) -> String {
        format!("ws://{}/", self.authority)
    }

    pub(super) fn route(&self, agent: &str) -> String {
        format!("ws://{}/agents/{agent}", self.authority)
    }

    pub(super) fn graphql(&self) -> String {
        format!("http://{}/api/v0/graphql", self.authority)
    }

    pub(super) fn queries(&self) -> Vec<String> {
        self.shared.queries.borrow().clone()
    }

    fn subscriptions(&self) -> usize {
        let queries = self.queries();
        queries
            .iter()
            .filter(|query| query.starts_with("subscription"))
            .count()
    }

    /// Returns once `count` posted GraphQL documents contain `text`.
    async fn posted(&self, text: &str, count: usize) {
        let mut queries = self.shared.queries.subscribe();
        let found =
            |queries: &Vec<String>| queries.iter().filter(|query| query.contains(text)).count();
        within(queries.wait_for(|queries| found(queries) >= count))
            .await
            .expect("the fake keeps its queries");
    }

    /// Returns once `count` GraphQL documents have been posted.
    async fn queried(&self, count: usize) {
        let mut queries = self.shared.queries.subscribe();
        queries
            .wait_for(|queries| queries.len() >= count)
            .await
            .expect("the fake keeps its queries");
    }

    /// One `next` event on each open tool-call subscription, as a tool call's commit sends.
    fn changed(&self) {
        self.next_event("AgentToolCall");
    }

    /// One `next` event on each open mailbox subscription, as an item's commit sends.
    fn mailbox_changed(&self) {
        self.next_event("MailboxItem");
    }

    fn next_event(&self, collection: &str) {
        let event = format!(
            "event: next\ndata: {{\"data\":{{\"{collection}\":[{{\"_docID\":\"bae-1\"}}]}}}}\n\n"
        );
        let streams = self.shared.streams.lock().unwrap();
        for (_, stream) in streams
            .iter()
            .filter(|(query, _)| query.contains(collection))
        {
            stream.send(Ok(Bytes::from(event.clone()))).unwrap();
        }
    }

    /// Holds every query's answer until `hold(false)`, as a slow node would.
    fn hold(&self, held: bool) {
        self.shared.held.send_replace(held);
    }

    /// Closes every subscription, as a node whose event bus stops does.
    fn end_streams(&self) {
        self.shared.streams.lock().unwrap().clear();
    }

    pub(super) fn calls(&self) -> Vec<Call> {
        self.shared.calls.borrow().clone()
    }

    pub(super) fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|call| call.method).collect()
    }

    /// Returns once some connection has sent `method`.
    pub(super) async fn called(&self, method: &str) {
        let mut calls = self.shared.calls.subscribe();
        within(calls.wait_for(|calls| calls.iter().any(|call| call.method == method)))
            .await
            .expect("the fake keeps its calls");
    }

    pub(super) fn push<T>(&self, agent: &str, message: &T)
    where
        T: Serialize,
    {
        let text = serde_json::to_string(message).unwrap();
        self.socket(agent).send(Message::Text(text.into())).unwrap();
    }

    pub(super) fn close(&self, agent: &str) {
        self.socket(agent).send(Message::Close(None)).unwrap();
    }

    /// Drops the agent's socket with no close frame, as a runtime that died would.
    pub(super) fn sever(&self, agent: &str) {
        self.shared.sockets.lock().unwrap().remove(agent);
    }

    fn socket(&self, agent: &str) -> mpsc::UnboundedSender<Message> {
        self.shared.sockets.lock().unwrap()[agent].clone()
    }
}

async fn serve_graphql(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let query = body["query"].as_str().unwrap_or_default().to_string();
    shared
        .queries
        .send_modify(|queries| queries.push(query.clone()));
    if headers
        .get(header::ACCEPT)
        .is_none_or(|accept| accept != "text/event-stream")
    {
        let mut held = shared.held.subscribe();
        let _ = held.wait_for(|held| !held).await;
        return Json((shared.graphql)(&query)).into_response();
    }
    let (chunks, sent) = mpsc::unbounded_channel();
    shared.streams.lock().unwrap().push((query, chunks));
    let events = futures_util::stream::unfold(sent, |mut sent| async move {
        sent.recv().await.map(|chunk| (chunk, sent))
    });
    let body = Body::from_stream(events);
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

async fn runtime(State(shared): State<Arc<Shared>>) -> Response {
    match &shared.identity {
        Some(identity) => Json(identity.clone()).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn root(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| serve(socket, "/".to_string(), headers, shared))
}

async fn routed(
    State(shared): State<Arc<Shared>>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| serve(socket, agent, headers, shared))
}

async fn serve(mut socket: WebSocket, agent: String, headers: HeaderMap, shared: Arc<Shared>) {
    let (outgoing, mut queued) = mpsc::unbounded_channel();
    shared
        .sockets
        .lock()
        .unwrap()
        .insert(agent.clone(), outgoing);
    loop {
        tokio::select! {
            message = queued.recv() => {
                let Some(message) = message else { return };
                let closing = matches!(message, Message::Close(_));
                if socket.send(message).await.is_err() || closing {
                    return;
                }
            }
            received = socket.recv() => {
                let Some(Ok(Message::Text(text))) = received else { return };
                let message: Value = serde_json::from_str(&text).unwrap();
                let method = message["method"].as_str().unwrap().to_string();
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let call = Call {
                    agent: agent.clone(),
                    id: message.get("id").cloned(),
                    method: method.clone(),
                    params: params.clone(),
                    headers: headers.clone(),
                };
                shared.calls.send_modify(|calls| calls.push(call));
                let Some(id) = message.get("id") else { continue };
                let answer = if method == "initialize" {
                    Some(Ok(json!({})))
                } else {
                    (shared.script)(&agent, &method, &params)
                };
                let reply = match answer {
                    Some(Ok(result)) => json!({ "id": id, "result": result }),
                    Some(Err((code, message))) => {
                        json!({ "id": id, "error": { "code": code, "message": message } })
                    }
                    None => continue,
                };
                if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

pub(super) async fn within<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::time::timeout(WAIT, future)
        .await
        .expect("the shell should have answered by now")
}

pub(super) fn agents_a_and_b() -> Option<RuntimeIdentity> {
    Some(identity(Some(&["a", "b"])))
}

pub(super) async fn connect(
    fake: &FakeRuntime,
    agent: &str,
    events: mpsc::UnboundedSender<(String, ClientEvent)>,
) -> Client {
    connect_answering(fake, agent, events, ANSWER_WAIT).await
}

async fn connect_answering(
    fake: &FakeRuntime,
    agent: &str,
    events: mpsc::UnboundedSender<(String, ClientEvent)>,
    answer_wait: Duration,
) -> Client {
    within(Client::connect(
        &fake.route(agent),
        agent.to_string(),
        events,
        &test_runtime_handle(),
        answer_wait,
    ))
    .await
    .expect("the fake accepts the handshake")
}

/// Short enough to keep a timeout test quick, long enough for the fake's real answers.
const SHORT_WAIT: Duration = Duration::from_secs(1);

#[tokio::test]
async fn connect_sends_initialize_then_initialized() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, _, _| None).await;
    let (events, _events) = mpsc::unbounded_channel();

    let _client = connect(&fake, "a", events).await;

    fake.called("initialized").await;
    assert_eq!(fake.methods(), ["initialize", "initialized"]);
    let initialize = &fake.calls()[0];
    assert_eq!(initialize.agent, "a");
    assert_eq!(initialize.params["clientInfo"]["name"], "gents-shell");
}

#[tokio::test]
async fn a_request_returns_its_typed_result() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/list" => Some(Ok(json!({ "data": [], "nextCursor": "page-2" }))),
        _ => Some(Ok(json!({ "unexpected": true }))),
    })
    .await;
    let (events, _events) = mpsc::unbounded_channel();
    let client = connect(&fake, "a", events).await;

    let listed: codex::ThreadListResponse =
        within(client.request("thread/list", &json!({ "limit": 1 })))
            .await
            .unwrap();

    assert_eq!(listed.next_cursor.as_deref(), Some("page-2"));
    assert_eq!(fake.calls().last().unwrap().params, json!({ "limit": 1 }));
    let misshapen =
        within(client.request::<_, codex::ThreadListResponse>("thread/read", &json!({})))
            .await
            .expect_err("the answer does not fit the type");
    assert!(
        matches!(
            misshapen,
            ClientError::Decode {
                method: "thread/read",
                ..
            }
        ),
        "{misshapen:?}"
    );
}

#[tokio::test]
async fn an_error_reply_keeps_method_code_and_message() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, _, _| {
        Some(Err((-32602, "unknown Codex thread `t9`".to_string())))
    })
    .await;
    let (events, _events) = mpsc::unbounded_channel();
    let client = connect(&fake, "a", events).await;

    let refused = within(client.request::<_, Value>("thread/resume", &json!({ "threadId": "t9" })))
        .await
        .expect_err("the fake refuses");

    let ClientError::Rpc {
        method,
        code,
        message,
    } = &refused
    else {
        panic!("{refused:?}");
    };
    assert_eq!((*method, *code), ("thread/resume", -32602));
    assert_eq!(message, "unknown Codex thread `t9`");
    assert_eq!(
        refused.to_string(),
        "thread/resume: unknown Codex thread `t9`"
    );
}

#[tokio::test]
async fn notifications_arrive_decoded_and_tagged_with_the_agent() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, _, _| None).await;
    let (events, mut received) = mpsc::unbounded_channel();
    let _client = connect(&fake, "b", events).await;

    fake.push(
        "b",
        &json!({ "id": 7, "method": "item/tool/requestUserInput", "params": {} }),
    );
    fake.push("b", &json!({ "method": "no/suchMethod", "params": {} }));
    fake.push(
        "b",
        &json!({
            "method": "thread/status/changed",
            "params": { "threadId": "t1", "status": { "type": "idle" } },
        }),
    );

    let (agent, event) = within(received.recv()).await.unwrap();
    assert_eq!(agent, "b");
    let ClientEvent::Notification(notification) = event else {
        panic!("{event:?}");
    };
    let codex::ServerNotification::ThreadStatusChanged(changed) = *notification else {
        panic!("the server request and the unknown method are dropped: {notification:?}");
    };
    assert_eq!(changed.thread_id, "t1");
    assert_eq!(changed.status, codex::ThreadStatus::Idle);
}

#[tokio::test]
async fn a_closed_socket_fails_pending_requests_and_reports_closed() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, _, _| None).await;
    let (events, mut received) = mpsc::unbounded_channel();
    let client = connect(&fake, "a", events).await;
    let params = json!({});

    let (answer, ()) = within(async {
        tokio::join!(client.request::<_, Value>("thread/list", &params), async {
            fake.called("thread/list").await;
            fake.close("a");
        },)
    })
    .await;

    assert!(matches!(answer, Err(ClientError::Closed)), "{answer:?}");
    let (agent, event) = within(received.recv()).await.unwrap();
    assert_eq!(agent, "a");
    assert!(matches!(event, ClientEvent::Closed(None)), "{event:?}");
    let after = within(client.request::<_, Value>("thread/list", &params)).await;
    assert!(matches!(after, Err(ClientError::Closed)), "{after:?}");
}

#[tokio::test]
async fn the_handshake_carries_no_origin() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, _, _| None).await;
    let (events, _events) = mpsc::unbounded_channel();

    let _client = connect(&fake, "a", events).await;

    let headers = &fake.calls()[0].headers;
    assert!(headers.contains_key("upgrade"), "{headers:?}");
    assert!(!headers.contains_key("origin"), "{headers:?}");
}

pub(super) fn thread(id: &str, updated_at: i64) -> Value {
    json!({
        "id": id,
        "sessionId": id,
        "preview": "",
        "ephemeral": false,
        "modelProvider": "gents",
        "createdAt": updated_at,
        "updatedAt": updated_at,
        "status": { "type": "idle" },
        "cwd": "/srv/root",
        "cliVersion": "test",
        "source": "appServer",
        "turns": [],
    })
}

pub(super) fn turn(id: &str, status: &str, items: Value) -> Value {
    json!({ "id": id, "items": items, "status": status, "error": null })
}

/// What `thread/start` and `thread/resume` answer.
pub(super) fn thread_answer(thread: Value) -> Value {
    json!({
        "thread": thread,
        "model": "m",
        "modelProvider": "gents",
        "serviceTier": null,
        "cwd": "/srv/root",
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "sandbox": { "type": "dangerFullAccess" },
        "reasoningEffort": null,
    })
}

fn page(threads: &[Value], next: Option<&str>) -> Answer {
    Some(Ok(json!({ "data": threads, "nextCursor": next })))
}

fn turn_answer(id: &str) -> Answer {
    Some(Ok(json!({ "turn": turn(id, "inProgress", json!([])) })))
}

/// Lists nothing and leaves everything else unanswered.
pub(super) fn quiet(method: &str) -> Answer {
    match method {
        "thread/list" => page(&[], None),
        _ => None,
    }
}

pub(super) fn user_message(id: &str, text: &str) -> Value {
    json!({ "type": "userMessage", "id": id, "content": [{ "type": "text", "text": text }] })
}

fn agent_message(id: &str, text: &str) -> Value {
    json!({ "type": "agentMessage", "id": id, "text": text })
}

fn item_note(method: &str, session: &str, item: Value) -> Value {
    json!({
        "method": method,
        "params": { "item": item, "threadId": session, "turnId": "t1", "startedAtMs": 0, "completedAtMs": 0 },
    })
}

pub(super) fn line(text: &str, tone: Tone, item: &str) -> ChatLine {
    ChatLine {
        text: text.to_string(),
        tone,
        item: Some(item.to_string()),
        change: None,
    }
}

fn blank() -> ChatLine {
    ChatLine {
        text: String::new(),
        tone: Tone::Plain,
        item: None,
        change: None,
    }
}

/// The source on the test runtime, fed and read as the shell would.
struct Shell {
    intents: mpsc::UnboundedSender<Intent>,
    updates: Arc<Mutex<std::sync::mpsc::Receiver<Input>>>,
}

fn open_shell(fake: &FakeRuntime) -> Shell {
    open_shell_answering(fake, ANSWER_WAIT)
}

fn open_shell_answering(fake: &FakeRuntime, answer_wait: Duration) -> Shell {
    open_shell_with(fake, None, None, answer_wait)
}

/// As `gents --agent AGENT PROMPT` opens it.
fn open_shell_with(
    fake: &FakeRuntime,
    agent: Option<&str>,
    prompt: Option<&str>,
    answer_wait: Duration,
) -> Shell {
    // `work_for` refuses an anonymous endpoint; the fake checks no bearer.
    let endpoint = GraphqlEndpoint::with_delegated_authorization(fake.graphql(), "Bearer shell")
        .expect("a bearer");
    let home = std::path::PathBuf::new();
    let work = Work {
        endpoint,
        home: home.clone(),
    };
    open_shell_on(fake, agent, prompt, answer_wait, Ok(work), home)
}

fn open_shell_on(
    fake: &FakeRuntime,
    agent: Option<&str>,
    prompt: Option<&str>,
    answer_wait: Duration,
    work: Result<Work, String>,
    home: std::path::PathBuf,
) -> Shell {
    open_shell_in(fake, agent, prompt, answer_wait, work, home, None)
}

fn open_shell_in(
    fake: &FakeRuntime,
    agent: Option<&str>,
    prompt: Option<&str>,
    answer_wait: Duration,
    work: Result<Work, String>,
    home: std::path::PathBuf,
    cwd: Option<std::path::PathBuf>,
) -> Shell {
    let runtime = test_runtime_handle();
    let (intents, intended) = mpsc::unbounded_channel();
    let (sender, updates) = std::sync::mpsc::channel();
    let opening = Opening {
        remote: fake.remote(),
        agent: agent.map(str::to_string),
        prompt: prompt.map(str::to_string),
        work,
        home,
        cwd,
    };
    runtime.spawn(source::run(
        opening,
        intended,
        sender,
        runtime.clone(),
        answer_wait,
    ));
    Shell {
        intents,
        updates: Arc::new(Mutex::new(updates)),
    }
}

impl Shell {
    async fn next(&self) -> Update {
        let updates = self.updates.clone();
        let input = test_runtime_handle()
            .spawn_blocking(move || updates.lock().unwrap().recv_timeout(WAIT))
            .await
            .unwrap()
            .expect("the source should have sent an update by now");
        match input {
            Input::Runtime(update) => update,
            other => panic!("{other:?}"),
        }
    }

    /// Reads `Agents`, `Ready`, the first session list, then the first jobs and background
    /// calls.
    async fn ready(&self) -> Vec<Session> {
        let sessions = self.opened().await;
        let update = self.next().await;
        assert!(matches!(update, Update::Jobs { .. }), "{update:?}");
        let update = self.next().await;
        assert!(matches!(update, Update::Background { .. }), "{update:?}");
        sessions
    }

    /// Reads `Agents`, `Ready` and the first session list.
    async fn opened(&self) -> Vec<Session> {
        let update = self.next().await;
        let Update::Agents(agents) = update else {
            panic!("{update:?}");
        };
        let ids: Vec<_> = agents.iter().map(|agent| agent.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(self.next().await, Update::Connection(Connection::Ready));
        let update = self.next().await;
        let Update::Sessions(sessions) = update else {
            panic!("{update:?}");
        };
        sessions
    }

    /// Reads updates through the first that `wanted` matches, and returns them.
    async fn until<F>(&self, wanted: F) -> Vec<Update>
    where
        F: Fn(&Update) -> bool,
    {
        let mut read = Vec::new();
        loop {
            let update = self.next().await;
            let done = wanted(&update);
            read.push(update);
            if done {
                return read;
            }
        }
    }

    fn intend(&self, intent: Intent) {
        self.intents.send(intent).unwrap();
    }

    /// Applies each update to `app` through the first that matches `last`, and returns them.
    async fn apply_through<F>(&self, app: &mut App, last: F) -> Vec<Update>
    where
        F: Fn(&Update) -> bool,
    {
        let mut applied = Vec::new();
        loop {
            let update = self.next().await;
            app.apply(update.clone());
            let done = last(&update);
            applied.push(update);
            if done {
                return applied;
            }
        }
    }
}

/// The shell's UI on the source, and what it intends.
fn connected_app() -> (App, std::sync::mpsc::Receiver<Intent>) {
    let (intents, intended) = std::sync::mpsc::channel();
    (App::connected(KeySet::Vim, 120, 36, intents), intended)
}

fn is_begin(update: &Update) -> bool {
    matches!(update, Update::Begin { .. })
}

#[tokio::test]
async fn a_given_agent_becomes_the_first_tabs_agent() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell_with(&fake, Some("b"), None, ANSWER_WAIT);
    let (mut app, intended) = connected_app();

    shell.apply_through(&mut app, is_begin).await;

    assert_eq!(app.tabs[0].agent_id, "b");
    assert_eq!(app.notice, None);
    assert!(
        intended.try_recv().is_err(),
        "no prompt, so nothing is sent"
    );
}

#[tokio::test]
async fn an_unknown_agent_is_named_and_the_first_tab_keeps_the_default() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell_with(&fake, Some("zed"), None, ANSWER_WAIT);
    let (mut app, _intended) = connected_app();

    shell.apply_through(&mut app, is_begin).await;

    assert_eq!(app.tabs[0].agent_id, "a");
    assert_eq!(
        app.notice.as_deref(),
        Some("agent zed is not open in this shell; using a")
    );
}

/// `Begin` is told straight after `Agents`, before the first sweep, so a send the user makes
/// while the sweep runs comes after it in the shell's one queue of updates and keys.
#[tokio::test]
async fn begin_follows_agents_before_the_first_session_sweep() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell_with(&fake, None, Some("hello"), ANSWER_WAIT);
    let (mut app, _intended) = connected_app();

    let applied = shell
        .apply_through(&mut app, |update| matches!(update, Update::Sessions(_)))
        .await;

    assert!(
        matches!(
            applied.as_slice(),
            [
                Update::Agents(_),
                Update::Begin { .. },
                Update::Connection(Connection::Ready),
                Update::Sessions(_)
            ]
        ),
        "{applied:?}"
    );
}

#[tokio::test]
async fn a_given_prompt_is_sent_once_as_the_first_tabs_first_message() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/start" => Some(Ok(thread_answer(thread("s-new", 100)))),
        "turn/start" => turn_answer("t1"),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell_with(&fake, Some("b"), Some("hello"), ANSWER_WAIT);
    let (mut app, intended) = connected_app();

    let applied = shell.apply_through(&mut app, is_begin).await;
    assert!(matches!(applied[0], Update::Agents(_)), "{applied:?}");
    let sent = intended.try_recv().expect("the prompt goes out");
    assert!(intended.try_recv().is_err(), "the prompt goes out once");
    assert!(
        matches!(&sent, Intent::Send { tab: 0, session: None, agent, text }
            if agent == "b" && text == "hello"),
        "{sent:?}"
    );
    shell.intend(sent);
    shell
        .apply_through(&mut app, |update| matches!(update, Update::Turn { .. }))
        .await;
    fake.push(
        "b",
        &item_note("item/started", "s-new", user_message("u1", "hello")),
    );
    shell
        .apply_through(&mut app, |update| matches!(update, Update::Item { .. }))
        .await;

    let first = &app.tabs[0];
    assert_eq!(first.session.as_deref(), Some("s-new"));
    assert_eq!(first.lines, [line("hello", Tone::You, "u1"), blank()]);
    let sent: Vec<_> = fake
        .calls()
        .into_iter()
        .filter(|call| call.agent == "b" && call.method != "thread/list")
        .map(|call| call.method)
        .collect();
    assert_eq!(
        sent,
        ["initialize", "initialized", "thread/start", "turn/start"]
    );
}

#[tokio::test]
async fn with_no_agent_or_prompt_the_shell_opens_as_before() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);
    let (mut app, intended) = connected_app();

    let applied = shell
        .apply_through(&mut app, |update| {
            matches!(update, Update::Background { .. })
        })
        .await;
    fake.push(
        "a",
        &item_note("item/completed", "s1", agent_message("r1", "hi")),
    );

    assert_eq!(applied.len(), 5, "{applied:?}");
    let item = Update::Item {
        session: "s1".to_string(),
        line: line("hi", Tone::Reply, "r1"),
    };
    assert_eq!(
        shell.next().await,
        item,
        "nothing else was queued before it"
    );
    assert_eq!(app.tabs[0].agent_id, "a");
    assert_eq!(app.notice, None);
    assert!(intended.try_recv().is_err());
}

#[tokio::test]
async fn sessions_from_every_agent_merge_newest_first() {
    let fake = FakeRuntime::start(agents_a_and_b(), |agent, method, params| {
        match (agent, method, params["cursor"].as_str()) {
            ("a", "thread/list", None) => {
                page(&[thread("a1", 300), thread("a2", 100)], Some("a-2"))
            }
            ("a", "thread/list", Some("a-2")) => page(&[thread("a3", 200)], None),
            ("b", "thread/list", _) => page(&[thread("b1", 250)], None),
            _ => None,
        }
    })
    .await;
    let shell = open_shell(&fake);

    let sessions = shell.ready().await;

    let listed: Vec<_> = sessions
        .iter()
        .map(|session| (session.id.as_str(), session.agent.as_str()))
        .collect();
    assert_eq!(listed, [("a1", "a"), ("b1", "b"), ("a3", "a"), ("a2", "a")]);
    let lists: Vec<_> = fake
        .calls()
        .into_iter()
        .filter(|call| call.method == "thread/list")
        .map(|call| (call.agent, call.params))
        .collect();
    assert_eq!(lists.len(), 3, "{lists:?}");
    for (_, params) in &lists {
        assert_eq!(params["sortKey"], "updated_at");
        assert_eq!(params["limit"], 100);
    }
    assert_eq!(
        (lists[1].0.as_str(), &lists[1].1["cursor"]),
        ("a", &json!("a-2"))
    );
    assert_eq!(lists[2].0, "b");
}

#[tokio::test]
async fn opening_a_session_sends_its_transcript_and_running_turn() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(Intent::Open {
        session: "s1".to_string(),
        agent: "b".to_string(),
    });
    fake.called("thread/resume").await;
    let resume = fake.calls().pop().unwrap();
    assert_eq!(resume.agent, "b");
    assert_eq!(resume.params["threadId"], "s1");
    assert!(
        resume.params.get("excludeTurns").is_none(),
        "{:?}",
        resume.params
    );
    // Sent before the answer, so it reaches the shell only after the transcript.
    fake.push(
        "b",
        &item_note("item/completed", "s1", agent_message("r2", "still going")),
    );
    let mut resumed = thread("s1", 100);
    resumed["turns"] = json!([
        turn(
            "t1",
            "completed",
            json!([user_message("u1", "hello"), agent_message("r1", "hi")])
        ),
        turn("t2", "inProgress", json!([user_message("u2", "and now?")])),
    ]);
    fake.push(
        "b",
        &json!({ "id": resume.id, "result": thread_answer(resumed) }),
    );

    let opened = Update::Opened {
        session: "s1".to_string(),
        lines: vec![
            line("hello", Tone::You, "u1"),
            blank(),
            line("hi", Tone::Reply, "r1"),
            blank(),
            line("and now?", Tone::You, "u2"),
            blank(),
        ],
    };
    let running = Update::Turn {
        session: "s1".to_string(),
        turn: "t2".to_string(),
    };
    assert_eq!(shell.next().await, running, "before the transcript");
    assert_eq!(shell.next().await, opened);
    assert_eq!(shell.next().await, rows("s1", Vec::new()));
    let item = Update::Item {
        session: "s1".to_string(),
        line: line("still going", Tone::Reply, "r2"),
    };
    let read = reread_and(&shell, is_item).await;
    let items: Vec<_> = read.iter().filter(|update| is_item(update)).collect();
    assert_eq!(items, [&item]);
}

const EDIT: &str = "@@ -1 +1 @@\n-a\n+b\n";

/// A `fileChange` item with one change.
fn file_change(id: &str, status: &str, path: &str, kind: &str, diff: &str) -> Value {
    json!({
        "type": "fileChange",
        "id": id,
        "status": status,
        "changes": [{ "path": path, "kind": { "type": kind }, "diff": diff }],
    })
}

/// Where each text sits among the conversation's rows.
fn row_of(app: &App, text: &str) -> Option<usize> {
    let rows = app.conversation_rows(120);
    rows.iter().position(|(row, _)| row.contains(text))
}

fn diff_view(app: &App) -> Vec<String> {
    let rows = app.view_rows(View::Diff);
    rows.iter().map(|row| row.text.to_string()).collect()
}

fn is_s1_rows(update: &Update) -> bool {
    matches!(update, Update::Running { session, .. } if session == "s1")
}

#[tokio::test]
async fn an_opened_sessions_changes_carry_their_turns() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/resume" => {
            let mut resumed = thread("s1", 100);
            resumed["turns"] = json!([
                turn(
                    "t1",
                    "completed",
                    json!([
                        user_message("u1", "edit it"),
                        file_change("fc1", "completed", "a.rs", "update", EDIT),
                    ])
                ),
                turn(
                    "t2",
                    "completed",
                    json!([file_change("fc2", "completed", "b.rs", "add", "x\n")])
                ),
            ]);
            Some(Ok(thread_answer(resumed)))
        }
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;

    open_s1(&shell);
    let applied = shell.apply_through(&mut app, is_s1_rows).await;

    let opened = applied.iter().find_map(|update| match update {
        Update::Opened { lines, .. } => Some(lines),
        _ => None,
    });
    let turns: Vec<_> = opened
        .expect("the transcript")
        .iter()
        .filter_map(|line| line.change.as_ref())
        .map(|change| change.turn.as_str())
        .collect();
    assert_eq!(turns, ["t1", "t2"]);
    let rows = app.conversation_rows(120);
    let header = |text: &str| rows.iter().position(|(row, _)| row == text);
    let first = header("  1 file changed  +1 -1").expect("t1's card");
    let second = header("  1 file changed  +1").expect("t2's card");
    assert!(first < second, "{rows:#?}");
    let view = diff_view(&app);
    let added = view.iter().position(|row| row.contains("A b.rs"));
    let edited = view.iter().position(|row| row.contains("M a.rs"));
    assert!(added.expect("b.rs") < edited.expect("a.rs"), "{view:?}");
}

#[tokio::test]
async fn a_streamed_file_change_shows_its_card_when_the_turn_completes() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "turn/start" => turn_answer("t1"),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;

    shell.intend(Intent::Send {
        tab: app.tabs[0].id,
        session: Some("s1".to_string()),
        agent: "a".to_string(),
        text: "edit it".to_string(),
    });
    let started = |update: &Update| matches!(update, Update::Turn { .. });
    shell.apply_through(&mut app, started).await;
    assert_eq!(app.tabs[0].turn.as_deref(), Some("t1"));
    for (method, status) in [
        ("item/started", "inProgress"),
        ("item/completed", "completed"),
    ] {
        let edit = file_change("fc", status, "a.rs", "update", EDIT);
        fake.push("a", &item_note(method, "s1", edit));
        let item = |update: &Update| matches!(update, Update::Item { .. });
        shell.apply_through(&mut app, item).await;
    }
    assert!(row_of(&app, "✓ edit  a.rs").is_some());
    assert_eq!(row_of(&app, "1 file changed"), None);

    let completed = json!({ "threadId": "s1", "turn": turn("t1", "completed", json!([])) });
    fake.push(
        "a",
        &json!({ "method": "turn/completed", "params": completed }),
    );
    let ended = |update: &Update| matches!(update, Update::TurnEnded { .. });
    shell.apply_through(&mut app, ended).await;
    assert!(row_of(&app, "1 file changed  +1 -1").is_some());
    app.show_file(0);
    assert!(app.diff_rows(0).contains(&(" +b".to_string(), Tone::Add)));
}

/// A running turn has no card, from the first update the tab applies.
#[tokio::test]
async fn opening_a_session_with_a_running_turn_never_shows_its_card() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/resume" => {
            let mut resumed = thread("s1", 100);
            resumed["turns"] = json!([
                turn(
                    "t1",
                    "completed",
                    json!([
                        user_message("u1", "edit it"),
                        file_change("fc1", "completed", "a.rs", "update", EDIT),
                    ])
                ),
                turn(
                    "t2",
                    "inProgress",
                    json!([
                        user_message("u2", "and add one"),
                        file_change("fc2", "completed", "b.rs", "add", "x\n"),
                    ])
                ),
            ]);
            Some(Ok(thread_answer(resumed)))
        }
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;

    open_s1(&shell);
    let card = |app: &App, text: &str| {
        let rows = app.conversation_rows(120);
        rows.iter().any(|(row, _)| row == text)
    };
    loop {
        let update = shell.next().await;
        app.apply(update.clone());
        assert!(!card(&app, "  1 file changed  +1"), "after {update:?}");
        if is_s1_rows(&update) {
            break;
        }
    }
    assert_eq!(app.tabs[0].turn.as_deref(), Some("t2"));
    assert!(
        card(&app, "  1 file changed  +1 -1"),
        "the ended turn's card"
    );
}

/// Opening a tab again replaces the changes it shows with the reread transcript's.
#[tokio::test]
async fn reopening_a_session_shows_only_the_reread_changes() {
    let reread = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let again = reread.clone();
    let fake = FakeRuntime::start(agents_a_and_b(), move |_, method, _| match method {
        "thread/resume" => {
            let path = match again.load(std::sync::atomic::Ordering::SeqCst) {
                false => "a.rs",
                true => "z.rs",
            };
            let mut resumed = thread("s1", 100);
            let edit = file_change("fc", "completed", path, "update", EDIT);
            resumed["turns"] = json!([turn("t1", "completed", json!([edit]))]);
            Some(Ok(thread_answer(resumed)))
        }
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;
    open_s1(&shell);
    shell.apply_through(&mut app, is_s1_rows).await;
    // Then the reread opening asks for.
    shell.apply_through(&mut app, is_s1_rows).await;
    assert!(row_of(&app, "M a.rs").is_some());

    reread.store(true, std::sync::atomic::Ordering::SeqCst);
    open_s1(&shell);
    shell.apply_through(&mut app, is_s1_rows).await;

    assert!(row_of(&app, "M z.rs").is_some());
    assert_eq!(row_of(&app, "a.rs"), None);
    let view = diff_view(&app);
    assert!(view.iter().any(|row| row.contains("M z.rs")), "{view:?}");
    assert!(!view.iter().any(|row| row.contains("a.rs")), "{view:?}");
}

/// A subagent's thread as the endpoint lists it: under its root `s1`, named after its job, on
/// the agent `local:researcher`.
pub(super) fn child(id: &str, parent: &str, status: Value) -> Value {
    let mut thread = thread(id, 100);
    thread["sessionId"] = json!("s1");
    thread["name"] = json!(format!("{id} job"));
    thread["status"] = status;
    thread["source"] = json!({
        "subAgent": {
            "thread_spawn": {
                "parent_thread_id": parent,
                "depth": 1,
                "agent_nickname": "local:researcher",
                "agent_role": "local:researcher",
            }
        }
    });
    thread["threadSource"] = json!("subagent");
    thread["agentNickname"] = json!("local:researcher");
    thread["agentRole"] = json!("local:researcher");
    thread
}

fn active() -> Value {
    json!({ "type": "active", "activeFlags": [] })
}

fn idle() -> Value {
    json!({ "type": "idle" })
}

fn is_child_list(method: &str, params: &Value) -> bool {
    method == "thread/list" && !params["sourceKinds"].is_null()
}

/// `thread/resume` answers each thread as `threads` has it, and `s1` as a plain session.
fn resumed(params: &Value, threads: &[Value]) -> Answer {
    let id = params["threadId"].as_str().unwrap();
    let found = threads.iter().find(|thread| thread["id"] == id).cloned();
    Some(Ok(thread_answer(found.unwrap_or_else(|| thread(id, 100)))))
}

fn rows(session: &str, rows: Vec<Running>) -> Update {
    Update::Running {
        session: session.to_string(),
        rows,
    }
}

fn row(id: &str, status: &str, state: RunState) -> Running {
    Running {
        name: "researcher".to_string(),
        job: format!("{id} job"),
        status: status.to_string(),
        state,
        opens: Opens::Agent(id.to_string()),
    }
}

fn open_s1(shell: &Shell) {
    shell.intend(Intent::Open {
        session: "s1".to_string(),
        agent: "a".to_string(),
    });
}

pub(super) fn resumes_of(fake: &FakeRuntime, thread: &str) -> usize {
    let calls = fake.calls().into_iter();
    calls
        .filter(|call| call.method == "thread/resume" && call.params["threadId"] == thread)
        .count()
}

#[tokio::test]
async fn opening_a_session_lists_its_subagents_as_running_rows() {
    let fake = FakeRuntime::start(agents_a_and_b(), |agent, method, params| {
        let error = json!({ "type": "systemError" });
        let children = [
            child("c1", "s1", idle()),
            child("c2", "s2", idle()),
            child("c3", "s1", error),
        ];
        match method {
            "thread/resume" => resumed(params, &children),
            _ if agent == "a" && is_child_list(method, params) => page(&children, None),
            _ => quiet(method),
        }
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    open_s1(&shell);

    let opened = shell.next().await;
    assert!(
        matches!(&opened, Update::Opened { session, .. } if session == "s1"),
        "{opened:?}"
    );
    let expected = vec![
        row("c1", "finished", RunState::Done),
        row("c3", "error", RunState::Failed),
    ];
    assert_eq!(shell.next().await, rows("s1", expected));
    let lists: Vec<_> = fake
        .calls()
        .into_iter()
        .filter(|call| is_child_list(&call.method, &call.params))
        .map(|call| (call.agent, call.params["sourceKinds"].clone()))
        .collect();
    let kinds = json!(["subAgent", "subAgentThreadSpawn"]);
    assert_eq!(lists, [("a".to_string(), kinds)]);
    assert_eq!(resumes_of(&fake, "c1") + resumes_of(&fake, "c3"), 0);
}

#[tokio::test]
async fn a_working_subagent_is_resumed_once_and_its_end_refreshes_the_rows() {
    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ends = ended.clone();
    let fake = FakeRuntime::start(agents_a_and_b(), move |agent, method, params| {
        let status = if ends.load(std::sync::atomic::Ordering::SeqCst) {
            idle()
        } else {
            active()
        };
        let children = [child("c1", "s1", status)];
        match method {
            "thread/resume" => resumed(params, &children),
            _ if agent == "a" && is_child_list(method, params) => page(&children, None),
            _ => quiet(method),
        }
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    open_s1(&shell);
    let opened = shell.next().await;
    assert!(matches!(opened, Update::Opened { .. }), "{opened:?}");
    let working = vec![row("c1", "working", RunState::Working)];
    assert_eq!(shell.next().await, rows("s1", working));
    let resume = fake.calls().pop().unwrap();
    assert_eq!(
        (resume.agent.as_str(), resume.method.as_str()),
        ("a", "thread/resume")
    );
    assert_eq!(resume.params["threadId"], "c1");
    reread(&shell, 1).await;

    ended.store(true, std::sync::atomic::Ordering::SeqCst);
    let completed = json!({ "threadId": "c1", "turn": turn("t1", "completed", json!([])) });
    fake.push(
        "a",
        &json!({ "method": "turn/completed", "params": completed }),
    );

    let done = Update::TurnEnded {
        session: "c1".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(shell.next().await, done, "no Opened for the resumed child");
    let relisted = shell.next().await;
    assert!(matches!(relisted, Update::Sessions(_)), "{relisted:?}");
    let finished = vec![row("c1", "finished", RunState::Done)];
    assert_eq!(shell.next().await, rows("s1", finished));
    assert_eq!(resumes_of(&fake, "c1"), 1);
}

#[tokio::test]
async fn a_subagent_that_ended_before_its_resume_reads_finished() {
    let fake = FakeRuntime::start(agents_a_and_b(), |agent, method, params| match method {
        "thread/resume" => resumed(params, &[child("c1", "s1", idle())]),
        _ if agent == "a" && is_child_list(method, params) => {
            page(&[child("c1", "s1", active())], None)
        }
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    open_s1(&shell);

    shell.next().await;
    let finished = vec![row("c1", "finished", RunState::Done)];
    assert_eq!(shell.next().await, rows("s1", finished));
}

#[tokio::test]
async fn a_spawn_on_the_open_session_relists_its_subagents() {
    let lists = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listed = lists.clone();
    let fake = FakeRuntime::start(
        agents_a_and_b(),
        move |agent, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ if agent == "a" && is_child_list(method, params) => {
                match listed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => page(&[], None),
                    _ => page(&[child("c1", "s1", idle())], None),
                }
            }
            _ => quiet(method),
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    open_s1(&shell);
    shell.next().await;
    assert_eq!(shell.next().await, rows("s1", Vec::new()));
    reread(&shell, 1).await;

    let spawn = json!({
        "type": "collabAgentToolCall",
        "id": "k1",
        "tool": "spawnAgent",
        "status": "inProgress",
        "senderThreadId": "s1",
        "receiverThreadIds": [],
        "prompt": "look into it",
        "agentsStates": {},
    });
    fake.push("a", &item_note("item/started", "s1", spawn));

    let item = shell.next().await;
    assert!(matches!(item, Update::Item { .. }), "{item:?}");
    let spawned = vec![row("c1", "finished", RunState::Done)];
    assert_eq!(shell.next().await, rows("s1", spawned));
}

#[tokio::test]
async fn stepping_into_a_subagent_resumes_it_and_streams_it() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, params| {
        let mut stepped = child("c1", "s1", active());
        stepped["turns"] = json!([turn(
            "t1",
            "inProgress",
            json!([user_message("u1", "look into it")])
        )]);
        match method {
            "thread/resume" => resumed(params, &[stepped]),
            _ => quiet(method),
        }
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(Intent::Open {
        session: "c1".to_string(),
        agent: "a".to_string(),
    });

    let opened = Update::Opened {
        session: "c1".to_string(),
        lines: vec![line("look into it", Tone::You, "u1"), blank()],
    };
    let turn = Update::Turn {
        session: "c1".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(shell.next().await, turn);
    assert_eq!(shell.next().await, opened);
    assert_eq!(shell.next().await, rows("c1", Vec::new()));
    reread(&shell, 1).await;
    let params = json!({ "threadId": "c1", "turnId": "t1", "itemId": "r1", "delta": "found" });
    fake.push(
        "a",
        &json!({ "method": "item/agentMessage/delta", "params": params }),
    );
    let delta = Update::Delta {
        session: "c1".to_string(),
        item: "r1".to_string(),
        text: "found".to_string(),
        tone: Tone::Reply,
    };
    assert_eq!(shell.next().await, delta);
    assert_eq!(resumes_of(&fake, "c1"), 1);
}

#[tokio::test]
async fn a_first_send_starts_a_thread_then_a_turn() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "turn/start" => turn_answer("t1"),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(Intent::Send {
        tab: 7,
        session: None,
        agent: "a".to_string(),
        text: "hello".to_string(),
    });
    fake.called("thread/start").await;
    let start = fake.calls().pop().unwrap();
    // The runtime may report on the new thread before it answers; that waits for `Started`.
    fake.push(
        "a",
        &item_note("item/started", "s-new", user_message("u1", "hello")),
    );
    fake.push(
        "a",
        &json!({ "id": start.id, "result": thread_answer(thread("s-new", 100)) }),
    );

    let started = Update::Started {
        tab: 7,
        session: "s-new".to_string(),
    };
    assert_eq!(shell.next().await, started);
    let running = Update::Turn {
        session: "s-new".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(shell.next().await, running);
    let item = Update::Item {
        session: "s-new".to_string(),
        line: line("hello", Tone::You, "u1"),
    };
    let read = reread_and(&shell, is_item).await;
    let items: Vec<_> = read.iter().filter(|update| is_item(update)).collect();
    assert_eq!(items, [&item]);
    let sent: Vec<_> = fake
        .calls()
        .into_iter()
        .filter(|call| call.agent == "a" && call.method != "thread/list")
        .collect();
    let methods: Vec<_> = sent.iter().map(|call| call.method.as_str()).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "thread/start", "turn/start"]
    );
    let turn_start = &sent[3].params;
    assert_eq!(turn_start["threadId"], "s-new");
    assert_eq!(
        turn_start["input"],
        json!([{ "type": "text", "text": "hello", "text_elements": [] }])
    );
}

#[tokio::test]
async fn a_reply_streams_as_deltas() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "turn/start" => turn_answer("t1"),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(Intent::Send {
        tab: 0,
        session: Some("s1".to_string()),
        agent: "a".to_string(),
        text: "hi".to_string(),
    });
    let running = Update::Turn {
        session: "s1".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(shell.next().await, running);
    reread(&shell, 1).await;
    for delta in ["Hel", "lo"] {
        let params = json!({ "threadId": "s1", "turnId": "t1", "itemId": "r1", "delta": delta });
        fake.push(
            "a",
            &json!({ "method": "item/agentMessage/delta", "params": params }),
        );
    }
    for text in ["Hel", "lo"] {
        let delta = Update::Delta {
            session: "s1".to_string(),
            item: "r1".to_string(),
            text: text.to_string(),
            tone: Tone::Reply,
        };
        assert_eq!(shell.next().await, delta);
    }

    let completed = json!({ "threadId": "s1", "turn": turn("t1", "completed", json!([])) });
    fake.push(
        "a",
        &json!({ "method": "turn/completed", "params": completed }),
    );
    let done = Update::TurnEnded {
        session: "s1".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(shell.next().await, done);
    let relisted = shell.next().await;
    assert!(matches!(relisted, Update::Sessions(_)), "{relisted:?}");
}

/// What `thread/read` answers for `s1` with these turns.
fn read_answer(turns: Value) -> Answer {
    let mut read = thread("s1", 100);
    read["turns"] = turns;
    Some(Ok(json!({ "thread": read })))
}

fn interrupt_t1(shell: &Shell) {
    shell.intend(Intent::Interrupt {
        session: "s1".to_string(),
        agent: "b".to_string(),
        turn: "t1".to_string(),
    });
}

fn t1_ended() -> Update {
    Update::TurnEnded {
        session: "s1".to_string(),
        turn: "t1".to_string(),
    }
}

#[tokio::test]
async fn interrupt_sends_turn_interrupt_for_a_running_turn() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/read" => read_answer(json!([turn("t1", "inProgress", json!([]))])),
        "turn/interrupt" => Some(Ok(json!({}))),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    interrupt_t1(&shell);

    assert_eq!(shell.next().await, t1_ended());
    let calls: Vec<_> = fake
        .calls()
        .into_iter()
        .filter(|call| call.agent == "b")
        .filter(|call| {
            !["initialize", "initialized", "thread/list"].contains(&call.method.as_str())
        })
        .map(|call| (call.method, call.params))
        .collect();
    let expected = [
        (
            "thread/read".to_string(),
            json!({ "threadId": "s1", "includeTurns": true }),
        ),
        (
            "turn/interrupt".to_string(),
            json!({ "threadId": "s1", "turnId": "t1" }),
        ),
    ];
    assert_eq!(calls, expected);
}

/// The endpoint interrupts whichever turn runs, so stopping an ended one would stop
/// another client's newer turn.
#[tokio::test]
async fn interrupt_clears_a_turn_that_has_ended_without_stopping_another() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/read" => read_answer(json!([
            turn("t1", "completed", json!([])),
            turn("t2", "inProgress", json!([])),
        ])),
        "turn/interrupt" => Some(Ok(json!({}))),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    interrupt_t1(&shell);

    assert_eq!(shell.next().await, t1_ended());
    let methods = fake.methods();
    assert!(methods.contains(&"thread/read".to_string()), "{methods:?}");
    assert!(
        !methods.contains(&"turn/interrupt".to_string()),
        "{methods:?}"
    );
}

#[tokio::test]
async fn a_refused_send_becomes_a_notice_in_the_runtimes_words() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/start" => Some(Err((-32602, "no model can run yet".to_string()))),
        "turn/start" => Some(Err((-32602, "the session is busy".to_string()))),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(Intent::Send {
        tab: 3,
        session: Some("s1".to_string()),
        agent: "a".to_string(),
        text: "hi".to_string(),
    });
    // A failure the tab is told of, so it stops showing the turn as running.
    let refused = Update::StartFailed {
        tab: 3,
        message: "turn/start: the session is busy".to_string(),
    };
    assert_eq!(shell.next().await, refused);
    reread(&shell, 1).await;

    shell.intend(Intent::Send {
        tab: 4,
        session: None,
        agent: "a".to_string(),
        text: "hi".to_string(),
    });
    let failed = Update::StartFailed {
        tab: 4,
        message: "thread/start: no model can run yet".to_string(),
    };
    assert_eq!(shell.next().await, failed);
    let turns = fake
        .methods()
        .into_iter()
        .filter(|method| method == "turn/start")
        .count();
    assert_eq!(turns, 1, "a failed start sends no turn");
}

/// What `skills/list` answers: `commit` and `review` enabled and `old` disabled, at the paths
/// the endpoint lists a skill at.
fn skills_answer() -> Answer {
    let skill = |id: &str, description: &str, enabled: bool| {
        let path = format!("/gents/skills/{id}");
        json!({ "name": id, "description": description, "path": path, "scope": "system", "enabled": enabled })
    };
    let skills = [
        skill("commit", "Writes commits", true),
        skill("review", "", true),
        skill("old", "Retired", false),
    ];
    Some(Ok(
        json!({ "data": [{ "cwd": "/srv/root", "skills": skills, "errors": [] }] }),
    ))
}

fn shown_skill(id: &str, about: &str) -> Skill {
    Skill {
        id: id.to_string(),
        about: about.to_string(),
    }
}

fn is_skills(update: &Update) -> bool {
    matches!(update, Update::Skills { .. })
}

/// `node_settings_of(did)` with agent `a` on context `main`, which lists `commit`, and agent `b`
/// on no context.
fn skill_settings(did: &str) -> Value {
    let mut config = node_settings_of(did);
    config["contexts"] = json!([
        { "node_did": did, "context_id": "main", "skill_ids": ["commit"] },
    ]);
    let commit = json!({ "node_did": did, "skill_id": "commit", "name": "commit" });
    config["skills"].as_array_mut().unwrap().push(commit);
    let agent = |id: &str| json!({ "node_did": did, "agent_id": id, "display_name": id, "inference_profile_id": "profile" });
    let mut a = agent("a");
    a["context_id"] = json!("main");
    config["agents"]
        .as_array_mut()
        .unwrap()
        .extend([a, agent("b")]);
    config
}

/// The shell on a fake runtime for agents `a` and `b`, whose work is a node served with
/// `skill_settings` and signed by an initialized home's key, as on a home `gents init` set up.
struct OnNode {
    shell: Shell,
    node: Arc<defra_node::EmbeddedNode>,
    work: Work,
    did: String,
    _dirs: [tempfile::TempDir; 2],
}

async fn on_node<F>(script: F) -> OnNode
where
    F: Fn(&str, &str, &Value) -> Answer + Send + Sync + 'static,
{
    let keys = tempfile::tempdir().unwrap();
    let key = keys.path().join("node.key");
    let signer = gents::store::identity::KeyIdentity::load_or_create(&key, None).unwrap();
    let did = gents::store::identity::NodeIdentity::did(&signer).to_string();
    let (node, graphql) = crate::shared::test_support::served_config(skill_settings(&did)).await;
    let (home, _) = home_serving(&graphql, Some(&key));
    let work = Work {
        endpoint: GraphqlEndpoint::anonymous(graphql),
        home: home.path().into(),
    };
    let mut runtime = identity(Some(&["a", "b"]));
    runtime.node_did = did.clone();
    let fake = FakeRuntime::start(Some(runtime), script).await;
    let shell = open_shell_on(
        &fake,
        None,
        None,
        ANSWER_WAIT,
        Ok(work.clone()),
        home.path().into(),
    );
    shell.opened().await;
    OnNode {
        shell,
        node,
        work,
        did,
        _dirs: [keys, home],
    }
}

fn read_skills(agent: &str) -> Intent {
    Intent::ReadSkills {
        agent: agent.to_string(),
    }
}

#[tokio::test]
async fn skills_list_only_what_the_agents_context_allows() {
    let on = on_node(|_, method, _| match method {
        "skills/list" => skills_answer(),
        _ => quiet(method),
    })
    .await;

    on.shell.intend(read_skills("a"));
    let told = on.shell.until(is_skills).await;

    let allowed = Update::Skills {
        agent: "a".to_string(),
        skills: vec![shown_skill("commit", "Writes commits")],
    };
    assert_eq!(told.last(), Some(&allowed));
    on.node.shutdown().await;
}

#[tokio::test]
async fn an_agent_with_no_context_offers_no_skills() {
    let on = on_node(|_, method, _| match method {
        "skills/list" => skills_answer(),
        _ => quiet(method),
    })
    .await;

    on.shell.intend(read_skills("b"));
    let told = on.shell.until(is_skills).await;

    let none = Update::Skills {
        agent: "b".to_string(),
        skills: Vec::new(),
    };
    assert_eq!(told.last(), Some(&none));
    on.node.shutdown().await;
}

/// With work off the context cannot be read, so every enabled skill is listed and admission
/// refuses one outside the context.
#[tokio::test]
async fn a_skill_turn_sends_the_listed_skill_input() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "skills/list" => skills_answer(),
        "turn/start" => turn_answer("t1"),
        _ => quiet(method),
    })
    .await;
    let off = "jobs and the mailbox are off: no runtime".to_string();
    let home = tempfile::tempdir().unwrap();
    let shell = open_shell_on(&fake, None, None, ANSWER_WAIT, Err(off), home.path().into());
    shell.opened().await;
    let send = |skill: &str, text: &str| Intent::SendSkill {
        tab: 7,
        session: Some("s1".to_string()),
        agent: "a".to_string(),
        skill: skill.to_string(),
        text: text.to_string(),
    };
    let is_turn = |update: &Update| matches!(update, Update::Turn { .. });

    shell.intend(read_skills("a"));
    let listed = shell.until(is_skills).await;
    shell.intend(send("commit", "fix it"));
    shell.until(is_turn).await;
    shell.intend(send("commit", ""));
    shell.until(is_turn).await;
    shell.intend(send("nope", "fix it"));
    let missed = shell
        .until(|update| matches!(update, Update::StartFailed { .. }))
        .await;

    let enabled = Update::Skills {
        agent: "a".to_string(),
        skills: vec![
            shown_skill("commit", "Writes commits"),
            shown_skill("review", "review"),
        ],
    };
    assert_eq!(listed.last(), Some(&enabled));
    let unknown = Update::StartFailed {
        tab: 7,
        message: "unknown skill /nope".to_string(),
    };
    assert_eq!(missed.last(), Some(&unknown));
    let inputs: Vec<_> = fake
        .calls()
        .into_iter()
        .filter(|call| call.method == "turn/start")
        .map(|call| call.params["input"].clone())
        .collect();
    let commit = json!({ "type": "skill", "name": "commit", "path": "/gents/skills/commit" });
    let fix = json!({ "type": "text", "text": "fix it", "text_elements": [] });
    assert_eq!(inputs, [json!([commit, fix]), json!([commit])]);
}

#[tokio::test]
async fn a_failed_skills_read_leaves_no_rows_to_show_or_send() {
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let read = reads.clone();
    let fake = FakeRuntime::start(agents_a_and_b(), move |_, method, _| match method {
        "skills/list" if read.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => {
            skills_answer()
        }
        "skills/list" => Some(Err((-32603, "the skills are unreadable".to_string()))),
        _ => quiet(method),
    })
    .await;
    let off = "jobs and the mailbox are off: no runtime".to_string();
    let home = tempfile::tempdir().unwrap();
    let shell = open_shell_on(&fake, None, None, ANSWER_WAIT, Err(off), home.path().into());
    shell.opened().await;

    shell.intend(read_skills("a"));
    shell.until(is_skills).await;
    shell.intend(read_skills("a"));
    let told = shell.until(is_skills).await;
    shell.intend(Intent::SendSkill {
        tab: 7,
        session: Some("s1".to_string()),
        agent: "a".to_string(),
        skill: "commit".to_string(),
        text: String::new(),
    });
    let missed = shell
        .until(|update| matches!(update, Update::StartFailed { .. }))
        .await;

    let notice = Update::Notice("skills/list: the skills are unreadable".to_string());
    assert!(told.contains(&notice), "{told:?}");
    let cleared = Update::Skills {
        agent: "a".to_string(),
        skills: Vec::new(),
    };
    assert_eq!(told.last(), Some(&cleared));
    let unknown = Update::StartFailed {
        tab: 7,
        message: "unknown skill /commit".to_string(),
    };
    assert_eq!(missed.last(), Some(&unknown));
}

fn set_goal(session: Option<&str>, objective: &str) -> Intent {
    Intent::SetGoal {
        tab: 7,
        session: session.map(str::to_string),
        agent: "a".to_string(),
        objective: objective.to_string(),
    }
}

fn is_goal_answer(update: &Update) -> bool {
    match update {
        Update::Notice(notice) => notice == source::GOAL_SET,
        Update::StartFailed { .. } => true,
        _ => false,
    }
}

fn is_opened(update: &Update, at: &str) -> bool {
    matches!(update, Update::Opened { session, .. } if session == at)
}

#[tokio::test]
async fn a_goal_starts_with_its_objective_as_the_sessions_next_message() {
    let on = on_node(|_, method, params| match method {
        "thread/resume" => resumed(params, &[]),
        _ => quiet(method),
    })
    .await;

    on.shell.intend(set_goal(Some("s1"), "ship it"));
    let told = on.shell.until(is_goal_answer).await;

    let opened = told.iter().position(|update| is_opened(update, "s1"));
    assert!(opened.is_some(), "{told:?}");
    let set = Update::Notice(source::GOAL_SET.to_string());
    assert_eq!(told.last(), Some(&set));
    let access = on.work.access();
    let goals = access
        .execute(r#"{ Goal(filter: { session_id: { _eq: "s1" } }) { objective assignment_root_request_doc_id } }"#)
        .await
        .unwrap();
    let goals = goals["data"]["Goal"].as_array().unwrap();
    assert_eq!(goals.len(), 1, "{goals:?}");
    assert_eq!(goals[0]["objective"], "ship it");
    let root = goals[0]["assignment_root_request_doc_id"].as_str().unwrap();
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ content session_id }} }}"#,
        gents::store::graphql::escape_graphql_string(root)
    );
    let requests = access.execute(&query).await.unwrap();
    let request = &requests["data"]["AgentRequest"][0];
    assert_eq!(
        (&request["content"], &request["session_id"]),
        (&json!("ship it"), &json!("s1")),
        "{requests}"
    );
    on.node.shutdown().await;
}

#[tokio::test]
async fn a_goal_with_no_session_starts_one_first() {
    let on = on_node(|agent, method, params| match method {
        "thread/start" => Some(Ok(thread_answer(thread("s-new", 100)))),
        "thread/resume" => resumed(params, &[]),
        "thread/list" if agent == "a" && !is_child_list(method, params) => {
            page(&[thread("s-new", 100)], None)
        }
        _ => quiet(method),
    })
    .await;

    on.shell.intend(set_goal(None, "ship it"));
    let told = on.shell.until(is_goal_answer).await;

    let started = Update::Started {
        tab: 7,
        session: "s-new".to_string(),
    };
    let started = told.iter().position(|update| *update == started);
    let opened = told.iter().position(|update| is_opened(update, "s-new"));
    assert!(
        started
            .zip(opened)
            .is_some_and(|(started, opened)| started < opened),
        "{told:?}"
    );
    let lists_new = |update: &Update| match update {
        Update::Sessions(sessions) => sessions.iter().any(|session| session.id == "s-new"),
        _ => false,
    };
    let listed = told.iter().position(lists_new);
    assert!(
        opened
            .zip(listed)
            .is_some_and(|(opened, listed)| opened < listed),
        "the new session joins the list: {told:?}"
    );
    let set = Update::Notice(source::GOAL_SET.to_string());
    assert_eq!(told.last(), Some(&set));
    on.node.shutdown().await;
}

#[tokio::test]
async fn a_refused_goal_shows_the_owners_reason() {
    let on = on_node(|_, method, params| match method {
        "thread/resume" => resumed(params, &[]),
        _ => quiet(method),
    })
    .await;
    on.shell.intend(set_goal(Some("s1"), "ship it"));
    let first = on.shell.until(is_goal_answer).await;
    let set = Update::Notice(source::GOAL_SET.to_string());
    assert_eq!(first.last(), Some(&set), "the first goal is set");

    on.shell.intend(set_goal(Some("s1"), "ship more"));
    let told = on.shell.until(is_goal_answer).await;

    let Some(Update::StartFailed { tab: 7, message }) = told.last() else {
        panic!("{told:?}");
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let options = crate::RequestSubmitOptions {
        retry_key: Some(format!("goal-request:{request_id}")),
        ..Default::default()
    };
    let (did, work) = (&on.did, &on.work);
    let prepared = crate::prepare_agent_request_with_input(
        &work.endpoint,
        did,
        "ship more",
        Some("s1"),
        Some("a"),
        Some(request_id),
        options,
    );
    let create = prepared.await.unwrap().create;
    let access = work.access();
    let owner =
        gents::goal::submit_goal_backed_request(&access, did, "s1", "ship more", None, &create);
    let owner = owner.await.unwrap_err();
    assert_eq!(*message, format!("Couldn’t set the goal: {owner:#}"));
    on.node.shutdown().await;
}

#[tokio::test]
async fn goal_is_off_without_work() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let off = "jobs and the mailbox are off: no runtime".to_string();
    let home = tempfile::tempdir().unwrap();
    let work = Err(off.clone());
    let shell = open_shell_on(&fake, None, None, ANSWER_WAIT, work, home.path().into());
    shell.opened().await;

    shell.intend(set_goal(None, "ship it"));
    let told = shell.until(is_goal_answer).await;

    let refused = Update::StartFailed {
        tab: 7,
        message: off,
    };
    assert_eq!(told.last(), Some(&refused));
    assert!(!fake.methods().contains(&"thread/start".to_string()));
}

#[tokio::test]
async fn a_lost_connection_names_the_next_step() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);
    shell.ready().await;

    fake.close("a");

    let lost = "runtime closed: check `gents status`, then reopen the shell";
    assert_eq!(
        shell.next().await,
        Update::Connection(Connection::Lost(lost.to_string()))
    );
}

#[tokio::test]
async fn a_broken_connection_names_its_error() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);
    shell.ready().await;

    fake.sever("a");

    let update = shell.next().await;
    let Update::Connection(Connection::Lost(lost)) = &update else {
        panic!("{update:?}");
    };
    assert!(
        lost.starts_with("runtime closed: check `gents status`, then reopen the shell (")
            && lost.contains("without closing handshake")
            && lost.ends_with(')'),
        "{lost}"
    );
}

/// The status bar clips on the right, so the next step leads.
#[test]
fn the_status_bar_shows_the_whole_next_step_of_a_closed_connection() {
    let error = "IO error: Connection reset by peer (os error 54)".to_string();
    let lost = source::closed(Some(error));
    for width in [70, 80] {
        let (intents, _intended) = std::sync::mpsc::channel();
        let mut app = App::connected(KeySet::Vim, width, 36, intents);
        app.apply(Update::Connection(Connection::Lost(lost.clone())));
        let area = Rect::new(0, 0, width, 1);
        let mut status = Buffer::empty(area);
        gents_shell::draw::status::draw(&app, area, &mut status);
        let shown: String = (0..width).map(|x| status[(x, 0)].symbol()).collect();
        let next = "runtime closed: check `gents status`, then reopen the shell";
        assert!(shown.contains(next), "{width}: {shown}");
    }
}

#[tokio::test]
async fn an_unreadable_runtime_is_lost_before_anything_connects() {
    let fake = FakeRuntime::start(None, |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);

    let lost = format!(
        "no runtime: check `gents status`, then reopen the shell (could not read this home's runtime at {})",
        fake.remote()
    );
    assert_eq!(
        shell.next().await,
        Update::Connection(Connection::Lost(lost))
    );
    assert!(fake.calls().is_empty(), "{:?}", fake.methods());
}

#[tokio::test]
async fn the_branch_comes_from_git_in_the_working_directory() {
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git(&["init", "-b", "trunk"]);
    git(&["commit", "--allow-empty", "-m", "start"]);
    let fake = FakeRuntime::start(Some(identity(None)), |_, method, _| quiet(method)).await;
    let endpoint = GraphqlEndpoint::with_delegated_authorization(fake.graphql(), "Bearer shell")
        .expect("a bearer");
    let home = std::path::PathBuf::new();
    let work = Work {
        endpoint,
        home: home.clone(),
    };
    let shell = open_shell_in(
        &fake,
        None,
        None,
        ANSWER_WAIT,
        Ok(work),
        home,
        Some(repo.path().into()),
    );

    assert!(matches!(shell.next().await, Update::Agents(_)));
    assert_eq!(shell.next().await, Update::Connection(Connection::Ready));
    let branch = Update::Branch("trunk".to_string());
    // `next` panics when nothing more arrives within its wait.
    while shell.next().await != branch {}
}

#[tokio::test]
async fn a_pinned_runtime_serves_only_its_bound_agent_at_the_endpoint() {
    let fake = FakeRuntime::start(Some(identity(None)), |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);

    let update = shell.next().await;
    let Update::Agents(agents) = update else {
        panic!("{update:?}");
    };
    let ids: Vec<_> = agents.iter().map(|agent| agent.id.as_str()).collect();
    assert_eq!(ids, ["a"]);
    assert_eq!(shell.next().await, Update::Connection(Connection::Ready));
    assert_eq!(shell.next().await, Update::Sessions(Vec::new()));
    let routes: Vec<_> = fake.calls().into_iter().map(|call| call.agent).collect();
    assert_eq!(routes, ["/", "/", "/"], "{:?}", fake.methods());
}

#[tokio::test]
async fn a_request_with_no_answer_times_out() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/read" => Some(Ok(json!({ "ok": true }))),
        _ => None,
    })
    .await;
    let (events, _events) = mpsc::unbounded_channel();
    let client = connect_answering(&fake, "a", events, SHORT_WAIT).await;

    let silent = within(client.request::<_, Value>("thread/list", &json!({})))
        .await
        .expect_err("nothing answers");

    assert!(
        matches!(
            silent,
            ClientError::Timeout {
                method: "thread/list",
                secs: 1
            }
        ),
        "{silent:?}"
    );
    assert_eq!(
        silent.to_string(),
        "no answer from the runtime: check `gents status`, then reopen the shell (thread/list waited 1s)"
    );
    // A late answer to the expired request is dropped, and the connection still answers.
    let late = fake.calls().last().unwrap().id.clone();
    fake.push("a", &json!({ "id": late, "result": {} }));
    let answered: Value = within(client.request("thread/read", &json!({})))
        .await
        .unwrap();
    assert_eq!(answered, json!({ "ok": true }));
}

#[tokio::test]
async fn an_unanswered_request_becomes_a_notice_and_the_shell_goes_on() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "thread/read" => read_answer(json!([turn("t1", "inProgress", json!([]))])),
        "turn/interrupt" => Some(Ok(json!({}))),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell_answering(&fake, SHORT_WAIT);
    shell.ready().await;

    shell.intend(Intent::Send {
        tab: 0,
        session: Some("s1".to_string()),
        agent: "a".to_string(),
        text: "hi".to_string(),
    });
    let expired = |method: &str| {
        format!("no answer from the runtime: check `gents status`, then reopen the shell ({method} waited 1s)")
    };
    let unanswered = Update::StartFailed {
        tab: 0,
        message: expired("turn/start"),
    };
    assert_eq!(shell.next().await, unanswered);
    reread(&shell, 1).await;
    shell.intend(Intent::Send {
        tab: 1,
        session: None,
        agent: "a".to_string(),
        text: "hi".to_string(),
    });
    let failed = Update::StartFailed {
        tab: 1,
        message: expired("thread/start"),
    };
    assert_eq!(shell.next().await, failed);

    shell.intend(Intent::Interrupt {
        session: "s1".to_string(),
        agent: "a".to_string(),
        turn: "t1".to_string(),
    });
    assert_eq!(shell.next().await, t1_ended());
    let started = json!({ "threadId": "s1", "turn": turn("t2", "inProgress", json!([])) });
    fake.push("a", &json!({ "method": "turn/started", "params": started }));
    let running = Update::Turn {
        session: "s1".to_string(),
        turn: "t2".to_string(),
    };
    assert_eq!(shell.next().await, running);
}

/// An initialized home whose recorded runtime serves GraphQL at `graphql`, signed by `key` when
/// one is given.
pub(super) fn home_serving(
    graphql: &str,
    key: Option<&std::path::Path>,
) -> (tempfile::TempDir, String) {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().to_string_lossy().into_owned();
    let node_did = match key {
        Some(key) => {
            let identity = gents::store::identity::KeyIdentity::load_or_create(key, None).unwrap();
            let did = gents::store::identity::NodeIdentity::did(&identity).to_string();
            crate::home_state::write_init_config(
                home.path(),
                &crate::shared::StoredInitConfig {
                    home: path.clone(),
                    node_name: "fixture".into(),
                    node_did: did.clone(),
                    key_path: Some(key.to_string_lossy().into_owned()),
                    identity_backend: None,
                    keychain_label: None,
                    secure_enclave_label: None,
                    tool_package: None,
                    tool_ceiling: crate::cli::args::ToolCeilingArg::MetaOnly,
                    tool_root: None,
                    store_encryption: None,
                },
            )
            .unwrap();
            did
        }
        None => "did:key:z6MkuNotThisShells".to_string(),
    };
    crate::home_state::write_runtime_state(
        home.path(),
        &crate::shared::StoredRuntimeState {
            home: path,
            graphql: graphql.to_string(),
            node_name: "fixture".into(),
            node_did: node_did.clone(),
            default_agent_id: "a".into(),
            p2p_transport: "none".into(),
            p2p_peer_id: None,
            p2p_listen_addresses: Vec::new(),
            p2p_admission: None,
        },
    )
    .unwrap();
    (home, node_did)
}

#[test]
fn work_for_signs_as_this_homes_node() {
    let keys = tempfile::tempdir().unwrap();
    let key = keys.path().join("principal.key");
    let graphql = "http://127.0.0.1:39999/api/v0/graphql";
    let (home, did) = home_serving(graphql, Some(&key));

    let work = work::work_for(home.path()).expect("this home's node signs");

    assert_eq!(work.endpoint, GraphqlEndpoint::as_principal(graphql, did));
}

/// A recorded runtime without an initialized identity would be read anonymously, and an
/// anonymous read shows nothing.
#[test]
fn work_for_turns_off_when_the_endpoint_would_be_anonymous() {
    let (home, _) = home_serving("http://127.0.0.1:39999/api/v0/graphql", None);

    let off = work::work_for(home.path())
        .err()
        .expect("no node to sign as");

    assert!(off.starts_with("jobs and the mailbox are off: "), "{off}");
    assert!(off.contains("gents status"), "{off}");

    let bare = tempfile::tempdir().unwrap();
    let off = work::work_for(bare.path())
        .err()
        .expect("no runtime recorded");
    assert!(off.starts_with("jobs and the mailbox are off: "), "{off}");
    assert!(off.contains("gents runtime server"), "{off}");
}

#[tokio::test]
async fn an_unavailable_graphql_is_told_once_and_the_shell_still_opens() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let off = "jobs and the mailbox are off: no runtime".to_string();
    let home = std::path::PathBuf::new();
    let shell = open_shell_on(&fake, None, None, ANSWER_WAIT, Err(off.clone()), home);

    shell.opened().await;
    assert_eq!(shell.next().await, Update::Notice(off));
    let off_jobs = Update::Jobs {
        jobs: Vec::new(),
        as_of: "off".to_string(),
    };
    assert_eq!(shell.next().await, off_jobs);
    let off_calls = Update::Background {
        calls: Vec::new(),
        as_of: "off".to_string(),
    };
    assert_eq!(shell.next().await, off_calls);
    fake.push(
        "a",
        &item_note("item/completed", "s1", agent_message("r1", "hi")),
    );

    let item = shell.next().await;
    assert!(
        matches!(item, Update::Item { .. }),
        "one notice only: {item:?}"
    );
}

/// A running plugin job's row in session `session`, as `list_jobs` reads it.
fn job_row(session: &str, call: &str) -> Value {
    json!({
        "_docID": format!("bae-{call}"),
        "session_id": session,
        "tool_call_id": call,
        "request_id": "r1",
        "request_doc_id": "bae-r1",
        "node_did": "did:key:z6MkNode",
        "requester_did": null,
        "lifecycle_state": "running",
        "started_at": "2026-11-05T12:00:00Z",
        "completed_at": null,
        "job_pin": {
            "script_id": "change_unit",
            "source_digest": "s1",
            "origin": "installed",
            "engine_digest": "e1",
            "plugin_ref": "gents/change-unit",
            "agent_budget": 4,
            "handle": "change-unit-7",
            "cwd": "/srv",
            "adopted_at": 0,
            "args_digest": "a1",
            "chain_root_doc_id": format!("bae-{call}"),
            "ceiling_at_pin": {
                "targets": [],
                "max_live": 1,
                "max_spawns": 1,
                "max_messages_per_slot": 1,
                "result_bytes": 1024,
                "max_wall_secs": 60,
            },
        },
        "job_progress": {
            "title": "cli ux",
            "state": "running",
            "phases": [
                { "title": "build", "state": "active" },
                { "title": "test", "state": "pending" },
            ],
            "active": [],
            "spawned": 0,
            "budget": { "total": 4, "spent": 0, "remaining": 4 },
            "last_event": "",
            "observed_terminal_children": 0,
        },
    })
}

/// A job row of `session` that ended in the tool-call `state`.
fn ended_row(session: &str, call: &str, state: &str) -> Value {
    let mut row = job_row(session, call);
    row["lifecycle_state"] = json!(state);
    row
}

fn background_row(tool: &str, state: &str) -> Value {
    json!({
        "request_id": "r1",
        "tool_call_id": format!("{tool}-call"),
        "tool_name": tool,
        "status": null,
        "lifecycle_state": state,
        "await_mode": "background",
        "started_at": null,
        "session_id": "s1",
    })
}

/// Answers `list_jobs`' query with the `jobs` of the session it names, as the node filters
/// them, the background owner's with `calls`, and the children and output reads with none.
fn work_answer(query: &str, jobs: &[Value], calls: &[Value]) -> Value {
    if query.contains("job_pin") {
        let named = |job: &&Value| {
            query.contains(&format!(r#"session_id: {{ _eq: {} }}"#, job["session_id"]))
        };
        let jobs: Vec<_> = jobs.iter().filter(named).collect();
        json!({ "data": { "AgentToolCall": jobs } })
    } else if query.contains("await_mode") {
        json!({ "data": { "AgentToolCall": calls } })
    } else {
        json!({ "data": { "AgentRequest": [], "AgentOutputSegment": [] } })
    }
}

fn shown_job(session: &str, call: &str) -> Job {
    Job {
        address: format!("{session}:{call}"),
        session: session.to_string(),
        name: "change-unit".to_string(),
        title: "cli ux".to_string(),
        status: "build · step 1 of 2".to_string(),
        waiting: false,
        ended: None,
        detail: vec!["● build".to_string(), "○ test".to_string()],
    }
}

fn shown_ended(session: &str, call: &str, status: &str, end: JobEnd) -> Job {
    Job {
        status: status.to_string(),
        ended: Some(end),
        ..shown_job(session, call)
    }
}

fn job_running(session: &str, call: &str) -> Running {
    Running {
        name: "change-unit".to_string(),
        job: "cli ux".to_string(),
        status: "build · step 1 of 2".to_string(),
        state: RunState::Working,
        opens: Opens::Workflow(format!("{session}:{call}")),
    }
}

const RUNNING_CALLS: &str = r#"lifecycle_state: { _in: ["pending", "running"] }"#;

#[tokio::test]
async fn jobs_and_background_calls_are_read_once_the_shell_is_ready() {
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |_, method, _| quiet(method),
        |query| {
            let calls = [background_row("bash", "pending")];
            work_answer(query, &[job_row("s1", "call-1")], &calls)
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.opened().await;

    let jobs = Update::Jobs {
        jobs: Vec::new(),
        as_of: "live".to_string(),
    };
    assert_eq!(
        shell.next().await,
        jobs,
        "no session is open to list jobs for"
    );
    let calls = Update::Background {
        calls: vec![BackgroundCall {
            session: "s1".to_string(),
            name: "bash".to_string(),
            status: "pending".to_string(),
            done: false,
        }],
        as_of: "live".to_string(),
    };
    assert_eq!(shell.next().await, calls);
    let queries = fake.queries();
    assert_eq!(
        queries[0],
        source::WATCHED,
        "it subscribes before it reads: {queries:?}"
    );
    let background = queries.last().unwrap();
    assert!(background.contains("await_mode"), "{queries:?}");
    assert!(
        background.contains(RUNNING_CALLS),
        "the node, not the shell, keeps only running calls: {background}"
    );
    assert!(background.contains("session_id"), "{background}");
}

/// The next `count` updates.
async fn take(shell: &Shell, count: usize) -> Vec<Update> {
    let mut read = Vec::with_capacity(count);
    for _ in 0..count {
        read.push(shell.next().await);
    }
    read
}

/// Whether `read` is exactly one work reread: the jobs, the background calls, then `open`
/// sessions' running rows.
fn is_reread(read: &[Update], open: usize) -> bool {
    let [Update::Jobs { .. }, Update::Background { .. }, rows @ ..] = read else {
        return false;
    };
    rows.len() == open
        && rows
            .iter()
            .all(|update| matches!(update, Update::Running { .. }))
}

/// Reads exactly what opening `session` tells before it returns: its transcript, then its
/// running rows.
async fn opening(shell: &Shell, session: &str) -> Vec<Update> {
    let read = take(shell, 2).await;
    let opened = matches!(
        &read[..],
        [Update::Opened { session: shown, .. }, Update::Running { session: listed, .. }]
            if shown == session && listed == session
    );
    assert!(opened, "not only opening {session}: {read:?}");
    read
}

/// Reads exactly the reread a session opening asks for, with `open` sessions open.
async fn reread(shell: &Shell, open: usize) -> Vec<Update> {
    let read = take(shell, open + 2).await;
    assert!(is_reread(&read, open), "not only the reread: {read:?}");
    read
}

/// Reads exactly the reread a session opening asks for and one update `wanted` matches, which
/// lands before or after it: the reread tells its updates together.
async fn reread_and<F>(shell: &Shell, wanted: F) -> Vec<Update>
where
    F: Fn(&Update) -> bool,
{
    let read = take(shell, 4).await;
    let after = wanted(&read[0]) && is_reread(&read[1..], 1);
    let before = is_reread(&read[..3], 1) && wanted(&read[3]);
    assert!(
        after || before,
        "not only the reread and one more: {read:?}"
    );
    read
}

fn is_item(update: &Update) -> bool {
    matches!(update, Update::Item { .. })
}

/// The `list_jobs` row reads: only they select `job_progress`.
fn job_reads(fake: &FakeRuntime) -> Vec<String> {
    let queries = fake.queries().into_iter();
    queries
        .filter(|query| query.contains("job_progress"))
        .collect()
}

fn call_reads(fake: &FakeRuntime) -> usize {
    let queries = fake.queries();
    queries
        .iter()
        .filter(|query| query.contains(RUNNING_CALLS))
        .count()
}

#[tokio::test]
async fn jobs_are_read_per_open_session_in_every_state() {
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |_, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ => quiet(method),
        },
        |query| {
            let jobs = [
                job_row("s1", "call-1"),
                ended_row("s1", "call-2", "completed"),
                job_row("s2", "call-3"),
                ended_row("s1", "call-4", "failed"),
                ended_row("s1", "call-5", "cancelled"),
                ended_row("s1", "call-6", "timedOut"),
            ];
            work_answer(query, &jobs, &[])
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    assert_eq!(job_reads(&fake), Vec::<String>::new(), "no session is open");

    open_s1(&shell);
    opening(&shell, "s1").await;

    let read = reread(&shell, 1).await;
    let jobs = vec![
        shown_job("s1", "call-1"),
        shown_ended("s1", "call-2", "finished", JobEnd::Done),
        shown_ended("s1", "call-4", "failed", JobEnd::Failed),
        shown_ended("s1", "call-5", "cancelled", JobEnd::Failed),
        shown_ended("s1", "call-6", "timed out", JobEnd::Failed),
    ];
    let listed = Update::Jobs {
        jobs,
        as_of: "live".to_string(),
    };
    assert_eq!(read[0], listed);
    let asked = job_reads(&fake);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].contains(r#"session_id: { _eq: "s1" }"#),
        "{asked:?}"
    );

    shell.intend(Intent::Open {
        session: "s2".to_string(),
        agent: "a".to_string(),
    });
    opening(&shell, "s2").await;

    let read = reread(&shell, 2).await;
    let mut listed: Vec<_> = read
        .iter()
        .filter_map(|update| match update {
            Update::Jobs { jobs, .. } => Some(jobs),
            _ => None,
        })
        .flatten()
        .map(|job| job.address.as_str())
        .collect();
    listed.sort();
    let all = [
        "s1:call-1",
        "s1:call-2",
        "s1:call-4",
        "s1:call-5",
        "s1:call-6",
        "s2:call-3",
    ];
    assert_eq!(listed, all);
    let asked = &job_reads(&fake)[1..];
    for session in ["s1", "s2"] {
        let named = format!(r#"session_id: {{ _eq: "{session}" }}"#);
        let reads = asked.iter().filter(|query| query.contains(&named)).count();
        assert_eq!(reads, 1, "{session}: {asked:?}");
    }
}

/// A session opens when a tab picks it or sends to it, and either reads its jobs.
#[tokio::test]
async fn opening_a_session_rereads_work() {
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |_, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            "turn/start" => turn_answer("t1"),
            _ => quiet(method),
        },
        |query| {
            let jobs = [job_row("s1", "call-1"), job_row("s2", "call-2")];
            work_answer(query, &jobs, &[])
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    open_s1(&shell);
    opening(&shell, "s1").await;

    let read = reread(&shell, 1).await;
    let s1 = Update::Jobs {
        jobs: vec![shown_job("s1", "call-1")],
        as_of: "live".to_string(),
    };
    assert_eq!(read[0], s1);

    shell.intend(Intent::Send {
        tab: 1,
        session: Some("s2".to_string()),
        agent: "a".to_string(),
        text: "hi".to_string(),
    });
    let turn = Update::Turn {
        session: "s2".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(shell.next().await, turn);

    let read = reread(&shell, 2).await;
    let jobs = read.iter().find_map(|update| match update {
        Update::Jobs { jobs, .. } => Some(jobs),
        _ => None,
    });
    assert!(
        jobs.is_some_and(|jobs| jobs.contains(&shown_job("s2", "call-2"))),
        "{read:?}"
    );
}

/// The no-polling rule: only an event, or the view being shown, reads again.
#[tokio::test]
async fn nothing_rereads_without_an_event() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);
    shell.ready().await;
    let read = fake.queries().len();

    let more = tokio::time::timeout(SHORT_WAIT, fake.queried(read + 1)).await;
    assert!(more.is_err(), "{:?}", fake.queries());

    shell.intend(Intent::Refresh);
    let update = shell.next().await;
    assert!(matches!(update, Update::Jobs { .. }), "{update:?}");
    let update = shell.next().await;
    assert!(matches!(update, Update::Background { .. }), "{update:?}");
    assert_eq!(
        fake.queries().len(),
        read + 1,
        "the calls, and no jobs with no session open: {:?}",
        fake.queries()
    );
    assert_eq!(fake.subscriptions(), 1, "a live subscription is kept");
}

#[tokio::test]
async fn a_tool_call_change_rereads_the_jobs() {
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let job = started.clone();
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |_, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ => quiet(method),
        },
        move |query| {
            let jobs = match job.load(std::sync::atomic::Ordering::SeqCst) {
                true => vec![job_row("s1", "call-1")],
                false => Vec::new(),
            };
            work_answer(query, &jobs, &[])
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    open_s1(&shell);
    opening(&shell, "s1").await;
    reread(&shell, 1).await;

    started.store(true, std::sync::atomic::Ordering::SeqCst);
    fake.changed();

    let jobs = Update::Jobs {
        jobs: vec![shown_job("s1", "call-1")],
        as_of: "live".to_string(),
    };
    assert_eq!(shell.next().await, jobs);
    let update = shell.next().await;
    assert!(matches!(update, Update::Background { .. }), "{update:?}");
}

fn turn_completed(fake: &FakeRuntime) {
    let completed = json!({ "threadId": "s9", "turn": turn("t1", "completed", json!([])) });
    fake.push(
        "a",
        &json!({ "method": "turn/completed", "params": completed }),
    );
}

/// The node sends no keep-alives and replays nothing, so an ended subscription is only noticed,
/// and the next turn subscribes again and rereads, once.
#[tokio::test]
async fn an_ended_subscription_goes_stale_until_the_next_turn() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);
    shell.ready().await;
    let read = fake.queries().len();

    turn_completed(&fake);
    assert!(matches!(shell.next().await, Update::TurnEnded { .. }));
    assert!(matches!(shell.next().await, Update::Sessions(_)));
    let more = tokio::time::timeout(SHORT_WAIT, fake.queried(read + 1)).await;
    assert!(
        more.is_err(),
        "a live view needs no reread: {:?}",
        fake.queries()
    );

    fake.end_streams();
    for _ in 0..2 {
        let update = shell.next().await;
        let (Update::Jobs { as_of, .. } | Update::Background { as_of, .. }) = &update else {
            panic!("{update:?}");
        };
        assert!(as_of.starts_with("as of "), "{update:?}");
    }
    assert_eq!(fake.queries().len(), read, "ending reads nothing");

    turn_completed(&fake);
    assert!(matches!(shell.next().await, Update::TurnEnded { .. }));
    assert!(matches!(shell.next().await, Update::Sessions(_)));
    let live = |update: &Update| matches!(update, Update::Jobs { as_of, .. } | Update::Background { as_of, .. } if as_of == "live");
    assert!(live(&shell.next().await));
    assert!(live(&shell.next().await));
    assert_eq!(fake.subscriptions(), 2);
    assert_eq!(
        fake.queries()[read],
        source::WATCHED,
        "it subscribes before it reads"
    );

    turn_completed(&fake);
    assert!(matches!(shell.next().await, Update::TurnEnded { .. }));
    assert!(matches!(shell.next().await, Update::Sessions(_)));
    let more = tokio::time::timeout(SHORT_WAIT, fake.queried(read + 4)).await;
    assert!(more.is_err(), "live again: {:?}", fake.queries());
}

#[tokio::test]
async fn a_sessions_jobs_follow_its_subagents_in_its_running_rows() {
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |agent, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ if agent == "a" && is_child_list(method, params) => {
                page(&[child("c1", "s1", idle())], None)
            }
            _ => quiet(method),
        },
        |query| {
            let jobs = [
                job_row("s1", "call-1"),
                ended_row("s1", "call-3", "completed"),
                job_row("s2", "call-2"),
            ];
            work_answer(query, &jobs, &[])
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    open_s1(&shell);

    let opened = shell.next().await;
    assert!(matches!(opened, Update::Opened { .. }), "{opened:?}");
    let subagent = row("c1", "finished", RunState::Done);
    assert_eq!(shell.next().await, rows("s1", vec![subagent.clone()]));
    let read = reread(&shell, 1).await;
    let expected = vec![subagent, job_running("s1", "call-1")];
    assert_eq!(
        read.last(),
        Some(&rows("s1", expected.clone())),
        "a finished run is not running"
    );

    fake.changed();
    let update = shell.next().await;
    assert!(matches!(update, Update::Jobs { .. }), "{update:?}");
    let update = shell.next().await;
    assert!(matches!(update, Update::Background { .. }), "{update:?}");
    assert_eq!(shell.next().await, rows("s1", expected));
}

#[tokio::test]
async fn a_failed_job_read_is_one_notice_and_the_background_still_reads() {
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |_, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ => quiet(method),
        },
        |query| match query.contains("job_pin") {
            true => json!({ "data": null, "errors": [{ "message": "the job store is closed" }] }),
            false => work_answer(query, &[], &[background_row("bash", "running")]),
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    open_s1(&shell);
    opening(&shell, "s1").await;

    let update = shell.next().await;
    let Update::Notice(notice) = &update else {
        panic!("{update:?}");
    };
    assert!(
        notice.starts_with("jobs: job store unavailable: "),
        "{notice}"
    );
    assert!(notice.contains("the job store is closed"), "{notice}");
    let read = reread(&shell, 1).await;
    let [Update::Jobs { jobs, as_of }, Update::Background { calls, .. }, _] = &read[..] else {
        unreachable!()
    };
    assert!(jobs.is_empty(), "{jobs:?}");
    assert!(as_of.starts_with("as of "), "not live: {as_of}");
    assert_eq!(calls[0].name, "bash", "one notice, then the calls");
}

#[tokio::test]
async fn a_failed_reread_after_a_good_one_says_as_of() {
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fails = failing.clone();
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |_, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ => quiet(method),
        },
        move |query| {
            let failed = fails.load(std::sync::atomic::Ordering::SeqCst);
            match failed && query.contains("job_pin") {
                true => {
                    json!({ "data": null, "errors": [{ "message": "the job store is closed" }] })
                }
                false => work_answer(query, &[job_row("s1", "call-1")], &[]),
            }
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    open_s1(&shell);
    opening(&shell, "s1").await;
    reread(&shell, 1).await;

    failing.store(true, std::sync::atomic::Ordering::SeqCst);
    fake.changed();

    let update = shell.next().await;
    assert!(matches!(update, Update::Notice(_)), "{update:?}");
    let update = shell.next().await;
    let Update::Jobs { jobs, as_of } = &update else {
        panic!("{update:?}");
    };
    assert_eq!(jobs, &[shown_job("s1", "call-1")], "the last good rows");
    assert!(as_of.starts_with("as of "), "not live: {as_of}");
    let update = shell.next().await;
    assert!(
        matches!(&update, Update::Background { as_of, .. } if as_of == "live"),
        "{update:?}"
    );
}

/// A reread waits on the node, never the chat: deltas and intents go on meanwhile.
#[tokio::test]
async fn a_slow_reread_holds_back_no_chat() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| match method {
        "turn/start" => turn_answer("t1"),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    let read = fake.queries().len();
    fake.hold(true);
    fake.changed();
    within(fake.queried(read + 1)).await;

    let params = json!({ "threadId": "s1", "turnId": "t1", "itemId": "r1", "delta": "Hel" });
    fake.push(
        "a",
        &json!({ "method": "item/agentMessage/delta", "params": params }),
    );
    let update = shell.next().await;
    assert!(matches!(update, Update::Delta { .. }), "{update:?}");
    shell.intend(Intent::Send {
        tab: 0,
        session: Some("s1".to_string()),
        agent: "a".to_string(),
        text: "hi".to_string(),
    });
    let update = shell.next().await;
    assert!(matches!(update, Update::Turn { .. }), "{update:?}");

    fake.hold(false);
    let update = shell.next().await;
    assert!(matches!(update, Update::Jobs { .. }), "{update:?}");
    let update = shell.next().await;
    assert!(matches!(update, Update::Background { .. }), "{update:?}");
}

#[tokio::test]
async fn changes_during_a_reread_read_exactly_once_more() {
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |_, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ => quiet(method),
        },
        |query| work_answer(query, &[job_row("s1", "call-1")], &[]),
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    open_s1(&shell);
    opening(&shell, "s1").await;
    reread(&shell, 1).await;
    let (jobs, calls) = (job_reads(&fake).len(), call_reads(&fake));
    fake.hold(true);
    fake.changed();
    fake.posted("job_progress", jobs + 1).await;

    for _ in 0..3 {
        fake.changed();
    }
    let more = tokio::time::timeout(SHORT_WAIT, fake.posted("job_progress", jobs + 2)).await;
    assert!(
        more.is_err(),
        "one reread at a time while its job read is out: {:?}",
        fake.queries()
    );
    assert_eq!(
        call_reads(&fake),
        calls,
        "the jobs are read before the calls"
    );
    fake.hold(false);

    for _ in 0..2 {
        reread(&shell, 1).await;
    }
    let more = tokio::time::timeout(SHORT_WAIT, fake.posted(RUNNING_CALLS, calls + 3)).await;
    assert!(more.is_err(), "{:?}", fake.queries());
    assert_eq!(
        (job_reads(&fake).len(), call_reads(&fake)),
        (jobs + 2, calls + 2),
        "two rereads, each of s1's jobs and the calls"
    );
}

/// An open mailbox item on `session` for node `node`, created `at` minutes past noon.
pub(super) fn mailbox_item(
    id: &str,
    session: &str,
    kind: &str,
    action: &str,
    node: &str,
    at: u32,
) -> Value {
    json!({
        "_docID": id,
        "item_key": format!("key:{id}"),
        "requester_did": "did:key:z",
        "node_did": "did:key:z",
        "status": "open",
        "kind": kind,
        "action": action,
        "title": format!("{id} title"),
        "summary": null,
        "payload": null,
        "source_kind": "runtime",
        "source_id": "request-doc",
        "session_id": session,
        "request_id": null,
        "cause_doc_id": null,
        "target_node_did": node,
        "target_agent_id": "a",
        "expected_collection": null,
        "parent_item_id": null,
        "deadline_at": null,
        "created_at": format!("2026-11-05T12:{at:02}:00Z"),
        "updated_at": null,
        "resolved_at": null,
        "resolved_doc_id": null,
    })
}

/// Answers the open-mailbox list with the open `items`, newest first as the node orders them,
/// each `_docID` read with its item, and every other query with no work.
fn mailbox_answer(query: &str, items: &[Value]) -> Value {
    if !query.contains("MailboxItem") {
        return no_work(query);
    }
    let rows: Vec<&Value> = match query.split_once(r#"_docID: { _eq: ""#) {
        Some((_, rest)) => {
            let id = rest.split('"').next().unwrap();
            items.iter().filter(|item| item["_docID"] == id).collect()
        }
        None => {
            let mut open: Vec<_> = items
                .iter()
                .filter(|item| item["status"] == "open")
                .collect();
            open.sort_by_key(|item| {
                std::cmp::Reverse(item["created_at"].as_str().unwrap().to_string())
            });
            open
        }
    };
    json!({ "data": { "MailboxItem": rows } })
}

fn asked(id: &str, kind: PromptKind, answer: PromptAnswer) -> Prompt {
    Prompt {
        item: id.to_string(),
        kind,
        title: format!("{id} title"),
        summary: None,
        answer,
        dismissable: true,
    }
}

fn prompts(session: &str, prompts: Vec<Prompt>) -> Update {
    Update::Prompts {
        session: session.to_string(),
        prompts,
    }
}

fn is_prompts(update: &Update) -> bool {
    matches!(update, Update::Prompts { .. })
}

const MAILBOX_LIST: &str = r#"requester_did: { _eq: "did:key:z" }, status: { _eq: "open" }"#;

/// Runs `fake`'s threads as plain sessions, with `items` in its mailbox.
async fn with_mailbox<G>(graphql: G) -> FakeRuntime
where
    G: Fn(&str) -> Value + Send + Sync + 'static,
{
    FakeRuntime::start_with(
        agents_a_and_b(),
        |_, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ => quiet(method),
        },
        graphql,
    )
    .await
}

#[tokio::test]
async fn an_open_session_shows_its_ask_and_gate_items_oldest_first() {
    let items = [
        mailbox_item("i5", "s1", "ask", "ack", "did:key:zOther", 5),
        mailbox_item("i4", "s1", "finished", "ack", "did:key:z", 4),
        mailbox_item("i3", "s2", "ask", "ack", "did:key:z", 3),
        mailbox_item("i2", "s1", "gate", "ack", "did:key:z", 2),
        mailbox_item("i1", "s1", "ask", "start_request", "did:key:z", 1),
    ];
    let fake = with_mailbox(move |query| mailbox_answer(query, &items)).await;
    let shell = open_shell(&fake);
    shell.ready().await;

    open_s1(&shell);

    let read = shell.until(is_prompts).await;
    let shown = vec![
        asked("i1", PromptKind::Ask, PromptAnswer::Reply),
        asked("i2", PromptKind::Gate, PromptAnswer::Ack),
    ];
    assert_eq!(read.last(), Some(&prompts("s1", shown)));
    let queries = fake.queries();
    let watched = queries
        .iter()
        .position(|query| query == source::MAILBOX_WATCHED);
    let listed = queries
        .iter()
        .position(|query| query.contains(MAILBOX_LIST));
    assert!(
        watched.is_some() && watched < listed,
        "it subscribes before it reads: {queries:?}"
    );
    assert!(
        source::MAILBOX_WATCHED.contains(r#"kind: { _in: ["ask", "gate"] }"#),
        "only items a prompt shows: {}",
        source::MAILBOX_WATCHED
    );
}

/// An item answered or closed elsewhere while the tab shows it: opening the tab again shows
/// the prompts as they are now, not as they were.
#[tokio::test]
async fn reopening_an_open_tab_clears_a_prompt_answered_elsewhere() {
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let elsewhere = closed.clone();
    let fake = with_mailbox(move |query| {
        let mut item = mailbox_item("p1", "s1", "gate", "ack", "did:key:z", 1);
        if elsewhere.load(std::sync::atomic::Ordering::SeqCst) {
            item["status"] = json!("acted");
        }
        mailbox_answer(query, &[item])
    })
    .await;
    let shell = open_shell(&fake);
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;
    open_s1(&shell);
    let mut read = opening(&shell, "s1").await;
    read.extend(reread_and(&shell, is_prompts).await);
    for update in read {
        app.apply(update);
    }
    assert_eq!(app.tabs[0].prompts.len(), 1);
    closed.store(true, std::sync::atomic::Ordering::SeqCst);

    open_s1(&shell);
    let opened_rows =
        |update: &Update| matches!(update, Update::Running { session, .. } if session == "s1");
    shell.apply_through(&mut app, opened_rows).await;
    fake.posted(MAILBOX_LIST, 2).await;

    assert!(app.tabs[0].prompts.is_empty(), "{:?}", app.tabs[0].prompts);
}

/// A subagent's question lands on its own session, which the parent tab does not show, so the
/// parent's row for it says so.
#[tokio::test]
async fn a_subagent_waiting_on_a_question_says_so_in_its_parents_row() {
    let fake = FakeRuntime::start_with(
        agents_a_and_b(),
        |agent, method, params| match method {
            "thread/resume" => resumed(params, &[]),
            _ if agent == "a" && is_child_list(method, params) => {
                page(&[child("c1", "s1", idle())], None)
            }
            _ => quiet(method),
        },
        |query| {
            let asked = mailbox_item("i1", "c1", "ask", "start_request", "did:key:z", 1);
            mailbox_answer(query, &[asked])
        },
    )
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    open_s1(&shell);

    let waits = |update: &Update| matches!(update, Update::Running { rows, .. } if rows.iter().any(|row| row.status == "waiting on you"));
    let read = shell.until(waits).await;
    let waiting = Running {
        status: "waiting on you".to_string(),
        ..row("c1", "finished", RunState::Done)
    };
    assert_eq!(read.last(), Some(&rows("s1", vec![waiting])));
}

#[tokio::test]
async fn dismissing_sends_the_mailbox_dismiss_and_shows_the_rest() {
    let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dismissed = gone.clone();
    let fake = with_mailbox(move |query| {
        if query.contains("update_MailboxItem") {
            dismissed.store(true, std::sync::atomic::Ordering::SeqCst);
            return json!({ "data": { "update_MailboxItem": [{ "_docID": "i1" }] } });
        }
        let mut first = mailbox_item("i1", "s1", "gate", "ack", "did:key:z", 1);
        if dismissed.load(std::sync::atomic::Ordering::SeqCst) {
            first["status"] = json!("dismissed");
        }
        let items = [
            first,
            mailbox_item("i2", "s1", "ask", "start_request", "did:key:z", 2),
        ];
        mailbox_answer(query, &items)
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    open_s1(&shell);
    opening(&shell, "s1").await;
    let read = reread_and(&shell, is_prompts).await;
    let both = vec![
        asked("i1", PromptKind::Gate, PromptAnswer::Ack),
        asked("i2", PromptKind::Ask, PromptAnswer::Reply),
    ];
    let shown: Vec<_> = read.iter().filter(|update| is_prompts(update)).collect();
    assert_eq!(shown, [&prompts("s1", both)]);

    shell.intend(Intent::Dismiss {
        session: "s1".to_string(),
        item: "i1".to_string(),
    });

    let read = shell.until(is_prompts).await;
    let rest = vec![asked("i2", PromptKind::Ask, PromptAnswer::Reply)];
    assert_eq!(read, [prompts("s1", rest)], "no notice: {read:?}");
    assert!(gone.load(std::sync::atomic::Ordering::SeqCst));
    let queries = fake.queries();
    let mutation = queries
        .iter()
        .find(|query| query.contains("update_MailboxItem"))
        .unwrap();
    assert!(
        mutation.contains(r#"_docID: { _eq: "i1" }"#)
            && mutation.contains(r#"status: "dismissed""#),
        "{mutation}"
    );
}

/// An item closed elsewhere while the mailbox feed is quiet: the refused reply rereads, so the
/// tab is not left answering a prompt that is gone.
#[tokio::test]
async fn a_reply_to_a_closed_item_is_a_notice_in_the_owners_words() {
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let elsewhere = closed.clone();
    let fake = with_mailbox(move |query| {
        let mut item = mailbox_item("i1", "s1", "ask", "start_request", "did:key:z", 1);
        if elsewhere.load(std::sync::atomic::Ordering::SeqCst) {
            item["status"] = json!("acted");
        }
        mailbox_answer(query, &[item])
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    open_s1(&shell);
    opening(&shell, "s1").await;
    let read = reread_and(&shell, is_prompts).await;
    let question = vec![asked("i1", PromptKind::Ask, PromptAnswer::Reply)];
    let shown: Vec<_> = read.iter().filter(|update| is_prompts(update)).collect();
    assert_eq!(shown, [&prompts("s1", question)]);
    closed.store(true, std::sync::atomic::Ordering::SeqCst);

    shell.intend(Intent::Reply {
        session: "s1".to_string(),
        agent: "a".to_string(),
        item: "i1".to_string(),
        text: "ship it".to_string(),
    });

    let closed = Update::Notice("MailboxItem is no longer open".to_string());
    assert_eq!(
        shell.until(is_prompts).await,
        [closed, prompts("s1", Vec::new())]
    );
    assert_eq!(
        resumes_of(&fake, "s1"),
        1,
        "a refused reply reopens nothing"
    );
}

/// The mailbox subscription is the only reason to read the prompts again.
#[tokio::test]
async fn a_mailbox_change_rereads_the_open_sessions_prompts() {
    let asked_now = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let asking = asked_now.clone();
    let fake = with_mailbox(move |query| {
        let items = match asking.load(std::sync::atomic::Ordering::SeqCst) {
            true => vec![mailbox_item("i1", "s1", "gate", "ack", "did:key:z", 1)],
            false => Vec::new(),
        };
        mailbox_answer(query, &items)
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;
    open_s1(&shell);
    fake.posted(MAILBOX_LIST, 1).await;

    let more = tokio::time::timeout(SHORT_WAIT, fake.posted(MAILBOX_LIST, 2)).await;
    assert!(
        more.is_err(),
        "no reread without an event: {:?}",
        fake.queries()
    );

    asked_now.store(true, std::sync::atomic::Ordering::SeqCst);
    fake.mailbox_changed();

    let read = shell.until(is_prompts).await;
    let shown = vec![asked("i1", PromptKind::Gate, PromptAnswer::Ack)];
    assert_eq!(read.last(), Some(&prompts("s1", shown)));
}

/// The node replays nothing, so an ended subscription is told, and the next turn subscribes
/// again and rereads.
#[tokio::test]
async fn an_ended_mailbox_subscription_is_told_and_the_next_turn_subscribes_again() {
    let fake = with_mailbox(|query| mailbox_answer(query, &[])).await;
    let shell = open_shell(&fake);
    shell.ready().await;
    open_s1(&shell);
    fake.posted(MAILBOX_LIST, 1).await;

    fake.end_streams();

    let not_live = |update: &Update| matches!(update, Update::Notice(notice) if notice.starts_with("prompts are not live"));
    shell.until(not_live).await;
    turn_completed(&fake);
    fake.posted(MAILBOX_LIST, 2).await;
    let watched = fake
        .queries()
        .iter()
        .filter(|query| *query == source::MAILBOX_WATCHED)
        .count();
    assert_eq!(watched, 2, "{:?}", fake.queries());
}

fn is_menu(update: &Update) -> bool {
    matches!(update, Update::Menu { .. })
}

#[tokio::test]
async fn reading_the_mailbox_menu_lists_every_open_item() {
    let items = [
        mailbox_item("i2", "s1", "ask", "ack", "did:key:z", 2),
        mailbox_item("i1", "s1", "finished", "ack", "did:key:z", 1),
    ];
    let fake = with_mailbox(move |query| mailbox_answer(query, &items)).await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(Intent::ReadMenu(MenuId::Mailbox));

    let read = shell.until(is_menu).await;
    let row = |text: &str, tone, item: &str| MenuRow {
        text: text.to_string(),
        tone,
        enter: Some(MenuAct::OpenSession {
            session: "s1".to_string(),
        }),
        d: Some(MenuAct::Dismiss {
            item: item.to_string(),
        }),
        more: Vec::new(),
    };
    let rows = vec![
        row(" ! ask: i2 title", Tone::Warn, "i2"),
        row(" · finished: i1 title", Tone::Plain, "i1"),
    ];
    let listed = Update::Menu {
        menu: MenuId::Mailbox,
        rows: Ok(rows),
    };
    assert_eq!(read.last(), Some(&listed));
}

/// The installed workflows are read from the home alone.
#[tokio::test]
async fn a_menu_without_work_says_why() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let off = "jobs and the mailbox are off: no runtime".to_string();
    let home = tempfile::tempdir().unwrap();
    let work = Err(off.clone());
    let shell = open_shell_on(&fake, None, None, ANSWER_WAIT, work, home.path().into());
    shell.opened().await;

    shell.intend(Intent::ReadMenu(MenuId::Mailbox));
    let read = shell.until(is_menu).await;
    let why = Update::Menu {
        menu: MenuId::Mailbox,
        rows: Err(off),
    };
    assert_eq!(read.last(), Some(&why));

    shell.intend(Intent::ReadMenu(MenuId::Workflows));
    let read = shell.until(is_menu).await;
    let installed = Update::Menu {
        menu: MenuId::Workflows,
        rows: Ok(Vec::new()),
    };
    assert_eq!(read.last(), Some(&installed));
}

fn dismiss_from_the_mailbox(item: &str) -> Intent {
    Intent::Act {
        menu: MenuId::Mailbox,
        act: MenuAct::Dismiss {
            item: item.to_string(),
        },
    }
}

#[tokio::test]
async fn an_act_notices_and_rereads_its_menu() {
    let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dismissed = gone.clone();
    let fake = with_mailbox(move |query| {
        if query.contains("update_MailboxItem") {
            dismissed.store(true, std::sync::atomic::Ordering::SeqCst);
            return json!({ "data": { "update_MailboxItem": [{ "_docID": "i1" }] } });
        }
        let mut item = mailbox_item("i1", "s1", "finished", "ack", "did:key:z", 1);
        if dismissed.load(std::sync::atomic::Ordering::SeqCst) {
            item["status"] = json!("dismissed");
        }
        mailbox_answer(query, &[item])
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(dismiss_from_the_mailbox("i1"));

    let reread = Update::Menu {
        menu: MenuId::Mailbox,
        rows: Ok(Vec::new()),
    };
    assert_eq!(
        shell.until(is_menu).await,
        [Update::Notice("dismissed".to_string()), reread]
    );
    assert!(gone.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn a_refused_act_notices_why_and_rereads() {
    let fake = with_mailbox(|query| {
        let mut question = mailbox_item("i1", "s1", "ask", "start_request", "did:key:z", 1);
        question["payload"] = json!(r#"{"version":2}"#);
        mailbox_answer(query, &[question])
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(dismiss_from_the_mailbox("i1"));

    let read = shell.until(is_menu).await;
    let [Update::Notice(why), reread] = &read[..] else {
        panic!("{read:?}");
    };
    assert!(why.starts_with("could not dismiss it: "), "{why}");
    let still_open = MenuRow {
        text: " ! ask: i1 title".to_string(),
        tone: Tone::Warn,
        enter: Some(MenuAct::OpenSession {
            session: "s1".to_string(),
        }),
        d: Some(MenuAct::Dismiss {
            item: "i1".to_string(),
        }),
        more: Vec::new(),
    };
    let listed = Update::Menu {
        menu: MenuId::Mailbox,
        rows: Ok(vec![still_open]),
    };
    assert_eq!(reread, &listed);
    let wrote = fake
        .queries()
        .iter()
        .any(|query| query.contains("update_MailboxItem"));
    assert!(!wrote, "{:?}", fake.queries());
}

fn read_tree(agent: &str) -> Intent {
    Intent::WorkingTree {
        agent: agent.to_string(),
    }
}

fn is_tree(update: &Update) -> bool {
    matches!(update, Update::WorkingTree(_))
}

#[tokio::test]
async fn the_working_tree_is_read_through_git_diff_to_remote() {
    let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n";
    let fake = FakeRuntime::start(agents_a_and_b(), move |_, method, _| match method {
        "gitDiffToRemote" => Some(Ok(json!({ "sha": "abc123", "diff": diff }))),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(read_tree("a"));

    let read = shell.until(is_tree).await;
    let edited = Change::new(
        String::new(),
        "a.rs".to_string(),
        ChangeKind::Update,
        "@@ -1 +1 @@\n-a\n+b\n".to_string(),
    );
    assert_eq!(read.last(), Some(&Update::WorkingTree(Ok(vec![edited]))));
    let wire = codex::ClientRequest::GitDiffToRemote {
        request_id: codex::RequestId::Integer(0),
        params: codex::GitDiffToRemoteParams { cwd: ".".into() },
    };
    let calls = fake.calls();
    let call = calls.iter().find(|call| call.method == wire.method());
    let call = call.expect("the runtime's git diff");
    assert_eq!(call.agent, "a");
    assert_eq!(call.params, json!({ "cwd": "." }));
}

#[tokio::test]
async fn a_refused_working_tree_read_says_why() {
    let why = "git rev-parse --show-toplevel failed: fatal: not a git repository";
    let fake = FakeRuntime::start(agents_a_and_b(), move |_, method, _| match method {
        "gitDiffToRemote" => Some(Err((-32603, why.to_string()))),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(read_tree("a"));
    let read = shell.until(is_tree).await;
    assert_eq!(
        read.last(),
        Some(&Update::WorkingTree(Err(why.to_string())))
    );

    shell.intend(read_tree("nobody"));
    let read = shell.until(is_tree).await;
    let unconnected = Update::WorkingTree(Err("not connected".to_string()));
    assert_eq!(read.last(), Some(&unconnected));
}

#[tokio::test]
async fn a_working_tree_read_holds_up_no_other_intent() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let shell = open_shell(&fake);
    shell.ready().await;

    shell.intend(read_tree("a"));
    fake.called("gitDiffToRemote").await;
    shell.intend(Intent::Refresh);

    let update = shell.next().await;
    assert!(matches!(update, Update::Jobs { .. }), "{update:?}");
}

/// The first read is left unanswered until it times out, and the next two are asked for while
/// it is out: they become one read, so the older answer never lands after the newer one.
#[tokio::test]
async fn working_tree_reads_land_in_order() {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = asked.clone();
    let fake = FakeRuntime::start(agents_a_and_b(), move |_, method, _| match method {
        "gitDiffToRemote" if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => None,
        "gitDiffToRemote" => Some(Ok(json!({ "sha": "abc123", "diff": "" }))),
        _ => quiet(method),
    })
    .await;
    let shell = open_shell_answering(&fake, SHORT_WAIT);
    shell.ready().await;

    for _ in 0..3 {
        shell.intend(read_tree("a"));
    }

    let first = shell.until(is_tree).await;
    assert!(
        matches!(first.last(), Some(Update::WorkingTree(Err(_)))),
        "the unanswered read lands first: {first:?}"
    );
    let second = shell.until(is_tree).await;
    assert_eq!(second.last(), Some(&Update::WorkingTree(Ok(Vec::new()))));
    let reads = fake
        .methods()
        .iter()
        .filter(|method| *method == "gitDiffToRemote")
        .count();
    assert_eq!(reads, 2, "the two asked while one was out are read once");
}

fn node_settings() -> Value {
    node_settings_of("owner")
}

fn node_settings_of(did: &str) -> Value {
    json!({
        "node": { "node_did": did, "default_agent_id": "engineer" },
        "inference_backends": [{
            "node_did": did,
            "backend_id": "backend",
            "name": "Backend",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:1/v1",
            "auth": { "kind": "unauthenticated" },
        }],
        "inference_profiles": [{
            "node_did": did,
            "profile_id": "profile",
            "backend_id": "backend",
            "model_name": "model",
        }],
        "agents": [{
            "node_did": did,
            "agent_id": "engineer",
            "display_name": "Engineer",
            "inference_profile_id": "profile",
        }],
        "tasks": [{
            "node_did": did,
            "task_id": "triage",
            "display_name": "Triage",
            "agent_id": "engineer",
            "prompt_template": "triage the inbox",
        }],
        "schedules": [{
            "node_did": did,
            "schedule_id": "daily",
            "display_name": "Daily",
            "cadence": { "kind": "cron", "expression": "0 2 * * *", "timezone": "UTC" },
        }],
        "triggers": [{
            "node_did": did,
            "trigger_id": "nightly",
            "display_name": "Nightly",
            "task_id": "triage",
            "source": { "kind": "schedule", "schedule_id": "daily" },
        }],
        "skills": [{
            "node_did": did,
            "skill_id": "review",
            "name": "review",
            "display_name": "Review",
            "tool_refs": ["read", "grep"],
        }, {
            "node_did": did,
            "skill_id": "lint",
        }],
    })
}

/// A node served with `node_settings`, and the shell's work on it.
async fn served_settings() -> (Arc<defra_node::EmbeddedNode>, Work, tempfile::TempDir) {
    served(node_settings()).await
}

/// A node served with `config`, and the shell's work on it.
async fn served(config: Value) -> (Arc<defra_node::EmbeddedNode>, Work, tempfile::TempDir) {
    let (node, graphql) = crate::shared::test_support::served_config(config).await;
    let home = tempfile::tempdir().unwrap();
    let work = Work {
        endpoint: GraphqlEndpoint::anonymous(graphql),
        home: home.path().into(),
    };
    (node, work, home)
}

async fn menu_texts(work: &Work, menu: MenuId) -> Vec<String> {
    let rows = super::menus::read_menu(&Ok(work.clone()), &work.home, "owner", menu).await;
    rows.unwrap().into_iter().map(|row| row.text).collect()
}

#[tokio::test]
async fn a_skill_turns_off_on_a_served_node() {
    let (node, work, home) = served_settings().await;
    let off = MenuAct::SetEnabled {
        doc: Doc::Skill,
        id: "review".to_string(),
        label: "Review".to_string(),
        enabled: false,
    };

    let turned = super::menus::act(&work, home.path(), "owner", &off).await;

    assert_eq!(turned, Ok("Review is off".to_string()));
    assert_eq!(
        menu_texts(&work, MenuId::Skills).await,
        [
            " + New skill",
            " ● lint   0 tools",
            " ○ Review   2 tools · disabled"
        ]
    );
    node.shutdown().await;
}

#[tokio::test]
async fn a_skill_copy_is_renamed_as_the_desktop_does() {
    let (node, work, home) = served_settings().await;
    let duplicate = |id: &str| MenuAct::Duplicate(Doc::Skill, id.to_string());

    let named = super::menus::act(&work, home.path(), "owner", &duplicate("review")).await;
    let bare = super::menus::act(&work, home.path(), "owner", &duplicate("lint")).await;

    assert_eq!(named, Ok("Duplicated Review".to_string()));
    assert_eq!(bare, Ok("Duplicated lint".to_string()));
    let skills = query_collection_sorted(&work.access(), SKILL_SPEC, "owner").await;
    let mut copies: Vec<_> = skills
        .unwrap()
        .into_iter()
        .filter(|skill| skill["skill_id"].as_str().unwrap().starts_with("skill-"))
        .map(|skill| {
            let shown = |field: &str| skill[field].clone();
            (shown("name"), shown("display_name"), shown("tool_refs"))
        })
        .collect();
    copies.sort_by_key(|(name, ..)| name.to_string());
    assert_eq!(
        copies,
        [
            (json!("lint-copy"), json!("lint copy"), Value::Null),
            (
                json!("review-copy"),
                json!("Review copy"),
                json!(["read", "grep"])
            ),
        ]
    );
    node.shutdown().await;
}

#[tokio::test]
async fn a_duplicate_is_a_new_document_off() {
    let (node, work, home) = served_settings().await;
    let duplicate = MenuAct::Duplicate(Doc::Trigger, "nightly".to_string());

    let duplicated = super::menus::act(&work, home.path(), "owner", &duplicate).await;

    assert_eq!(duplicated, Ok("Duplicated Nightly".to_string()));
    let work = Ok(work);
    let rows = super::menus::read_menu(&work, home.path(), "owner", MenuId::Triggers).await;
    let rows = rows.unwrap();
    let texts: Vec<_> = rows.iter().map(|row| row.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            " + New trigger",
            " ● Nightly   Every day at 02:00 · run Triage with Engineer",
            " ○ Nightly copy   Every day at 02:00 · run Triage with Engineer",
        ]
    );
    let Some(MenuAct::Open(FormRef::Edit(Doc::Trigger, copy))) = &rows[2].enter else {
        panic!("{:?}", rows[2]);
    };
    assert!(copy.starts_with("trig-"), "{copy}");
    node.shutdown().await;
}

#[tokio::test]
async fn a_trigger_reads_its_own_nodes_schedule() {
    let (node, work, _home) = served_settings().await;
    let other = json!({
        "node": { "node_did": "other" },
        "schedules": [{
            "node_did": "other",
            "schedule_id": "daily",
            "cadence": { "kind": "interval", "interval_secs": 60 },
        }],
    });
    let other = serde_json::from_value(other).unwrap();
    gents::documents::ensure_node(&node, "other").await.unwrap();
    gents_desktop_core::client::apply_config_components_on(&work.access(), &other)
        .await
        .unwrap();
    let theirs = query_collection_sorted(&work.access(), SCHEDULE_SPEC, "other").await;
    assert_eq!(theirs.unwrap().len(), 1);

    assert_eq!(
        menu_texts(&work, MenuId::Triggers).await,
        [
            " + New trigger",
            " ● Nightly   Every day at 02:00 · run Triage with Engineer"
        ]
    );
    node.shutdown().await;
}

#[tokio::test]
async fn a_schedule_in_use_is_not_deleted() {
    let (node, work, home) = served_settings().await;
    let delete = |doc, id: &str, label: &str| MenuAct::Delete {
        doc,
        id: id.to_string(),
        label: label.to_string(),
    };
    let daily = delete(Doc::Schedule, "daily", "Daily");

    let refused = super::menus::act(&work, home.path(), "owner", &daily).await;

    let Err(refused) = refused else {
        panic!("{refused:?}");
    };
    assert!(
        refused.starts_with("Couldn’t delete Daily: ") && refused.contains("nightly"),
        "{refused}"
    );
    assert_eq!(
        menu_texts(&work, MenuId::Schedules).await,
        [" + New schedule", " Daily   0 2 * * *"]
    );
    let nightly = delete(Doc::Trigger, "nightly", "Nightly");
    let trigger = super::menus::act(&work, home.path(), "owner", &nightly).await;
    let schedule = super::menus::act(&work, home.path(), "owner", &daily).await;
    assert_eq!(trigger, Ok("Deleted Nightly".to_string()));
    assert_eq!(schedule, Ok("Deleted Daily".to_string()));
    assert_eq!(
        menu_texts(&work, MenuId::Schedules).await,
        [
            " + New schedule",
            " No schedules.",
            " A trigger binds a task to a schedule."
        ]
    );
    node.shutdown().await;
}

/// The values the shell sends for `form` as it opened: no heading, fixed or hidden field.
fn as_opened(form: &gents_shell::source::Form) -> Vec<(String, String)> {
    use gents_shell::source::FieldKind;
    let sent = form.fields.iter().filter(|field| {
        let typed = !matches!(field.kind, FieldKind::Heading | FieldKind::Fixed);
        typed && shown(form, field)
    });
    sent.map(|field| (field.key.clone(), field.value.clone()))
        .collect()
}

/// Whether `form` shows `field`: its `when` holds, on a field it shows.
fn shown(form: &gents_shell::source::Form, field: &gents_shell::source::FormField) -> bool {
    field.when.as_ref().is_none_or(|(key, values)| {
        let on = form.fields.iter().find(|on| on.key == *key);
        on.is_some_and(|on| values.contains(&on.value) && shown(form, on))
    })
}

fn with(values: Vec<(String, String)>, key: &str, value: &str) -> Vec<(String, String)> {
    let edit = |(sent, was): (String, String)| match sent == key {
        true => (sent, value.to_string()),
        false => (sent, was),
    };
    values.into_iter().map(edit).collect()
}

#[tokio::test]
async fn owner_rules_come_back_in_the_status() {
    let (node, work, _home) = served_settings().await;
    let access = work.access();
    let (grouping, cadence) = (FormRef::New(Doc::EventSource), FormRef::New(Doc::Schedule));
    let grouped = super::forms::open(&access, "owner", &grouping, "evsrc-a").await;
    let grouped = with(
        as_opened(&grouped.unwrap()),
        "correlation_field",
        "session_id",
    );
    let grouped = with(grouped, "expected_count", "2");
    let grouped = with(grouped, "timeout_secs", "60");
    let grouped = with(grouped, "min_count", "3");
    let cron = super::forms::open(&access, "owner", &cadence, "sched-a").await;
    let cron = with(as_opened(&cron.unwrap()), "expression", "every day");

    let over =
        super::forms::save(&access, &work.home, "owner", &grouping, &grouped, "evsrc-a").await;
    let bad = super::forms::save(&access, &work.home, "owner", &cadence, &cron, "sched-a").await;

    let Err(FormError {
        field: None,
        message: over,
    }) = over
    else {
        panic!("{over:?}");
    };
    assert!(
        over.contains("min_count 3 exceeds expected_count 2"),
        "{over}"
    );
    let owner = gents::documents::schedule_cron::validate_cron_schedule("every day", "UTC", None);
    let owner = format!("{:#}", owner.unwrap_err());
    let Err(FormError {
        field: None,
        message: bad,
    }) = bad
    else {
        panic!("{bad:?}");
    };
    assert!(bad.contains(&owner), "{bad} does not carry {owner}");
    assert_eq!(
        menu_texts(&work, MenuId::EventSources).await,
        [
            " + New event source",
            " No event sources.",
            " A trigger binds a task to a reusable source."
        ]
    );
    assert_eq!(
        menu_texts(&work, MenuId::Schedules).await,
        [" + New schedule", " Daily   0 2 * * *"]
    );
    node.shutdown().await;
}

#[tokio::test]
async fn an_edit_keeps_what_the_form_does_not_show() {
    let mut config = node_settings();
    config["contexts"] = json!([
        { "node_did": "owner", "context_id": "main", "skill_ids": ["commit"] },
    ]);
    config["agents"][0]["context_id"] = json!("main");
    config["skills"].as_array_mut().unwrap().push(json!({
        "node_did": "owner",
        "skill_id": "commit",
        "name": "commit",
        "display_name": "Commit",
        "instructions": "Stage, then commit.",
        "created_at": "2026-01-01T00:00:00Z",
        "tags": ["gents:pack:git", "vcs"],
    }));
    let (node, work, _home) = served(config).await;
    let access = work.access();
    let commit = FormRef::Edit(Doc::Skill, "commit".to_string());

    let form = super::forms::open(&access, "owner", &commit, "commit").await;
    let form = form.unwrap();
    let values = with(as_opened(&form), "display_name", "Commit changes");
    let values = with(values, "tags", "vcs, daily");
    let saved = super::forms::save(&access, &work.home, "owner", &commit, &values, "commit").await;

    assert_eq!(form.note, ["Used by Engineer · From git"]);
    assert_eq!(saved, Ok("Saved".to_string()));
    let stored = crate::commands::config::crud::load_one(&access, SKILL_SPEC, "owner", "commit");
    let stored = stored.await.unwrap();
    assert_eq!(stored["display_name"], json!("Commit changes"));
    assert_eq!(stored["instructions"], json!("Stage, then commit."));
    assert_eq!(stored["tags"], json!(["gents:pack:git", "vcs", "daily"]));
    assert_eq!(stored["created_at"], json!("2026-01-01T00:00:00Z"));
    node.shutdown().await;
}

#[tokio::test]
async fn new_then_nothing_writes_nothing() {
    let (node, work, _home) = served_settings().await;
    let access = work.access();
    let skills = || query_collection_sorted(&access, SKILL_SPEC, "owner");
    let before = skills().await.unwrap();

    let new = FormRef::New(Doc::Skill);
    let form = super::forms::open(&access, "owner", &new, "skill-new").await;

    assert_eq!(form.unwrap().title, "New skill");
    assert_eq!(skills().await.unwrap(), before);
    node.shutdown().await;
}

#[tokio::test]
async fn a_new_form_saves_under_the_id_it_showed() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let (node, work, home) = served(node_settings_of("did:key:z")).await;
    let access = work.access();
    let shell = open_shell_on(&fake, None, None, ANSWER_WAIT, Ok(work), home.path().into());
    shell.opened().await;
    let new = FormRef::New(Doc::Skill);

    shell.intend(Intent::OpenForm {
        form: new.clone(),
        opening: 7,
    });
    let opened = shell
        .until(|update| matches!(update, Update::Form { .. }))
        .await;
    let Some(Update::Form {
        opening: 7,
        result: Ok(form),
        ..
    }) = opened.last()
    else {
        panic!("{opened:?}");
    };
    let id = form.fields.iter().find(|field| field.key == "skill_id");
    let id = id.unwrap().value.clone();
    let values = with(as_opened(form), "name", "drafts");
    shell.intend(Intent::SaveForm {
        form: new.clone(),
        opening: 7,
        values: FormValues(values.clone()),
    });
    let saved = shell
        .until(|update| matches!(update, Update::FormSaved { .. }))
        .await;
    let act = gents_shell::source::FormAct::RunSchedule;
    shell.intend(Intent::FormAct {
        form: new.clone(),
        opening: 7,
        act,
        values: FormValues(values),
    });
    let noted = shell
        .until(|update| matches!(update, Update::FormNote { .. }))
        .await;

    let done = Update::FormSaved {
        form: new.clone(),
        opening: 7,
        result: Ok("Saved".to_string()),
    };
    assert_eq!(saved.last(), Some(&done));
    let not_wired = Update::FormNote {
        form: new,
        opening: 7,
        text: "not wired yet".to_string(),
        tone: Tone::Warn,
    };
    assert_eq!(noted.last(), Some(&not_wired));
    assert!(id.starts_with("skill-"), "{id}");
    let skills = query_collection_sorted(&access, SKILL_SPEC, "did:key:z").await;
    let drafts = skills
        .unwrap()
        .into_iter()
        .find(|skill| skill["skill_id"] == *id);
    let drafts = drafts.expect("the skill saved under the id its form showed");
    assert_eq!(
        (&drafts["name"], &drafts["enabled"]),
        (&json!("drafts"), &json!(false))
    );
    node.shutdown().await;
}

#[tokio::test]
async fn a_schedule_runs_now_through_its_trigger() {
    let keys = tempfile::tempdir().unwrap();
    let key = keys.path().join("node.key");
    let identity = gents::store::identity::KeyIdentity::load_or_create(&key, None).unwrap();
    let did = gents::store::identity::NodeIdentity::did(&identity).to_string();
    let mut config = node_settings_of(&did);
    config["schedules"].as_array_mut().unwrap().push(json!({
        "node_did": did,
        "schedule_id": "weekly",
        "cadence": { "kind": "cron", "expression": "0 9 * * 1", "timezone": "UTC" },
    }));
    let (node, graphql) = crate::shared::test_support::served_config(config).await;
    let (home, _) = home_serving(&graphql, Some(&key));
    let work = Work {
        endpoint: GraphqlEndpoint::anonymous(graphql),
        home: home.path().into(),
    };
    let access = work.access();
    let run = |schedule: &str| FormRef::Edit(Doc::Schedule, schedule.to_string());
    let act = gents_shell::source::FormAct::RunSchedule;

    let started = super::forms::act(
        &access,
        home.path(),
        &did,
        &run("daily"),
        &act,
        &[],
        "daily",
    )
    .await;
    let refused = super::forms::act(
        &access,
        home.path(),
        &did,
        &run("weekly"),
        &act,
        &[],
        "weekly",
    )
    .await;

    let (text, tone) = started;
    assert_eq!(tone, Tone::Accent, "{text}");
    let request = text.strip_prefix("Schedule started · ").expect(&text);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ caused_by_trigger_kind retry_key }} }}"#,
        gents::store::graphql::escape_graphql_string(request)
    );
    let written = gents::store::config_client::ConfigAccess::local(node.clone())
        .execute(&query)
        .await
        .unwrap();
    let fired = &written["data"]["AgentRequest"][0];
    let task = fired["retry_key"].as_str().unwrap_or_default();
    assert_eq!(fired["caused_by_trigger_kind"], "manual", "{written}");
    assert!(task.contains(":manual:triage:"), "{written}");
    let no_trigger = "Couldn’t run the schedule: schedule weekly has no enabled Trigger";
    assert_eq!(refused, (no_trigger.to_string(), Tone::Warn));
    node.shutdown().await;
}

#[tokio::test]
async fn a_new_trigger_on_a_preset_writes_schedule_task_and_trigger() {
    let (node, work, _home) = served_settings().await;
    let access = work.access();
    let new = FormRef::New(Doc::Trigger);

    let form = super::forms::open(&access, "owner", &new, "trig-abc").await;
    let form = form.unwrap();
    let sent_with = |pairs: &[(&str, &str)]| {
        let mut form = form.clone();
        for field in &mut form.fields {
            if let Some((_, value)) = pairs.iter().find(|(key, _)| *key == field.key) {
                field.value = value.to_string();
            }
        }
        as_opened(&form)
    };
    let keys = |pairs: &[(&str, &str)]| {
        let sent = sent_with(pairs).into_iter();
        sent.map(|(key, _)| key).collect::<Vec<_>>()
    };
    let prompt = "Summarise the inbox.\nThen post it.";
    let custom = sent_with(&[
        ("preset", "custom"),
        ("cron", "every day"),
        ("prompt", prompt),
    ]);
    let values = sent_with(&[("prompt", prompt)]);
    let save = |values| super::forms::save(&access, &work.home, "owner", &new, values, "trig-abc");
    let refused = save(&custom).await;
    let saved = save(&values).await;

    assert_eq!(
        keys(&[]),
        ["name", "kind", "schedule", "preset", "task", "prompt", "agent", "enable"]
    );
    assert_eq!(
        keys(&[("preset", "custom")])[..5],
        ["name", "kind", "schedule", "preset", "cron"]
    );
    assert_eq!(
        keys(&[("kind", "event")])[..5],
        ["name", "kind", "event", "collection", "event_kind"]
    );
    let zone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string());
    let owner = gents::documents::schedule_cron::validate_cron_schedule("every day", &zone, None);
    let owner = format!("{:#}", owner.unwrap_err());
    let Err(FormError {
        field: None,
        message: refused,
    }) = refused
    else {
        panic!("{refused:?}");
    };
    assert!(refused.contains(&owner), "{refused} does not carry {owner}");
    assert_eq!(saved, Ok("Saved".to_string()));
    let stored = |spec, id| load_one(&access, spec, "owner", id);
    let schedule = stored(SCHEDULE_SPEC, "sched-abc").await.unwrap();
    assert_eq!(schedule["display_name"], json!("Every day at 09:00"));
    assert_eq!(
        schedule["cadence"],
        json!({
            "kind": "cron",
            "expression": "0 9 * * *",
            "timezone": zone,
            "missed_run_policy": "latest_only",
        })
    );
    let trigger = stored(TRIGGER_SPEC, "trig-abc").await.unwrap();
    let shown = |document: &Value, keys: &[&str]| {
        let keys = keys.iter();
        keys.map(|key| document[*key].clone()).collect::<Vec<_>>()
    };
    assert_eq!(
        shown(
            &trigger,
            &[
                "display_name",
                "task_id",
                "source",
                "concurrency",
                "enabled"
            ]
        ),
        [
            json!("Summarise the inbox."),
            json!("task-abc"),
            json!({ "kind": "schedule", "schedule_id": "sched-abc" }),
            json!("serial"),
            json!(false),
        ]
    );
    let tasks = crate::commands::task::load_task_inventory(&access, Some("task-abc")).await;
    let tasks = tasks.unwrap().tasks;
    assert_eq!(
        tasks
            .iter()
            .map(|task| shown(task, &["display_name", "agent_id", "prompt_template"]))
            .collect::<Vec<_>>(),
        [[
            json!("Summarise the inbox."),
            json!("engineer"),
            json!(prompt)
        ]]
    );
    assert_eq!(
        menu_texts(&work, MenuId::Triggers).await,
        [
            " + New trigger",
            " ● Nightly   Every day at 02:00 · run Triage with Engineer",
            " ○ Summarise the inbox.   Every day at 09:00 · run Summarise the inbox. with Engineer",
        ]
    );
    node.shutdown().await;
}

#[tokio::test]
async fn a_schedule_trigger_refuses_a_concurrency_it_cannot_honour() {
    let (node, work, _home) = served_settings().await;
    let access = work.access();
    let nightly = FormRef::Edit(Doc::Trigger, "nightly".to_string());
    let form = super::forms::open(&access, "owner", &nightly, "nightly").await;
    let opened = as_opened(&form.unwrap());
    let save = |concurrency: &str| {
        let values = with(opened.clone(), "concurrency", concurrency);
        let (access, home, nightly) = (&access, &work.home, &nightly);
        async move { super::forms::save(access, home, "owner", nightly, &values, "nightly").await }
    };

    let queued = save("queued_serial").await;
    let serial = save("serial").await;

    let Err(FormError {
        field: None,
        message: queued,
    }) = queued
    else {
        panic!("{queued:?}");
    };
    assert!(
        queued.contains("queued_serial requires an explicit session_id_template"),
        "{queued}"
    );
    assert_eq!(serial, Ok("Saved".to_string()));
    let stored = load_one(&access, TRIGGER_SPEC, "owner", "nightly").await;
    assert_eq!(stored.unwrap()["concurrency"], json!("serial"));
    node.shutdown().await;
}

/// The shell's work on `home` with no runtime behind it: reads and writes that need none.
fn offline(home: &std::path::Path) -> Work {
    Work {
        endpoint: GraphqlEndpoint::anonymous("http://127.0.0.1:1/api/v0/graphql".to_string()),
        home: home.into(),
    }
}

#[tokio::test]
async fn a_remote_tool_without_a_host_shows_the_owners_reason() {
    let (node, work, home) = served_settings().await;
    let access = work.access();
    let new = FormRef::New(Doc::RemoteTool);
    let form = super::forms::open(&access, "owner", &new, "mcp-a").await;
    let no_host = with(as_opened(&form.unwrap()), "hostname", " ");
    let save = |values| super::forms::save(&access, home.path(), "owner", &new, values, "mcp-a");

    let refused = save(&no_host).await;
    let empty = menu_texts(&work, MenuId::RemoteTools).await;
    let saved = save(&with(no_host.clone(), "lan_ip", "10.0.0.5")).await;

    let Err(FormError {
        field: None,
        message,
    }) = refused
    else {
        panic!("{refused:?}");
    };
    let reason = "service mcp-a requires hostname, tailscale_ip, or lan_ip";
    assert!(message.contains(reason), "{message}");
    assert_eq!(
        empty,
        [
            " + New remote tools",
            " No remote tools.",
            " Add an MCP connection, then select its tools in a Tools document."
        ]
    );
    assert_eq!(saved, Ok("Saved".to_string()));
    let services = query_collection_sorted(&access, MCP_SPEC, "owner")
        .await
        .unwrap();
    let service = &services[0];
    assert_eq!(
        (
            &service["service_id"],
            &service["hostname"],
            &service["lan_ip"]
        ),
        (&json!("mcp-a"), &Value::Null, &json!("10.0.0.5"))
    );
    assert_eq!(
        (&service["mcp_port"], &service["enabled"]),
        (&json!(3333), &json!(false))
    );
    node.shutdown().await;
}

#[tokio::test]
async fn test_connection_uses_the_forms_values() {
    let home = tempfile::tempdir().unwrap();
    let access = offline(home.path()).access();
    let new = FormRef::New(Doc::RemoteTool);
    let form = super::forms::open(&access, "owner", &new, "mcp-a").await;
    let opened = as_opened(&form.unwrap());
    let test = gents_shell::source::FormAct::TestConnection;
    let tested =
        |values| super::forms::act(&access, home.path(), "owner", &new, &test, values, "mcp-a");
    let (no_host, far) = (
        with(opened.clone(), "hostname", ""),
        with(opened, "mcp_port", "70000"),
    );

    let no_host = tested(&no_host).await;
    let far = tested(&far).await;

    let failed = |reason: &str| (format!("Test failed: {reason}"), Tone::Warn);
    assert_eq!(
        no_host,
        failed("hostname, tailscale_ip, or lan_ip is required")
    );
    assert_eq!(far, failed("MCP port must be 65535 or less"));
}

#[tokio::test]
async fn a_node_save_keeps_what_the_form_does_not_show() {
    let mut config = node_settings();
    config["node"] = json!({
        "node_did": "owner",
        "display_name": "Studio",
        "avatar": "7",
        "default_agent_id": "engineer",
        "created_at": "2026-01-01T00:00:00Z",
        "created_by": "did:key:zMaker",
        "tags": ["gents:pack:home", "desk"],
    });
    let (node, work, home) = served(config).await;
    let access = work.access();
    let stored = || async {
        let nodes = query_collection(&access, NODE_SPEC, "owner", None).await;
        nodes.unwrap().remove(0)
    };
    let before = stored().await;

    let form = super::forms::open(&access, "owner", &FormRef::Node, "").await;
    let form = form.unwrap();
    let values = with(as_opened(&form), "display_name", " Workshop ");
    let values = with(values, "tags", "desk, bench");
    let saved =
        super::forms::save(&access, home.path(), "owner", &FormRef::Node, &values, "").await;

    assert_eq!(form.note, ["Studio"]);
    assert_eq!(saved, Ok("Saved".to_string()));
    let after = stored().await;
    assert_eq!(before["avatar"], json!("7"));
    assert_eq!(after["display_name"], json!("Workshop"));
    assert_eq!(after["tags"], json!(["gents:pack:home", "desk", "bench"]));
    for kept in [
        "avatar",
        "created_at",
        "created_by",
        "default_agent_id",
        "enabled",
    ] {
        assert_eq!(after[kept], before[kept], "{kept}");
    }
    node.shutdown().await;
}

#[tokio::test]
async fn changing_the_default_agent_enables_it() {
    let mut config = node_settings();
    config["agents"].as_array_mut().unwrap().push(json!({
        "node_did": "owner",
        "agent_id": "scout",
        "display_name": "Scout",
        "inference_profile_id": "profile",
        "enabled": false,
    }));
    let (node, work, home) = served(config).await;
    let access = work.access();
    let scout = || async {
        let state = gents_desktop_core::inference::load_inference_setup_state(&access, "owner");
        let agents = state.await.unwrap().agents;
        let scout = agents.into_iter().find(|agent| agent.agent_id == "scout");
        let scout = scout.unwrap();
        (scout.enabled, scout.is_default)
    };
    let before = scout().await;

    let form = super::forms::open(&access, "owner", &FormRef::Node, "").await;
    let form = form.unwrap();
    let values = with(as_opened(&form), "default_agent_id", "scout");
    let values = with(values, "display_name", "Studio");
    let saved =
        super::forms::save(&access, home.path(), "owner", &FormRef::Node, &values, "").await;

    let default = form
        .fields
        .iter()
        .find(|field| field.key == "default_agent_id");
    let gents_shell::source::FieldKind::Choice(agents) = &default.unwrap().kind else {
        panic!("{default:?}");
    };
    let scout_label = (
        "scout".to_string(),
        "Scout · disabled, enabled when saved as default".to_string(),
    );
    assert!(agents.contains(&scout_label), "{agents:?}");
    assert_eq!(saved, Ok("Saved".to_string()));
    assert_eq!(before, (false, false), "enabled, default");
    assert_eq!(scout().await, (true, true), "enabled, default");
    assert_eq!(
        menu_texts(&work, MenuId::Agents).await,
        [
            " ● Scout   default   profile · model",
            "   Engineer   profile · model"
        ]
    );
    node.shutdown().await;
}

#[tokio::test]
async fn folder_rows_toggle_and_remove() {
    use gents::pack::BindAccess;
    use gents::plugin::allowed;
    let (home, folders) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let [notes, repos] = ["notes", "repos"].map(|name| {
        let folder = folders.path().join(name);
        std::fs::create_dir(&folder).unwrap();
        folder.canonicalize().unwrap()
    });
    allowed::add(home.path(), &repos, BindAccess::ReadWrite).unwrap();
    allowed::add(home.path(), &notes, BindAccess::Read).unwrap();
    let work = offline(home.path());
    let read = || async {
        let off = Err("work is off".to_string());
        super::menus::read_menu(&off, home.path(), "owner", MenuId::AllowedFolders).await
    };
    let (work, at) = (&work, home.path());
    let act = |act: MenuAct| async move { super::menus::act(work, at, "owner", &act).await };
    let (notes_at, repos_at) = (notes.display().to_string(), repos.display().to_string());

    let listed = read().await.unwrap();
    let widened = act(MenuAct::SetFolderAccess {
        path: notes_at.clone(),
        access: "read_write".to_string(),
    })
    .await;
    let removed = act(MenuAct::RemoveFolder(repos_at.clone())).await;
    let left = read().await.unwrap();
    let missing = folders.path().join("missing").display().to_string();
    let refused = act(MenuAct::SetFolderAccess {
        path: missing.clone(),
        access: "read".to_string(),
    })
    .await;
    act(MenuAct::RemoveFolder(notes_at.clone())).await.unwrap();
    let none = read().await.unwrap();

    let folder = |path: &str, access: &str, other: &str| MenuRow {
        text: format!(" {path}   {access}"),
        tone: Tone::Plain,
        enter: None,
        d: Some(MenuAct::SetFolderAccess {
            path: path.to_string(),
            access: other.to_string(),
        }),
        more: vec![(
            "Remove".to_string(),
            MenuAct::RemoveFolder(path.to_string()),
        )],
    };
    let add = MenuRow {
        text: " + Add a folder".to_string(),
        tone: Tone::Accent,
        enter: Some(MenuAct::Open(FormRef::AddFolder)),
        d: None,
        more: Vec::new(),
    };
    assert_eq!(
        listed,
        [
            add.clone(),
            folder(&notes_at, "Read only", "read_write"),
            folder(&repos_at, "Read and write", "read"),
        ]
    );
    assert_eq!(widened, Ok(format!("{notes_at} read and write")));
    assert_eq!(removed, Ok(format!("{repos_at} removed")));
    assert_eq!(left, [add, folder(&notes_at, "Read and write", "read")]);
    let refused = refused.unwrap_err();
    assert!(
        refused.starts_with(&format!("{missing} does not exist or cannot be read")),
        "{refused}"
    );
    let texts: Vec<_> = none.into_iter().map(|row| row.text).collect();
    assert_eq!(
        texts,
        [
            " + Add a folder",
            " None added.",
            " The working folder is always readable. Other paths are asked about."
        ]
    );
    let dirs = [allowed::AllowedDir {
        path: notes.clone(),
        access: BindAccess::Read,
    }];
    let under = super::sections::folders(
        &dirs,
        Some(folders.path().canonicalize().unwrap().as_path()),
    );
    let at_home = super::sections::folders(&dirs, Some(notes.as_path()));
    assert_eq!(under[1].text, " ~/notes   Read only");
    assert_eq!(at_home[1].text, " ~   Read only");
    assert_eq!(under[1].d, folder(&notes_at, "", "read_write").d);
}

#[tokio::test]
async fn adding_a_folder_writes_the_homes_file() {
    use gents::pack::BindAccess;
    use gents::plugin::allowed;
    let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let access = offline(home.path()).access();
    let add = FormRef::AddFolder;
    let typed = |path: String| vec![("path".to_string(), path)];

    let form = super::forms::open(&access, "owner", &add, "")
        .await
        .unwrap();
    let saved = super::forms::save(
        &access,
        home.path(),
        "owner",
        &add,
        &typed(format!(" {} ", folder.path().display())),
        "",
    )
    .await;
    let missing = folder.path().join("missing");
    let refused = super::forms::save(
        &access,
        home.path(),
        "owner",
        &add,
        &typed(missing.display().to_string()),
        "",
    )
    .await;

    assert_eq!(form.title, "Add a folder");
    let path = super::forms::field(
        "path",
        "Folder path",
        gents_shell::source::FieldKind::Text,
        "",
    );
    let path = gents_shell::source::FormField {
        placeholder: "/path/to/folder".to_string(),
        ..path
    };
    assert_eq!(form.fields, [path]);
    assert_eq!(form.actions, []);
    let canonical = folder.path().canonicalize().unwrap();
    assert_eq!(saved, Ok(format!("{} read only", canonical.display())));
    let Err(FormError {
        field: None,
        message,
    }) = refused
    else {
        panic!("{refused:?}");
    };
    let reason = format!("{} does not exist or cannot be read", missing.display());
    assert!(message.starts_with(&reason), "{message}");
    let dirs = allowed::list(home.path()).unwrap();
    assert_eq!(
        dirs,
        [allowed::AllowedDir {
            path: canonical,
            access: BindAccess::Read,
        }]
    );
}

fn profile_of(
    display_name: Option<&str>,
    id: &str,
    model: &str,
) -> gents::documents::InferenceProfile {
    let mut profile = json!({
        "node_did": "owner", "profile_id": id, "backend_id": "backend", "model_name": model,
    });
    if let Some(name) = display_name {
        profile["display_name"] = json!(name);
    }
    serde_json::from_value(profile).unwrap()
}

#[test]
fn profile_labels_mirror_the_desktop() {
    use super::menus::profile_label;
    let named = profile_of(Some("Claude sonnet"), "p", "claude-sonnet");
    let holding = profile_of(Some("claude-sonnet-4-5"), "p", "claude-sonnet-4-5");
    assert_eq!(profile_label(&named), "Claude sonnet · claude-sonnet");
    assert_eq!(profile_label(&holding), "claude-sonnet-4-5");
    assert_eq!(profile_label(&profile_of(None, "p", "m")), "p · m");
}

fn two_profiles() -> Value {
    let mut config = node_settings();
    config["inference_profiles"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "node_did": "owner", "profile_id": "other", "backend_id": "backend",
            "model_name": "other-model", "display_name": "Other",
        }));
    config["agents"].as_array_mut().unwrap().push(json!({
        "node_did": "owner", "agent_id": "scout", "display_name": "Scout",
        "inference_profile_id": "profile",
    }));
    config
}

async fn profile_rows(work: &Work, agent: &str) -> Vec<gents_shell::source::MenuRow> {
    super::menus::read_profiles(&Ok(work.clone()), "owner", agent)
        .await
        .unwrap()
}

#[tokio::test]
async fn choosing_a_profile_moves_only_that_agent() {
    let (node, work, home) = served(two_profiles()).await;
    let rows =
        super::menus::read_menu(&Ok(work.clone()), &work.home, "owner", MenuId::Agents).await;
    let rows = rows.unwrap();
    let texts: Vec<_> = rows.iter().map(|row| row.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            " ● Engineer   default   profile · model",
            "   Scout   profile · model"
        ]
    );
    let choose = |agent: &str, name: &str| {
        vec![(
            "Inference profile…".to_string(),
            MenuAct::ChooseProfile {
                agent: agent.into(),
                name: name.into(),
            },
        )]
    };
    assert_eq!(rows[0].more, choose("engineer", "Engineer"));
    assert_eq!(rows[1].more, choose("scout", "Scout"));
    let engineer = profile_rows(&work, "engineer").await;
    assert_eq!(
        shown_rows(&engineer),
        [
            ("   Other · other-model", Tone::Plain),
            (" ● profile · model", Tone::Accent)
        ]
    );
    let choose = profile_rows(&work, "scout").await.remove(0).enter.unwrap();

    let done = super::menus::act(&work, home.path(), "owner", &choose).await;

    assert_eq!(done, Ok("Saved".to_string()));
    let scout = profile_rows(&work, "scout").await;
    assert_eq!(
        shown_rows(&scout),
        [
            (" ● Other · other-model", Tone::Accent),
            ("   profile · model", Tone::Plain)
        ]
    );
    assert_eq!(profile_rows(&work, "engineer").await, engineer);
    node.shutdown().await;
}

#[test]
fn the_key_set_survives_a_restart() {
    use super::prefs::{read_keys, save_keys};
    let home = tempfile::tempdir().unwrap();
    assert_eq!(read_keys(home.path()), KeySet::Vim);
    save_keys(home.path(), KeySet::Emacs).unwrap();
    assert_eq!(read_keys(home.path()), KeySet::Emacs);
    std::fs::write(home.path().join("shell.json"), "garbage").unwrap();
    assert_eq!(read_keys(home.path()), KeySet::Vim);
}

#[tokio::test]
async fn a_failed_key_save_says_so() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let missing = tempfile::tempdir().unwrap().path().join("gone");
    let shell = open_shell_on(
        &fake,
        None,
        None,
        ANSWER_WAIT,
        Err("off".into()),
        missing.clone(),
    );
    shell.opened().await;

    shell.intend(Intent::SaveKeys(KeySet::Emacs));

    let told = shell
        .until(|update| matches!(update, Update::Notice(n) if n.starts_with("could not save")))
        .await;
    let Some(Update::Notice(notice)) = told.last() else {
        panic!("{told:?}");
    };
    assert!(
        notice.starts_with("could not save the key set: "),
        "{notice}"
    );
}

#[tokio::test]
async fn choosing_a_profile_rereads_the_agents_not_the_profiles() {
    let fake = FakeRuntime::start(agents_a_and_b(), |_, method, _| quiet(method)).await;
    let mut config = node_settings_of("did:key:z");
    config["inference_profiles"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "node_did": "did:key:z", "profile_id": "other", "backend_id": "backend",
            "model_name": "other-model", "display_name": "Other",
        }));
    let (node, work, home) = served(config).await;
    let shell = open_shell_on(
        &fake,
        None,
        None,
        ANSWER_WAIT,
        Ok(work.clone()),
        home.path().into(),
    );
    shell.opened().await;
    let choose = MenuAct::SetAgentProfile {
        agent: "engineer".into(),
        name: "Engineer".into(),
        profile: "other".into(),
        label: "Other · other-model".into(),
    };

    shell.intend(Intent::Act {
        menu: MenuId::Profiles,
        act: choose,
    });

    let told = shell.until(is_menu).await;
    let Some(Update::Menu { menu, rows }) = told.last() else {
        panic!("{told:?}");
    };
    assert_eq!(*menu, MenuId::Agents);
    assert!(rows.as_ref().unwrap()[0].text.contains("Other"), "{rows:?}");
    node.shutdown().await;
}

/// A registry search's answer.
fn found(packs: Value, has_more: bool) -> Value {
    json!({ "packs": packs, "has_more": has_more })
}

#[test]
fn pack_rows_mirror_the_desktop() {
    use super::packs::{adds, pack_rows};
    use Tone::{Label, Plain, Warn};
    let git = || vec!["gents:pack:git".to_string()];
    let adds = adds(&[vec![git(), git()], vec![git(), vec!["other".to_string()]]]);
    let installed = json!({ "packs": [
        { "pack": "acme/git", "installed": "1.0.0", "latest": "1.1.0", "outdated": true, "error": null },
        { "pack": "acme/ocr", "installed": "0.2.0", "latest": null, "outdated": null, "error": "offline" },
    ]});
    let slots = json!({
        "plugins": [
            { "plugin": "acme/ocr", "slot": "vision", "profile": "fast" },
            { "plugin": "acme/git", "slot": "summary", "profile": null },
        ],
        "profiles": [
            { "profile_id": "fast", "display_name": "", "model_name": "gpt-fast", "usable": true },
        ],
    });
    let registry = found(
        json!([
            { "namespace": "acme", "name": "git", "description": "Git tools" },
            { "namespace": "acme", "name": "web", "description": " " },
        ]),
        false,
    );

    let (offline, _) = pack_rows(
        Ok(&installed),
        Ok(&slots),
        None,
        Err("offline"),
        "",
        1,
        &adds,
    );
    let (signed_in, _) = pack_rows(
        Ok(&installed),
        Ok(&Value::Null),
        Some("ada"),
        Ok(&registry),
        "acme",
        1,
        &adds,
    );

    assert_eq!(
        shown_rows(&offline),
        [
            (" Installed", Label),
            (" acme/git   1.0.0, 1.1.0 available", Plain),
            ("   Adds 2 agents · 1 skill", Label),
            (" acme/ocr   0.2.0 · couldn't check for updates", Plain),
            (" Model calls", Label),
            (" acme/ocr   gpt-fast", Plain),
            (" acme/git   Not set", Plain),
            (" Registry", Label),
            (" Search   Packs and plugins", Plain),
            ("   offline", Warn),
            (" Account", Label),
            (" Sign in   A token from your registry dashboard", Plain),
        ]
    );
    assert_eq!(offline[8].enter, Some(MenuAct::Open(FormRef::PackSearch)));
    assert_eq!(
        shown_rows(&signed_in)[4..],
        [
            (" Registry", Label),
            (" Search   acme", Plain),
            (" acme/git   Git tools · Installed", Label),
            (" acme/web", Plain),
            (" Account", Label),
            (" ada   Signed in to the registry", Plain),
        ]
    );
}

#[test]
fn pack_rows_show_the_empty_and_unread_states() {
    use super::packs::pack_rows;
    use Tone::{Label, Plain, Warn};
    let none = Default::default();
    let empty = found(json!([]), false);

    let (unread, _) = pack_rows(
        Err("denied"),
        Ok(&Value::Null),
        None,
        Ok(&empty),
        "",
        1,
        &none,
    );
    let listed = json!({ "packs": [] });
    let (nothing, _) = pack_rows(
        Ok(&listed),
        Ok(&Value::Null),
        None,
        Ok(&empty),
        "zz",
        1,
        &none,
    );

    assert_eq!(
        shown_rows(&unread)[..6],
        [
            (" Installed", Label),
            (" Couldn't read the installed packs", Warn),
            ("   denied", Warn),
            (" Registry", Label),
            (" Search   Packs and plugins", Plain),
            (" The registry has no packs yet", Label),
        ]
    );
    assert_eq!(shown_rows(&nothing)[1], (" No packs installed", Label));
    assert_eq!(shown_rows(&nothing)[4], (" No packs match", Label));
}

/// A failed model-calls read drops its group and says why, as the desktop's toast does
/// (`PacksPanel.tsx:72-75`).
#[test]
fn pack_rows_tell_a_failed_model_calls_read() {
    use super::packs::pack_rows;
    let (none, listed, empty) = (
        Default::default(),
        json!({ "packs": [] }),
        found(json!([]), false),
    );
    let slots = json!({
        "plugins": [{ "plugin": "acme/ocr", "slot": "vision", "profile": null }],
        "profiles": [],
    });
    let shows_group = |rows: &[MenuRow]| rows.iter().any(|row| row.text == " Model calls");

    let (read, quiet) = pack_rows(Ok(&listed), Ok(&slots), None, Ok(&empty), "", 1, &none);
    let unread = Err("reading the plugin store failed: locked");
    let (failed, told) = pack_rows(Ok(&listed), unread, None, Ok(&empty), "", 1, &none);

    assert!(shows_group(&read) && quiet.is_none(), "{read:?} {quiet:?}");
    assert!(!shows_group(&failed), "{failed:?}");
    let told = told.as_deref();
    assert_eq!(told, Some("reading the plugin store failed: locked"));
}

#[test]
fn pack_rows_page_the_registry() {
    use super::packs::pack_rows;
    use gents_shell::source::PackAct;
    let (none, listed) = (Default::default(), json!({ "packs": [] }));
    let paging = |page, has_more| {
        let registry = found(json!([{ "namespace": "acme", "name": "git" }]), has_more);
        let (rows, _) = pack_rows(
            Ok(&listed),
            Ok(&Value::Null),
            None,
            Ok(&registry),
            "",
            page,
            &none,
        );
        let after = rows
            .into_iter()
            .skip_while(|row| row.text != " acme/git")
            .skip(1);
        let paging = after.take_while(|row| row.text != " Account");
        paging.map(|row| (row.text, row.enter)).collect::<Vec<_>>()
    };
    let to = |page| Some(MenuAct::Pack(PackAct::Page(page)));
    let shown = |text: &str, enter| (text.to_string(), enter);

    assert_eq!(
        paging(1, true),
        [shown(" Page 1", None), shown(" Next", to(2))]
    );
    assert_eq!(
        paging(2, false),
        [shown(" Page 2", None), shown(" Previous", to(1))]
    );
    assert_eq!(
        paging(2, true),
        [
            shown(" Page 2", None),
            shown(" Previous", to(1)),
            shown(" Next", to(3))
        ]
    );
    assert_eq!(paging(1, false), []);
}

/// With no runtime answering, a pack call opens and claims the home's store
/// (`resolve_config_access`), so Packs answers the shell's reason and makes none.
#[tokio::test]
async fn packs_wait_for_the_runtime() {
    let keys = tempfile::tempdir().unwrap();
    let key = keys.path().join("principal.key");
    let (home, did) = home_serving("http://127.0.0.1:1/api/v0/graphql", Some(&key));
    let work = work::work_for(home.path());
    assert!(
        work.is_ok(),
        "the home's node signs: {:?}",
        work.as_ref().err()
    );
    let off: Result<Work, String> = Err("jobs and the mailbox are off: no runtime".to_string());
    let runtime = test_runtime_handle();

    let rows = super::packs::read(&work, home.path(), &did, &runtime, "", 1).await;
    let off_rows = super::packs::read(&off, home.path(), &did, &runtime, "", 1).await;

    let data = crate::home_state::default_data_dir(home.path());
    assert!(!data.exists(), "a pack call opened the store");
    assert!(
        !data.with_extension("lock").exists(),
        "a pack call claimed the store"
    );
    assert_eq!(rows, Err(source::closed(None)));
    assert_eq!(off_rows.err(), off.err());
}

fn install(edited: EditedChoice, grant: bool) -> MenuAct {
    install_bound(edited, grant, &[])
}

fn install_bound(edited: EditedChoice, grant: bool, slots: &[&str]) -> MenuAct {
    let pack = "acme/git".to_string();
    MenuAct::Pack(PackAct::Install {
        pack,
        edited,
        grant,
        slots: slots.iter().map(|slot| slot.to_string()).collect(),
    })
}

fn ungranted() -> PackChangeError {
    PackChangeError::Grant(GrantError::Ungranted {
        plugin: "acme/http".to_string(),
        asks: "HTTP to api.example.com".to_string(),
    })
}

fn edited_documents() -> PackChangeError {
    PackChangeError::Edited {
        coordinate: "acme/git".to_string(),
        edited: vec!["skill review".to_string()],
    }
}

fn choice(text: &str, act: MenuAct) -> (String, Tone, Option<MenuAct>) {
    (text.to_string(), Tone::Plain, Some(act))
}

#[test]
fn an_ungranted_install_asks_to_allow_it() {
    use EditedChoice::Refuse;
    let pack = "acme/git".to_string();

    let answer = super::packs::installed(pack, Refuse, false, Vec::new(), Err(ungranted()));

    let (title, rows) = question(answer);
    assert_eq!(title, "Install acme/git");
    assert_eq!(
        rows,
        [
            (format!(" {}", ungranted()), Tone::Warn, None),
            (" Allow it?".to_string(), Tone::Label, None),
            choice(" Allow", install(Refuse, true)),
        ]
    );
}

#[test]
fn an_edited_install_asks_replace_or_keep_and_keeps_the_grant() {
    use EditedChoice::{Keep, Overwrite, Refuse};
    let pack = "acme/git".to_string();

    let answer = super::packs::installed(pack, Refuse, true, Vec::new(), Err(edited_documents()));

    let (title, rows) = question(answer);
    assert_eq!(title, "Install acme/git");
    assert_eq!(
        rows,
        [
            (format!(" {}", edited_documents()), Tone::Warn, None),
            choice(" Replace them", install(Overwrite, true)),
            choice(" Keep my edits", install(Keep, true)),
        ]
    );
}

#[test]
fn a_replaced_install_can_still_ask_for_authority() {
    use EditedChoice::Overwrite;
    let pack = "acme/git".to_string();

    let answer = super::packs::installed(pack, Overwrite, false, Vec::new(), Err(ungranted()));

    let (_, rows) = question(answer);
    assert_eq!(
        rows.last(),
        Some(&choice(" Allow", install(Overwrite, true)))
    );
}

#[test]
fn an_install_that_cannot_pick_a_model_asks_which_and_keeps_the_rest() {
    use gents::pack::SlotBindingError;
    use EditedChoice::Keep;
    let profile = |id: &str, name: &str, model: &str| {
        let profile = json!({
            "profile_id": id, "display_name": name, "model_name": model,
            "backend_id": "b", "usable": true,
        });
        serde_json::from_value(profile).unwrap()
    };
    let unbound = PackChangeError::Slots(SlotBindingError::Unbound {
        pack: "acme/git".to_string(),
        slots: vec!["web_researcher".to_string(), "summarizer".to_string()],
        profiles: vec![
            profile("profile-grok", "Grok", "grok-4.7"),
            profile("profile-local", "", "qwen3-coder"),
        ],
    });
    let chosen = vec!["reader=profile-grok".to_string()];
    let pack = "acme/git".to_string();

    let answer = super::packs::installed(pack, Keep, true, chosen, Err(unbound));

    let (title, rows) = question(answer);
    assert_eq!(title, "Install acme/git");
    let pick = |profile| {
        let slots = ["reader=profile-grok", profile];
        install_bound(Keep, true, &slots)
    };
    assert_eq!(
        rows,
        [
            (
                " Which model should web_researcher use?".to_string(),
                Tone::Label,
                None
            ),
            choice(" Grok", pick("web_researcher=profile-grok")),
            choice(" qwen3-coder", pick("web_researcher=profile-local")),
        ]
    );
}

#[test]
fn an_install_says_what_it_did_or_why_not() {
    use EditedChoice::Refuse;
    let pack = || "acme/git".to_string();
    let failed = PackChangeError::Other(anyhow::anyhow!("offline").context("fetching acme/git"));

    let done = super::packs::installed(pack(), Refuse, false, Vec::new(), Ok(json!({})));
    let refused = super::packs::installed(pack(), Refuse, false, Vec::new(), Err(failed));

    assert_eq!(done, Update::Notice("Installed acme/git".to_string()));
    let why = "fetching acme/git: offline".to_string();
    assert_eq!(refused, Update::Notice(why));
}

#[test]
fn an_update_cannot_grant_so_it_says_why() {
    use EditedChoice::{Keep, Overwrite};
    let pack = || "acme/git".to_string();
    let update = |edited| {
        let pack = pack();
        MenuAct::Pack(PackAct::Update { pack, edited })
    };

    let ungranted_update = super::packs::updated(pack(), Err(ungranted()));
    let edited = super::packs::updated(pack(), Err(edited_documents()));
    let done = super::packs::updated(pack(), Ok(json!({})));

    assert_eq!(ungranted_update, Update::Notice(ungranted().to_string()));
    let (title, rows) = question(edited);
    assert_eq!(title, "Update acme/git");
    assert_eq!(
        rows[1..],
        [
            choice(" Replace them", update(Overwrite)),
            choice(" Keep my edits", update(Keep)),
        ]
    );
    assert_eq!(done, Update::Notice("Updated acme/git".to_string()));
}

/// Install is enter on a registry pack not installed; update, when one is available, and remove
/// are an installed pack's further acts.
#[test]
fn pack_rows_install_update_and_remove() {
    use super::packs::pack_rows;
    use EditedChoice::Refuse;
    let installed = json!({ "packs": [
        { "pack": "acme/git", "installed": "1.0.0", "latest": "1.1.0", "outdated": true, "error": null },
        { "pack": "acme/ocr", "installed": "0.2.0", "latest": "0.2.0", "outdated": false, "error": null },
    ]});
    let registry = found(
        json!([
            { "namespace": "acme", "name": "git" },
            { "namespace": "acme", "name": "web" },
        ]),
        false,
    );
    let none = Default::default();

    let (rows, _) = pack_rows(
        Ok(&installed),
        Ok(&Value::Null),
        None,
        Ok(&registry),
        "",
        1,
        &none,
    );

    let named = |text: &str| rows.iter().find(|row| row.text == text).expect(text);
    let remove = |pack: &str| {
        let pack = pack.to_string();
        let remove = PackAct::Remove {
            pack,
            confirmed: false,
        };
        ("Remove".to_string(), MenuAct::Pack(remove))
    };
    let update = PackAct::Update {
        pack: "acme/git".to_string(),
        edited: Refuse,
    };
    let update = ("Update".to_string(), MenuAct::Pack(update));
    let git = named(" acme/git   1.0.0, 1.1.0 available");
    assert_eq!(git.more, [update, remove("acme/git")]);
    assert_eq!(named(" acme/ocr   0.2.0").more, [remove("acme/ocr")]);
    let web = PackAct::Install {
        pack: "acme/web".to_string(),
        edited: Refuse,
        grant: false,
        slots: Vec::new(),
    };
    assert_eq!(named(" acme/web").enter, Some(MenuAct::Pack(web)));
    assert_eq!(named(" acme/git · Installed").enter, None);
}
