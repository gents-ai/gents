use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use gents_codex_protocol as codex;
use gents_shell::app::{App, ChatLine};
use gents_shell::keys::KeySet;
use gents_shell::sample::{Opens, RunState, Running, Tone};
use gents_shell::source::{Connection, Input, Intent, Session, Update};
use rapidhash::RapidHashMap;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use super::client::{Client, ClientError, ClientEvent, ANSWER_WAIT};
use super::source;
use crate::commands::codex::tests::identity;
use crate::commands::codex_shim::RuntimeIdentity;
use crate::commands::eval::testing::test_runtime_handle;

const WAIT: Duration = Duration::from_secs(10);

/// `None` leaves the request unanswered.
type Answer = Option<Result<Value, (i64, String)>>;
type Script = Box<dyn Fn(&str, &str, &Value) -> Answer + Send + Sync>;

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
}

/// `/runtime` answers `identity`, or fails without one. `/agents/{id}` serves that agent and
/// `/` the bound one, whose calls it records under the agent `/`. Each answers `initialize`
/// itself and every other request from `script`, given the agent, the method and the params.
pub(super) struct FakeRuntime {
    shared: Arc<Shared>,
    authority: String,
}

impl FakeRuntime {
    pub(super) async fn start<F>(identity: Option<RuntimeIdentity>, script: F) -> FakeRuntime
    where
        F: Fn(&str, &str, &Value) -> Answer + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        let shared = Arc::new(Shared {
            identity,
            script: Box::new(script),
            calls: watch::channel(Vec::new()).0,
            sockets: Mutex::default(),
        });
        let app = Router::new()
            .route("/runtime", get(runtime))
            .route("/", get(root))
            .route("/agents/{id}", get(routed))
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

fn agents_a_and_b() -> Option<RuntimeIdentity> {
    Some(identity(Some(&["a", "b"])))
}

async fn connect(
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

fn turn(id: &str, status: &str, items: Value) -> Value {
    json!({ "id": id, "items": items, "status": status, "error": null })
}

/// What `thread/start` and `thread/resume` answer.
fn thread_answer(thread: Value) -> Value {
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
fn quiet(method: &str) -> Answer {
    match method {
        "thread/list" => page(&[], None),
        _ => None,
    }
}

fn user_message(id: &str, text: &str) -> Value {
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
    }
}

fn blank() -> ChatLine {
    ChatLine {
        text: String::new(),
        tone: Tone::Plain,
        item: None,
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
    let runtime = test_runtime_handle();
    let (intents, intended) = mpsc::unbounded_channel();
    let (sender, updates) = std::sync::mpsc::channel();
    runtime.spawn(source::run(
        fake.remote(),
        agent.map(str::to_string),
        prompt.map(str::to_string),
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

    /// Reads `Agents`, `Ready` and the first session list.
    async fn ready(&self) -> Vec<Session> {
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
        .apply_through(&mut app, |update| matches!(update, Update::Sessions(_)))
        .await;
    fake.push(
        "a",
        &item_note("item/completed", "s1", agent_message("r1", "hi")),
    );

    assert_eq!(applied.len(), 3, "{applied:?}");
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
    assert_eq!(shell.next().await, opened);
    let running = Update::Turn {
        session: "s1".to_string(),
        turn: "t2".to_string(),
    };
    assert_eq!(shell.next().await, running);
    assert_eq!(shell.next().await, rows("s1", Vec::new()));
    let item = Update::Item {
        session: "s1".to_string(),
        line: line("still going", Tone::Reply, "r2"),
    };
    assert_eq!(shell.next().await, item);
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

fn resumes_of(fake: &FakeRuntime, thread: &str) -> usize {
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
    assert_eq!(shell.next().await, opened);
    let turn = Update::Turn {
        session: "c1".to_string(),
        turn: "t1".to_string(),
    };
    assert_eq!(shell.next().await, turn);
    assert_eq!(shell.next().await, rows("c1", Vec::new()));
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
    assert_eq!(shell.next().await, item);
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
    let refused = Update::Notice("turn/start: the session is busy".to_string());
    assert_eq!(shell.next().await, refused);

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
    assert_eq!(shell.next().await, Update::Notice(expired("turn/start")));
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
