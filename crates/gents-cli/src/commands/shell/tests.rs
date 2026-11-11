use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use gents_pack::pack::PackChangeError;
use gents_pack::plugin::authority::GrantError;
use gents_protocol::output::MessageRole;
use gents_protocol::row::AgentRequestRow;
use gents_shell::app::{App, ChatLine};
use gents_shell::keys::KeySet;
use gents_shell::sample::{MenuId, Opens, RunState, Running, Tone, View};
use gents_shell::source::{
    Answer as PromptAnswer, BackgroundCall, Change, ChangeKind, Connection, Doc, EditedChoice,
    FormError, FormRef, FormValues, Input, Intent, Job, JobEnd, MenuAct, MenuRow, PackAct, Prompt,
    PromptKind, Session, Skill, Update,
};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_store::graphql::escape_graphql_string;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use super::source::{self, Opening};
use super::testing::{question, shown_rows, text_message, At};
use super::work::{self, Work};
use crate::commands::config::crud::{
    load_one, query_collection, query_collection_sorted, MCP_SPEC, NODE_SPEC, SCHEDULE_SPEC,
    SKILL_SPEC, TRIGGER_SPEC,
};
use crate::commands::eval::testing::test_runtime_handle;

const WAIT: Duration = Duration::from_secs(10);
/// Long enough for a read that would come, short enough to keep a test quick.
const SHORT_WAIT: Duration = Duration::from_secs(1);

/// The fixture runtime the nodes here run as.
pub(super) const OWNER: &str = crate::caused_sessions::fixtures::FIXTURE_DID;

/// The DID a fixture's Agent named `name` takes.
pub(super) fn did(name: &str) -> String {
    crate::shared::test_support::fixture_agent_named(name).agent_did
}

pub(super) async fn within<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::time::timeout(WAIT, future)
        .await
        .expect("the shell should have answered by now")
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

/// The User every test shell signs as; its key lives as long as the process.
fn test_user() -> &'static gents_store::identity::KeyIdentity {
    static USER: std::sync::LazyLock<(tempfile::TempDir, gents_store::identity::KeyIdentity)> =
        std::sync::LazyLock::new(|| {
            let keys = tempfile::tempdir().unwrap();
            let user = gents_store::identity::KeyIdentity::load_or_create(
                keys.path().join("user.key"),
                None,
            );
            (keys, user.unwrap())
        });
    &USER.1
}

/// The test User's signed endpoint on `graphql`, as `work_for`'s endpoint signs.
fn user_endpoint(graphql: &str) -> GraphqlEndpoint {
    crate::shared::test_support::user_endpoint(graphql, test_user())
}

/// The test User's access for writing fixtures: the same signer, without the signed-create
/// rule that refuses an ordinary mutation.
fn seeding_access(graphql: &str) -> ConfigAccess {
    use gents_store::identity::NodeIdentity as _;
    let user = test_user();
    let endpoint = GraphqlEndpoint::as_principal(graphql, user.did());
    let capability = user.client_signing_capability().unwrap();
    ConfigAccess::Graphql(endpoint.with_client_signing_capability(capability).unwrap())
}

/// `config`'s inference backend and profile, under which each of `agents` runs.
fn chat_config(agents: &[&str]) -> Value {
    let agents: Vec<_> = agents
        .iter()
        .map(|name| {
            let binding = crate::shared::test_support::fixture_agent_named(name);
            json!({
                "node_did": OWNER, "agent_did": binding.agent_did,
                "creation_nonce": binding.creation_nonce, "display_name": name,
                "inference_profile_id": "profile",
            })
        })
        .collect();
    json!({
        "node": { "node_did": OWNER, "default_agent_did": did("a") },
        "inference_backends": [{
            "node_did": OWNER, "backend_id": "backend", "name": "Backend",
            "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
            "auth": { "kind": "unauthenticated" },
        }],
        "inference_profiles": [{
            "node_did": OWNER, "profile_id": "profile", "backend_id": "backend",
            "model_name": "model",
        }],
        "agents": agents,
    })
}

/// Publishes the runtime's readiness, with each of `agents` ready and the first its default,
/// as the runtime publishes it: under its Node, which shares it with the Node's household.
async fn publish_ready(access: &ConfigAccess, agents: &[&str]) {
    use gents_protocol::node_readiness::{
        AgentReadinessEntry, AgentReadinessState, NodeReadinessProcessState, NodeReadinessSnapshot,
        NODE_READINESS_FORMAT_VERSION,
    };
    let mut entries: Vec<_> = agents
        .iter()
        .map(|name| AgentReadinessEntry {
            agent_did: did(name),
            state: AgentReadinessState::Ready,
            reason: None,
        })
        .collect();
    entries.sort_by(|left, right| left.agent_did.cmp(&right.agent_did));
    let snapshot = NodeReadinessSnapshot {
        format_version: NODE_READINESS_FORMAT_VERSION,
        process_state: NodeReadinessProcessState::Ready,
        active_generation: 1,
        router_generation: 1,
        default_agent_did: did(agents[0]),
        agents: entries,
    };
    let snapshot = escape_graphql_string(&serde_json::to_string(&snapshot).unwrap());
    let read = format!(r#"{{ Node(filter: {{ node_did: {{ _eq: "{OWNER}" }} }}) {{ _docID }} }}"#);
    let nodes = access.execute(&read).await.unwrap();
    let parent = nodes["data"]["Node"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_string();
    let mutation = format!(
        r#"mutation {{ create_NodeReadiness(input: {{ node_did: "{OWNER}", node_doc_id: "{parent}", parent_linked: true, snapshot_json: "{snapshot}", updated_at: "2026-11-05T12:00:00Z" }}) {{ _docID }} }}"#
    );
    let created = access
        .transact_write("test.readiness", |txn| {
            let mutation = &mutation;
            Box::pin(async move { txn.execute(mutation).await })
        })
        .await
        .unwrap();
    let doc = gents_protocol::graphql::extract_mutation_doc_id(&created, "NodeReadiness").unwrap();
    let linked =
        access.add_resource_relationship("NodeReadiness", &doc, "parent", "nodes", &parent);
    linked.await.unwrap();
}

/// A node whose runtime is the fixture signer, served over its HTTP endpoint without node ACP.
/// The test User writes its config and, standing in for the runtime, its chat documents: the
/// User reads its own documents as it reads those the runtime shares with it.
pub(super) struct ChatNode {
    /// Kept serving while the test reads it.
    _node: Arc<defra_node::Node>,
    pub(super) graphql: String,
    written: std::sync::atomic::AtomicU32,
}

impl ChatNode {
    /// Agents `a` and `b`, both ready, `a` the default, and `researcher`, which only agents
    /// start sessions on.
    pub(super) async fn new() -> ChatNode {
        Self::with_agents(&["a", "b", "researcher"], &["a", "b"]).await
    }

    /// The `agents` of the config, of which `ready` are ready.
    async fn with_agents(agents: &[&str], ready: &[&str]) -> ChatNode {
        use gents_documents::apply_desired_state_plan;
        use gents_documents::DesiredStateApplyPlan;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let graphql = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let http = gents::defra_node::HttpConfig::with_listener(listener).unwrap();
        let builder = crate::shared::test_support::fixture_node_builder();
        let node = Arc::new(builder.with_http(http).build().await.unwrap());
        gents_store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        let config = serde_json::from_value(chat_config(agents)).unwrap();
        let plan = DesiredStateApplyPlan::from_pack_config(&config).unwrap();
        let user = ConfigAccess::Graphql(user_endpoint(&graphql));
        user.transact_write("test.chat_node.config", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
        if !ready.is_empty() {
            publish_ready(&seeding_access(&graphql), ready).await;
        }
        ChatNode {
            _node: node,
            graphql,
            written: Default::default(),
        }
    }

    pub(super) fn work(&self) -> Work {
        Work {
            endpoint: user_endpoint(&self.graphql),
            home: std::path::PathBuf::new(),
        }
    }

    /// The test User's access, which writes the runtime's documents here.
    pub(super) fn access(&self) -> ConfigAccess {
        seeding_access(&self.graphql)
    }

    /// Runs `request` as the test User, and returns its data.
    pub(super) async fn run(&self, request: &str) -> Value {
        let access = self.access();
        let answered = match request.trim_start().starts_with("mutation") {
            true => {
                let written = access.transact_write("test.chat_node", |txn| {
                    Box::pin(async move { txn.execute(request).await })
                });
                written.await
            }
            false => access.execute(request).await,
        };
        let answered = answered.unwrap_or_else(|error| panic!("{request}: {error:#}"));
        answered["data"].clone()
    }

    /// Creates the one document `mutation` with `variables` makes, and returns its id.
    async fn create(&self, mutation: &str, variables: Value, collection: &str) -> String {
        let answered = self
            .access()
            .transact_write("test.chat_node.create", |txn| {
                let (mutation, variables) = (mutation, &variables);
                Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
            })
            .await
            .unwrap();
        gents_protocol::graphql::extract_mutation_doc_id(&answered, collection).unwrap()
    }

    async fn segment(&self, segment: &gents_protocol::output::OutputSegment) -> String {
        use gents_protocol::output::rows::{
            output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        };
        let variables = output_segment_create_variables(segment).unwrap();
        let mutation = CREATE_AGENT_OUTPUT_SEGMENT_MUTATION;
        self.create(mutation, variables, "AgentOutputSegment").await
    }

    async fn header(&self, header: &gents_protocol::output::TranscriptMessage) -> String {
        use gents_protocol::output::rows::{
            transcript_message_create_variables, CREATE_AGENT_MESSAGE_MUTATION,
        };
        let variables = transcript_message_create_variables(header).unwrap();
        self.create(CREATE_AGENT_MESSAGE_MUTATION, variables, "AgentMessage")
            .await
    }

    /// A time after every document written before it, so the newest reads last.
    fn next_at(&self) -> String {
        let n = self
            .written
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-05T12:00:00Z").unwrap();
        let at = at + chrono::Duration::seconds(i64::from(n));
        at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// The runtime's session `id`, on `agent`.
    pub(super) async fn session(&self, id: &str, agent: &str) {
        let (id, agent) = (escape_graphql_string(id), did(agent));
        let at = self.next_at();
        self.run(&format!(
            r#"mutation {{ create_AgentSession(input: {{ session_id: "{id}", node_did: "{OWNER}", requester_did: "{OWNER}", agent_did: "{agent}", created_at: "{at}" }}) {{ _docID }} }}"#
        ))
        .await;
    }

    /// The runtime's request `id` in `session`, in `state`, on agent `a`.
    pub(super) async fn request(&self, session: &str, id: &str, state: &str) -> AgentRequestRow {
        self.request_with(session, id, state, r#"content: "work""#)
            .await
    }

    /// [`ChatNode::request`] with `fields`, its content among them.
    async fn request_with(
        &self,
        session: &str,
        id: &str,
        state: &str,
        fields: &str,
    ) -> AgentRequestRow {
        let at = self.next_at();
        let agent = did("a");
        self.run(&format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "{id}", purpose: "normal", node_did: "{OWNER}", requester_did: "{OWNER}", agent_did: "{agent}", session_id: "{session}", {fields}, lifecycle_state: "{state}", execution_generation: "g-{id}", execution_lease_secs: 300, execution_lease_expires_at: "2126-01-01T00:00:00Z", created_at: "{at}" }}) {{ _docID }} }}"#
        ))
        .await;
        self.request_row(id).await
    }

    pub(super) async fn request_row(&self, id: &str) -> AgentRequestRow {
        let filter = format!(r#"request_id: {{ _eq: "{}" }}"#, escape_graphql_string(id));
        let mut rows = self.requests(&filter).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        rows.remove(0)
    }

    /// The requests `filter` picks.
    async fn requests(&self, filter: &str) -> Vec<AgentRequestRow> {
        let fields = "_docID request_id node_did requester_did agent_did session_id content input lifecycle_state interrupt_requested_at";
        let rows = self
            .run(&format!(
                "{{ AgentRequest(filter: {{ {filter} }}) {{ {fields} }} }}"
            ))
            .await;
        serde_json::from_value(rows["AgentRequest"].clone()).unwrap()
    }

    /// Sets `fields` on the request, as its runtime's lifecycle does.
    pub(super) async fn update(&self, request: &AgentRequestRow, fields: &str) {
        let doc = escape_graphql_string(request.doc_id.as_deref().unwrap());
        self.run(&format!(
            r#"mutation {{ update_AgentRequest(docID: "{doc}", input: {{ {fields} }}) {{ _docID }} }}"#
        ))
        .await;
    }

    /// Where `request`'s output is written.
    pub(super) fn at(request: &AgentRequestRow) -> At {
        At {
            node: OWNER.to_string(),
            session: request.session_id.clone().unwrap(),
            requester: request.requester_did.clone(),
            request: request.doc_id.clone().unwrap(),
            generation: format!("g-{}", request.request_id),
        }
    }

    /// A published text message of `request` at `sequence`.
    pub(super) async fn text(
        &self,
        request: &AgentRequestRow,
        sequence: u32,
        role: MessageRole,
        text: &str,
    ) {
        let at = Self::at(request);
        let (segment, _) = text_message(&at, sequence, role, text, "");
        let close = self.segment(&segment).await;
        let (_, header) = text_message(&at, sequence, role, text, &close);
        self.header(&header).await;
    }

    /// The running turn's next flush of its reply, `ordinal` counting from 0.
    pub(super) async fn flush(&self, request: &AgentRequestRow, ordinal: u32, text: &str) {
        use gents_protocol::output::{SegmentRun, StreamDeclaration, StreamPayload};
        let at = Self::at(request);
        let declaration = (ordinal == 0).then_some(StreamDeclaration {
            block_index: 0,
            part_index: 0,
            payload: StreamPayload::Text,
        });
        let segment = gents_protocol::output::OutputSegment {
            ordinal: Some(ordinal),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: u32::try_from(text.len()).unwrap(),
                declaration,
            }],
            close: None,
            ..super::testing::closed(
                &at,
                super::testing::turn_source(0),
                super::testing::execution(&at),
                StreamPayload::Text,
                text,
            )
        };
        self.segment(&segment).await;
    }

    /// A call of `tool` with `arguments` at `sequence`, in `state`, delivered `result` at the
    /// next sequence. Returns the call's document.
    pub(super) async fn tool(
        &self,
        request: &AgentRequestRow,
        sequence: u32,
        tool: &str,
        arguments: Value,
        state: &str,
        result: Option<&str>,
    ) -> String {
        use super::testing::{closed, execution, header, presented, turn_source};
        use gents_protocol::output::{
            MessageBlock, MessagePublication, OutputSource, OutputWriter, StreamPayload,
            ToolResultPart,
        };
        let at = Self::at(request);
        let call = format!("call-{sequence}");
        let (doc, request_id) = (escape_graphql_string(&at.request), &request.request_id);
        let started = self.next_at();
        let requester = at.requester.as_deref().map(escape_graphql_string);
        let requester = requester.map_or("null".to_string(), |did| format!(r#""{did}""#));
        let mutation = format!(
            r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "{doc}:{call}", request_id: "{request_id}", request_doc_id: "{doc}", node_did: "{OWNER}", requester_did: {requester}, session_id: "{}", tool_call_id: "{call}", tool_name: "{tool}", message_sequence: {sequence}, lifecycle_state: "{state}", started_at: "{started}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&at.session),
        );
        let tool_doc = self.create(&mutation, json!({}), "AgentToolCall").await;
        let payload = StreamPayload::ToolArguments {
            id: call.clone(),
            call_id: None,
            name: tool.to_string(),
        };
        let source = turn_source(sequence);
        let arguments = arguments.to_string();
        let segment = closed(&at, source, execution(&at), payload, &arguments);
        let close = self.segment(&segment).await;
        let block = MessageBlock::ToolCall {
            tool_call_doc_id: tool_doc.clone(),
            id: call.clone(),
            call_id: None,
            name: tool.to_string(),
            arguments: presented(&close).output,
            signature: None,
            additional_params: None,
        };
        let publication = MessagePublication::RequestExecution {
            execution_generation: at.generation.clone(),
        };
        let key = format!("accepted:{}:{sequence}", at.request);
        let role = MessageRole::Assistant;
        self.header(&header(&at, &key, sequence, role, publication, vec![block]))
            .await;
        let Some(result) = result else {
            return tool_doc;
        };
        let source = OutputSource::ToolCall {
            tool_call_doc_id: tool_doc.clone(),
        };
        let writer = OutputWriter::ToolExecution {
            tool_call_doc_id: tool_doc.clone(),
        };
        let segment = closed(&at, source, writer, StreamPayload::ToolOutput, result);
        let close = self.segment(&segment).await;
        let block = MessageBlock::ToolResult {
            tool_call_doc_id: tool_doc.clone(),
            id: call.clone(),
            call_id: None,
            parts: vec![ToolResultPart::Text {
                text: presented(&close),
            }],
        };
        let publication = MessagePublication::ToolDelivery {
            tool_call_doc_id: tool_doc.clone(),
        };
        let key = format!("delivery:{}:{call}", at.request);
        let (next, role) = (sequence + 1, MessageRole::User);
        self.header(&header(&at, &key, next, role, publication, vec![block]))
            .await;
        tool_doc
    }

    /// `agent`'s session `child`, started by `request` of its parent as `agent_new` starts one:
    /// its opening request names the parent's, and its receipt is the runtime's.
    pub(super) async fn child(&self, request: &AgentRequestRow, child: &str, agent: &str) {
        use gents_protocol::session::{
            SessionCreationCause, SessionCreationReceipt, SessionCreationStarter,
            SessionProvenance, SessionReference,
        };
        use gents_store::identity::NodeIdentity as _;
        let parent = request.doc_id.clone().unwrap();
        // `agent_new` records the call that started it beside the request.
        let caused = format!(
            r#"caused_by_parent_request_doc_id: "{}", caused_by_parent_tool_call_doc_id: "fixture-call", content: "{child} job""#,
            escape_graphql_string(&parent)
        );
        let id = format!("{child}-r1");
        let opening = self.request_with(child, &id, "completed", &caused).await;
        let opened = opening.doc_id.clone().unwrap();
        let source = SessionReference {
            node_did: OWNER.to_string(),
            session_id: request.session_id.clone().unwrap(),
            requester_did: Some(OWNER.to_string()),
        };
        let mut session = crate::caused_sessions::fixtures::fixture_session(OWNER, child);
        session.agent_did = did(agent);
        session.created_at = self.next_at();
        let mut provenance = SessionProvenance {
            parent_request_doc_id: Some(parent.clone()),
            opening_request_doc_id: Some(opened.clone()),
            started_by: Some(source.clone()),
            ..Default::default()
        };
        session.provenance = Some(provenance.clone());
        let mut input = serde_json::to_value(&session).unwrap();
        input["started_by_node_did"] = source.node_did.clone().into();
        input["started_by_session_id"] = source.session_id.clone().into();
        let mutation = "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }";
        let doc = self
            .create(mutation, json!({ "input": input }), "AgentSession")
            .await;
        let mut receipt = SessionCreationReceipt {
            node_did: OWNER.to_string(),
            session_id: child.to_string(),
            session_doc_id: doc.clone(),
            requester_did: session.requester_did.clone(),
            agent_did: session.agent_did.clone(),
            opening_request_doc_id: opened,
            cause: SessionCreationCause::AgentNew,
            cause_binding: "fixture-call".to_string(),
            starter: Some(SessionCreationStarter {
                source,
                parent_request_doc_id: parent,
            }),
            signature: Vec::new(),
        };
        let signer = crate::caused_sessions::fixtures::fixture_signer();
        receipt.signature = signer.sign(&receipt.signing_payload()).await.unwrap();
        provenance.creation_receipt = Some(receipt);
        let input = json!({ "provenance": provenance });
        let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
        let doc = escape_graphql_string(&doc);
        self.run(&format!(
            r#"mutation {{ update_AgentSession(docID: "{doc}", input: {input}) {{ _docID }} }}"#
        ))
        .await;
    }
}

/// Answers a GraphQL query, given its text; `None` lets it reach the node.
type Script = Box<dyn Fn(&str) -> Option<Value> + Send + Sync>;
type Chunks = mpsc::UnboundedSender<Result<Bytes, std::io::Error>>;

struct Shared {
    script: Script,
    /// The node every request the script does not answer reaches.
    upstream: String,
    client: reqwest::Client,
    /// Every GraphQL document posted, subscriptions included, in order.
    queries: watch::Sender<Vec<String>>,
    /// The open subscriptions' documents and event streams.
    streams: Mutex<Vec<(String, Chunks)>>,
    /// While true, every query the script answers waits before it is answered.
    held: watch::Sender<bool>,
}

/// A node's GraphQL as the shell reaches it, with each subscription held here, so a test sends
/// its events, and each query the script answers answered here. Every other request reaches
/// the node unchanged, its signature with it.
pub(super) struct FakeGraphql {
    shared: Arc<Shared>,
    graphql: String,
    pub(super) node: ChatNode,
}

impl FakeGraphql {
    pub(super) async fn start<G>(node: ChatNode, script: G) -> FakeGraphql
    where
        G: Fn(&str) -> Option<Value> + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        let upstream = node.graphql.strip_suffix("/api/v0/graphql").unwrap();
        let shared = Arc::new(Shared {
            script: Box::new(script),
            upstream: upstream.to_string(),
            client: reqwest::Client::new(),
            queries: watch::channel(Vec::new()).0,
            streams: Mutex::default(),
            held: watch::channel(false).0,
        });
        let app = Router::new().fallback(serve).with_state(shared.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        let graphql = format!("http://{authority}/api/v0/graphql");
        FakeGraphql {
            shared,
            graphql,
            node,
        }
    }

    pub(super) fn work(&self) -> Work {
        Work {
            endpoint: user_endpoint(&self.graphql),
            home: std::path::PathBuf::new(),
        }
    }

    pub(super) fn queries(&self) -> Vec<String> {
        self.shared.queries.borrow().clone()
    }

    /// How many times `document` was subscribed.
    fn subscribed(&self, document: &str) -> usize {
        let queries = self.queries();
        queries.iter().filter(|query| *query == document).count()
    }

    /// Returns once `count` posted GraphQL documents contain `text`.
    async fn posted(&self, text: &str, count: usize) {
        let mut queries = self.shared.queries.subscribe();
        let text = without_whitespace(text);
        let found = |queries: &Vec<String>| {
            queries
                .iter()
                .filter(|query| without_whitespace(query).contains(&text))
                .count()
        };
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
        self.next_event(source::WATCHED, "AgentToolCall");
    }

    /// One `next` event on each open mailbox subscription, as an item's commit sends.
    fn mailbox_changed(&self) {
        self.next_event(source::MAILBOX_WATCHED, "MailboxItem");
    }

    /// One `next` event on the chat's subscription to `collection`.
    fn chat_changed(&self, collection: &str) {
        self.next_event(&source::chat_document(collection, OWNER), collection);
    }

    fn next_event(&self, document: &str, collection: &str) {
        let event = format!(
            "event: next\ndata: {{\"data\":{{\"{collection}\":[{{\"_docID\":\"bae-1\"}}]}}}}\n\n"
        );
        let streams = self.shared.streams.lock().unwrap();
        for (_, stream) in streams.iter().filter(|(query, _)| query == document) {
            stream.send(Ok(Bytes::from(event.clone()))).unwrap();
        }
    }

    /// Holds every scripted answer until `hold(false)`, as a slow node would.
    fn hold(&self, held: bool) {
        self.shared.held.send_replace(held);
    }

    /// Closes the subscriptions `ended` picks, as a node whose event bus stops does.
    fn end_streams<F>(&self, ended: F)
    where
        F: Fn(&str) -> bool,
    {
        let mut streams = self.shared.streams.lock().unwrap();
        streams.retain(|(query, _)| !ended(query));
    }

    /// Breaks the subscriptions `broken` picks mid-stream, as a node that died would.
    fn break_streams<F>(&self, broken: F)
    where
        F: Fn(&str) -> bool,
    {
        let streams = self.shared.streams.lock().unwrap();
        for (_, stream) in streams.iter().filter(|(query, _)| broken(query)) {
            let reset = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset by peer");
            let _ = stream.send(Err(reset));
        }
    }
}

fn is_chat(document: &str) -> bool {
    source::CHAT
        .iter()
        .any(|collection| document == source::chat_document(collection, OWNER))
}

fn is_work(document: &str) -> bool {
    document == source::WATCHED || document == source::MAILBOX_WATCHED
}

async fn serve(State(shared): State<Arc<Shared>>, request: axum::extract::Request) -> Response {
    use axum::extract::FromRequestParts as _;
    let (mut parts, body) = request.into_parts();
    // A client-signed write holds its signing conversation with the node.
    if parts.headers.contains_key(header::UPGRADE) {
        let upgrade = axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &());
        let upgrade = upgrade.await.unwrap();
        let upstream = shared.upstream.replacen("http://", "ws://", 1);
        return upgrade.on_upgrade(move |socket| {
            crate::shared::test_support::pipe_websocket(socket, upstream, parts)
        });
    }
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    if parts.uri.path() == "/api/v0/graphql" {
        let posted: Value = serde_json::from_slice(&body).unwrap_or_default();
        let query = posted_query(&posted);
        shared
            .queries
            .send_modify(|queries| queries.push(query.clone()));
        let accept = parts.headers.get(header::ACCEPT);
        if accept.is_some_and(|accept| accept == "text/event-stream") {
            let (chunks, sent) = mpsc::unbounded_channel();
            shared.streams.lock().unwrap().push((query, chunks));
            let events = futures_util::stream::unfold(sent, |mut sent| async move {
                sent.recv().await.map(|chunk| (chunk, sent))
            });
            let body = Body::from_stream(events);
            return ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response();
        }
        if let Some(answer) = (shared.script)(&query) {
            let mut held = shared.held.subscribe();
            let _ = held.wait_for(|held| !held).await;
            return Json(answer).into_response();
        }
    }
    let path = parts.uri.path_and_query().map_or("/", |path| path.as_str());
    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).unwrap();
    let mut outbound = shared
        .client
        .request(method, format!("{}{path}", shared.upstream));
    // The signed authorization binds the request's authority, so the client's Host goes along.
    for (name, value) in &parts.headers {
        if name != header::CONTENT_LENGTH {
            outbound = outbound.header(name.as_str(), value.as_bytes());
        }
    }
    let answer = outbound.body(body).send().await.unwrap();
    let status = axum::http::StatusCode::from_u16(answer.status().as_u16()).unwrap();
    let kind = answer.headers().get("content-type").cloned();
    let bytes = answer.bytes().await.unwrap();
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    if let Some(kind) = kind.and_then(|kind| header::HeaderValue::from_bytes(kind.as_bytes()).ok())
    {
        response.headers_mut().insert(header::CONTENT_TYPE, kind);
    }
    response
}

/// The source on the test runtime, fed and read as the shell would.
struct Shell {
    intents: mpsc::UnboundedSender<Intent>,
    updates: Arc<Mutex<std::sync::mpsc::Receiver<Input>>>,
}

/// What `gents` opens the shell with, beside its work.
#[derive(Default)]
struct Launch<'a> {
    agent: Option<&'a str>,
    prompt: Option<&'a str>,
    session: Option<&'a str>,
    cwd: Option<std::path::PathBuf>,
    tree: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
}

fn open_shell(work: Work) -> Shell {
    open_shell_with(Ok(work), Launch::default())
}

fn open_shell_with(work: Result<Work, String>, launch: Launch) -> Shell {
    let runtime = test_runtime_handle();
    let (intents, intended) = mpsc::unbounded_channel();
    let (sender, updates) = std::sync::mpsc::channel();
    let home = launch.home.unwrap_or_else(|| match &work {
        Ok(work) => work.home.clone(),
        Err(_) => std::path::PathBuf::new(),
    });
    let opening = Opening {
        node: OWNER.to_string(),
        agent: launch.agent.map(str::to_string),
        prompt: launch.prompt.map(str::to_string),
        session: launch.session.map(str::to_string),
        work,
        home,
        cwd: launch.cwd,
        tree: launch.tree,
    };
    runtime.spawn(source::run(opening, intended, sender, runtime.clone()));
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

    /// Whether nothing more is told within `SHORT_WAIT`.
    async fn quiet(&self) -> Option<Update> {
        let updates = self.updates.clone();
        let input = test_runtime_handle()
            .spawn_blocking(move || updates.lock().unwrap().recv_timeout(SHORT_WAIT))
            .await
            .unwrap();
        input.ok().map(|input| match input {
            Input::Runtime(update) => update,
            other => panic!("{other:?}"),
        })
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
        self.opened_as(&[&did("a"), &did("b")]).await
    }

    /// [`Shell::opened`], on a node whose ready agents are `ids`.
    async fn opened_as(&self, ids: &[&str]) -> Vec<Session> {
        let update = self.next().await;
        let Update::Agents(agents) = update else {
            panic!("{update:?}");
        };
        let shown: Vec<_> = agents.iter().map(|agent| agent.id.as_str()).collect();
        assert_eq!(shown, ids);
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

fn is_item(update: &Update) -> bool {
    matches!(update, Update::Item { .. })
}

fn is_rows_of(update: &Update, at: &str) -> bool {
    matches!(update, Update::Running { session, .. } if session == at)
}

fn is_opened(update: &Update, at: &str) -> bool {
    matches!(update, Update::Opened { session, .. } if session == at)
}

fn open(shell: &Shell, session: &str) {
    shell.intend(Intent::Open {
        session: session.to_string(),
        agent: did("a"),
    });
}

fn send(tab: u64, session: Option<&str>, agent: &str, text: &str) -> Intent {
    Intent::Send {
        tab,
        session: session.map(str::to_string),
        agent: agent.to_string(),
        text: text.to_string(),
    }
}

fn turn(session: &str, turn: &str) -> Update {
    Update::Turn {
        session: session.to_string(),
        turn: turn.to_string(),
    }
}

fn ended(session: &str, turn: &str) -> Update {
    Update::TurnEnded {
        session: session.to_string(),
        turn: turn.to_string(),
    }
}

fn item(session: &str, line: ChatLine) -> Update {
    Update::Item {
        session: session.to_string(),
        line,
    }
}

fn rows(session: &str, rows: Vec<Running>) -> Update {
    Update::Running {
        session: session.to_string(),
        rows,
    }
}

/// What the runtime sets on a request it claims and runs.
fn claimed(request: &AgentRequestRow) -> String {
    let generation = format!("g-{}", request.request_id);
    format!(
        r#"lifecycle_state: "processing", execution_generation: "{generation}", execution_lease_secs: 300, execution_lease_expires_at: "2126-01-01T00:00:00Z""#
    )
}

/// The request the shell sent in `session`.
async fn sent_in(node: &ChatNode, session: &str) -> AgentRequestRow {
    let filter = format!(
        r#"session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(session)
    );
    let mut rows = node.requests(&filter).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows.remove(0)
}

#[tokio::test]
async fn a_given_agent_becomes_the_first_tabs_agent() {
    let node = ChatNode::new().await;
    let agent = did("b");
    let opened = Launch {
        agent: Some(&agent),
        ..Default::default()
    };
    let shell = open_shell_with(Ok(node.work()), opened);
    let (mut app, intended) = connected_app();

    shell.apply_through(&mut app, is_begin).await;

    assert_eq!(app.tabs[0].agent_id, did("b"));
    assert_eq!(app.tabs[0].agent, "b", "named as setup names it");
    assert_eq!(app.notice, None);
    assert!(
        intended.try_recv().is_err(),
        "no prompt, so nothing is sent"
    );
}

#[tokio::test]
async fn an_unknown_agent_is_named_and_the_first_tab_keeps_the_default() {
    let node = ChatNode::new().await;
    let opened = Launch {
        agent: Some("zed"),
        ..Default::default()
    };
    let shell = open_shell_with(Ok(node.work()), opened);
    let (mut app, _intended) = connected_app();

    shell.apply_through(&mut app, is_begin).await;

    assert_eq!(app.tabs[0].agent_id, did("a"));
    assert_eq!(
        app.notice.as_deref(),
        Some("agent zed is not open in this shell; using a")
    );
}

/// Only the agents the runtime says are ready are offered, the node's default first.
#[tokio::test]
async fn only_the_ready_agents_are_listed_the_default_first() {
    let node = ChatNode::with_agents(&["c", "b", "a"], &["b", "a"]).await;
    let shell = open_shell(node.work());

    shell.opened_as(&[&did("a"), &did("b")]).await;
}

/// With no ready agent there is nothing to talk to, so the shell is lost before it opens.
#[tokio::test]
async fn a_runtime_with_no_ready_agent_is_lost() {
    let node = ChatNode::with_agents(&["a"], &[]).await;
    let shell = open_shell(node.work());

    let lost =
        "no runtime: check `gents status`, then reopen the shell (no agent is ready on this node)";
    let update = Update::Connection(Connection::Lost(lost.to_string()));
    assert_eq!(shell.next().await, update);
}

/// `Begin` is told straight after `Agents`, before the first sweep, so a send the user makes
/// while the sweep runs comes after it in the shell's one queue of updates and keys.
#[tokio::test]
async fn begin_follows_agents_before_the_first_session_sweep() {
    let node = ChatNode::new().await;
    let opened = Launch {
        prompt: Some("hello"),
        ..Default::default()
    };
    let shell = open_shell_with(Ok(node.work()), opened);
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

/// As `gents --continue s1` opens it: `Continue` takes `Begin`'s place, before `Ready`, so no
/// key the user presses while the session lists load can reach the starter tab first.
#[tokio::test]
async fn a_continued_session_is_told_where_begin_would_be() {
    let node = ChatNode::new().await;
    let agent = did("b");
    let opened = Launch {
        agent: Some(&agent),
        session: Some("s1"),
        ..Default::default()
    };
    let shell = open_shell_with(Ok(node.work()), opened);
    let (mut app, _intended) = connected_app();

    let applied = shell
        .apply_through(&mut app, |update| matches!(update, Update::Sessions(_)))
        .await;

    let continued = Update::Continue {
        session: "s1".to_string(),
        agent: Some(did("b")),
    };
    assert!(
        matches!(
            applied.as_slice(),
            [
                Update::Agents(_),
                found,
                Update::Connection(Connection::Ready),
                Update::Sessions(_)
            ] if *found == continued
        ),
        "{applied:?}"
    );
    assert_eq!(app.tabs[0].session.as_deref(), Some("s1"));
    assert_eq!(app.tabs[0].agent_id, did("b"));
}

#[tokio::test]
async fn a_given_prompt_is_sent_once_as_the_first_tabs_first_message() {
    let node = ChatNode::new().await;
    let agent = did("b");
    let opened = Launch {
        agent: Some(&agent),
        prompt: Some("hello"),
        ..Default::default()
    };
    let shell = open_shell_with(Ok(node.work()), opened);
    let (mut app, intended) = connected_app();

    let applied = shell.apply_through(&mut app, is_begin).await;
    assert!(matches!(applied[0], Update::Agents(_)), "{applied:?}");
    let sent = intended.try_recv().expect("the prompt goes out");
    assert!(intended.try_recv().is_err(), "the prompt goes out once");
    assert!(
        matches!(&sent, Intent::Send { tab: 0, session: None, agent, text }
            if *agent == did("b") && text == "hello"),
        "{sent:?}"
    );
    shell.intend(sent);
    shell
        .apply_through(&mut app, |update| matches!(update, Update::Turn { .. }))
        .await;
    let session = app.tabs[0].session.clone().expect("the tab's new session");
    let request = sent_in(&node, &session).await;
    assert_eq!(
        (request.content.as_deref(), request.agent_did.as_deref()),
        (Some("hello"), Some(did("b").as_str()))
    );
    // The runtime publishes the prompt it claims.
    node.text(&request, 0, MessageRole::User, "hello").await;
    shell.apply_through(&mut app, is_item).await;

    assert_eq!(app.tabs[0].lines, [line("hello", Tone::You, "m0"), blank()]);
}

#[tokio::test]
async fn with_no_agent_or_prompt_the_shell_opens_as_before() {
    let node = ChatNode::new().await;
    let shell = open_shell(node.work());
    let (mut app, intended) = connected_app();

    let applied = shell
        .apply_through(&mut app, |update| {
            matches!(update, Update::Background { .. })
        })
        .await;

    assert_eq!(applied.len(), 5, "{applied:?}");
    assert_eq!(shell.quiet().await, None, "nothing else was queued");
    assert_eq!(app.tabs[0].agent_id, did("a"));
    assert_eq!(app.notice, None);
    assert!(intended.try_recv().is_err());
}

#[tokio::test]
async fn sessions_from_every_agent_merge_newest_first() {
    let node = ChatNode::new().await;
    for (session, agent) in [("a2", "a"), ("b1", "b"), ("a1", "a")] {
        node.session(session, agent).await;
    }
    let shell = open_shell(node.work());

    let sessions = shell.ready().await;

    let listed: Vec<_> = sessions
        .iter()
        .map(|session| (session.id.as_str(), session.agent.clone()))
        .collect();
    assert_eq!(
        listed,
        [("a1", did("a")), ("b1", did("b")), ("a2", did("a"))]
    );
}

#[tokio::test]
async fn opening_a_session_sends_its_transcript_and_running_turn() {
    let node = ChatNode::new().await;
    node.session("s1", "a").await;
    let first = node.request("s1", "t1", "completed").await;
    node.text(&first, 0, MessageRole::User, "hello").await;
    node.text(&first, 1, MessageRole::Assistant, "hi").await;
    let running = node.request("s1", "t2", "processing").await;
    node.text(&running, 2, MessageRole::User, "and now?").await;
    let shell = open_shell(node.work());
    shell.ready().await;

    open(&shell, "s1");

    let opened = Update::Opened {
        session: "s1".to_string(),
        lines: vec![
            line("hello", Tone::You, "m0"),
            blank(),
            line("hi", Tone::Reply, "m1"),
            blank(),
            line("and now?", Tone::You, "m2"),
            blank(),
        ],
    };
    assert_eq!(
        shell.next().await,
        turn("s1", "t2"),
        "before the transcript"
    );
    assert_eq!(shell.next().await, opened);
    assert_eq!(shell.next().await, rows("s1", Vec::new()));
    reread(&shell, 1).await;

    node.text(&running, 3, MessageRole::Assistant, "still going")
        .await;
    let still = item("s1", line("still going", Tone::Reply, "m3"));
    let read = shell.until(is_item).await;
    assert_eq!(read.last(), Some(&still));
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
    is_rows_of(update, "s1")
}

/// An edit of `path` from `before` to `after`, recorded as the runtime records a native file
/// edit's versions, by `request`'s `edit_file` call at `sequence`.
async fn edit(
    node: &ChatNode,
    request: &AgentRequestRow,
    sequence: u32,
    path: &str,
    before: Option<&str>,
    after: &str,
) {
    edit_in(node, request, sequence, path, before, after, "completed").await;
}

async fn edit_in(
    node: &ChatNode,
    request: &AgentRequestRow,
    sequence: u32,
    path: &str,
    before: Option<&str>,
    after: &str,
    state: &str,
) -> String {
    let arguments = json!({ "path": path });
    let tool = node
        .tool(request, sequence, "edit_file", arguments, state, None)
        .await;
    if state == "completed" {
        record(node, request, &tool, path, before, after).await;
    }
    tool
}

/// The versions of `path` the runtime recorded for `tool`'s edit.
async fn record(
    node: &ChatNode,
    request: &AgentRequestRow,
    tool: &str,
    path: &str,
    before: Option<&str>,
    after: &str,
) {
    use base64::Engine as _;
    use gents_protocol::file_changes::{
        FileChangeImages, FileImage, FilePermissions, ToolFileChanges, FILE_CHANGES_VERSION,
    };
    let image = |text: Option<&str>| match text {
        None => FileImage::Missing,
        Some(text) => FileImage::File {
            content_hash: {
                use sha2::Digest as _;
                let digest = sha2::Sha256::digest(text.as_bytes());
                let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
                format!("sha256:{hex}")
            },
            data: base64::engine::general_purpose::STANDARD.encode(text),
            permissions: FilePermissions {
                readonly: false,
                unix_mode: None,
            },
            links: None,
        },
    };
    let changes = ToolFileChanges {
        version: FILE_CHANGES_VERSION,
        tool_call_doc_id: tool.to_string(),
        changes: vec![FileChangeImages {
            root: "/srv/root".to_string(),
            path: path.to_string(),
            before: image(before),
            after: image(Some(after)),
        }],
    };
    let input = json!({ "input": {
        "tool_call_doc_id": tool, "request_doc_id": request.doc_id,
        "node_did": OWNER, "session_id": request.session_id,
        "requester_did": request.requester_did, "file_changes": changes,
    }});
    let mutation = "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }";
    node.create(mutation, input, "ToolFileChange").await;
}

#[tokio::test]
async fn an_opened_sessions_changes_carry_their_turns() {
    let node = ChatNode::new().await;
    node.session("s1", "a").await;
    let first = node.request("s1", "t1", "completed").await;
    node.text(&first, 0, MessageRole::User, "edit it").await;
    edit(&node, &first, 1, "a.rs", Some("a\n"), "b\n").await;
    let second = node.request("s1", "t2", "completed").await;
    edit(&node, &second, 2, "b.rs", None, "x\n").await;
    let shell = open_shell(node.work());
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;

    open(&shell, "s1");
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
    let node = ChatNode::new().await;
    let shell = open_shell(node.work());
    let (mut app, _intended) = connected_app();
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;

    shell.intend(send(app.tabs[0].id, None, &did("a"), "edit it"));
    let started = |update: &Update| matches!(update, Update::Turn { .. });
    shell.apply_through(&mut app, started).await;
    let session = app.tabs[0].session.clone().unwrap();
    let request = sent_in(&node, &session).await;
    assert_eq!(
        app.tabs[0].turn.as_deref(),
        Some(request.request_id.as_str())
    );
    node.update(&request, &claimed(&request)).await;
    let running = edit_in(&node, &request, 0, "a.rs", None, "", "running").await;
    shell.apply_through(&mut app, is_item).await;
    assert!(row_of(&app, "… edit  a.rs").is_some());

    // The runtime commits an edit's versions beside its end.
    record(&node, &request, &running, "a.rs", Some("a\n"), "b\n").await;
    let tool = escape_graphql_string(&running);
    node.run(&format!(
        r#"mutation {{ update_AgentToolCall(docID: "{tool}", input: {{ lifecycle_state: "completed" }}) {{ _docID }} }}"#
    ))
    .await;
    let changed =
        |update: &Update| matches!(update, Update::Item { line, .. } if line.change.is_some());
    shell.apply_through(&mut app, changed).await;
    assert!(row_of(&app, "✓ edit  a.rs").is_some());
    assert_eq!(row_of(&app, "1 file changed"), None, "the turn still runs");

    node.update(&request, r#"lifecycle_state: "completed""#)
        .await;
    let turn_ended = |update: &Update| matches!(update, Update::TurnEnded { .. });
    shell.apply_through(&mut app, turn_ended).await;
    assert!(row_of(&app, "1 file changed  +1 -1").is_some());
    app.show_file(0);
    assert!(app.diff_rows(0).contains(&(" +b".to_string(), Tone::Add)));
}

/// A running turn has no card, from the first update the tab applies.
#[tokio::test]
async fn opening_a_session_with_a_running_turn_never_shows_its_card() {
    let node = ChatNode::new().await;
    node.session("s1", "a").await;
    let first = node.request("s1", "t1", "completed").await;
    node.text(&first, 0, MessageRole::User, "edit it").await;
    edit(&node, &first, 1, "a.rs", Some("a\n"), "b\n").await;
    let running = node.request("s1", "t2", "processing").await;
    node.text(&running, 2, MessageRole::User, "and add one")
        .await;
    edit(&node, &running, 3, "b.rs", None, "x\n").await;
    let shell = open_shell(node.work());
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;

    open(&shell, "s1");
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
    let node = ChatNode::new().await;
    node.session("s1", "a").await;
    let first = node.request("s1", "t1", "completed").await;
    edit(&node, &first, 0, "a.rs", Some("a\n"), "b\n").await;
    let shell = open_shell(node.work());
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;
    open(&shell, "s1");
    shell.apply_through(&mut app, is_s1_rows).await;
    // Then the reread opening asks for.
    shell.apply_through(&mut app, is_s1_rows).await;
    assert!(row_of(&app, "M a.rs").is_some());

    // A session's newest page: an older one's edits are not on it.
    let later = node.request("s1", "t2", "completed").await;
    for sequence in 1..=60 {
        node.text(&later, sequence, MessageRole::Assistant, "more")
            .await;
    }
    edit(&node, &later, 61, "z.rs", Some("a\n"), "b\n").await;
    open(&shell, "s1");
    shell.apply_through(&mut app, is_s1_rows).await;

    assert!(row_of(&app, "M z.rs").is_some());
    assert_eq!(row_of(&app, "a.rs"), None);
    let view = diff_view(&app);
    assert!(view.iter().any(|row| row.contains("M z.rs")), "{view:?}");
    assert!(!view.iter().any(|row| row.contains("a.rs")), "{view:?}");
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

/// `s1`, on agent `a`, whose request `r1` started a subagent session for each of `children` on
/// agent `researcher`, with its latest request in that state.
async fn spawned(node: &ChatNode, children: &[(&str, &str)]) -> AgentRequestRow {
    node.session("s1", "a").await;
    let request = node.request("s1", "r1", "completed").await;
    for (child, state) in children {
        node.child(&request, child, "researcher").await;
        if *state != "completed" {
            node.request(child, &format!("{child}-r2"), state).await;
        }
    }
    request
}

#[tokio::test]
async fn opening_a_session_lists_its_subagents_as_running_rows() {
    let node = ChatNode::new().await;
    spawned(&node, &[("c1", "completed"), ("c3", "failed")]).await;
    let shell = open_shell(node.work());
    shell.ready().await;

    open(&shell, "s1");

    let opened = shell.next().await;
    assert!(is_opened(&opened, "s1"), "{opened:?}");
    let expected = vec![
        row("c1", "finished", RunState::Done),
        row("c3", "error", RunState::Failed),
    ];
    assert_eq!(shell.next().await, rows("s1", expected));
}

#[tokio::test]
async fn a_working_subagents_end_refreshes_the_rows() {
    let node = ChatNode::new().await;
    spawned(&node, &[("c1", "processing")]).await;
    let shell = open_shell(node.work());
    shell.ready().await;

    open(&shell, "s1");
    let opened = shell.next().await;
    assert!(is_opened(&opened, "s1"), "{opened:?}");
    let working = vec![row("c1", "working", RunState::Working)];
    assert_eq!(shell.next().await, rows("s1", working));
    reread(&shell, 1).await;

    let latest = node.request_row("c1-r2").await;
    node.update(&latest, r#"lifecycle_state: "completed""#)
        .await;

    let finished = rows("s1", vec![row("c1", "finished", RunState::Done)]);
    let read = shell.until(is_s1_rows).await;
    assert_eq!(read.last(), Some(&finished));
}

#[tokio::test]
async fn a_spawn_on_the_open_session_relists_its_subagents() {
    let node = ChatNode::new().await;
    let request = spawned(&node, &[]).await;
    let shell = open_shell(node.work());
    shell.ready().await;
    open(&shell, "s1");
    shell.next().await;
    assert_eq!(shell.next().await, rows("s1", Vec::new()));
    reread(&shell, 1).await;

    node.child(&request, "c1", "researcher").await;

    let spawned = rows("s1", vec![row("c1", "finished", RunState::Done)]);
    let read = shell.until(is_s1_rows).await;
    assert_eq!(read.last(), Some(&spawned));
}

#[tokio::test]
async fn opening_a_subagent_shows_it_and_streams_it() {
    let node = ChatNode::new().await;
    spawned(&node, &[("c1", "processing")]).await;
    let working = node.request_row("c1-r2").await;
    node.text(&working, 0, MessageRole::User, "look into it")
        .await;
    let shell = open_shell(node.work());
    shell.ready().await;

    shell.intend(Intent::Open {
        session: "c1".to_string(),
        agent: did("researcher"),
    });

    let opened = Update::Opened {
        session: "c1".to_string(),
        lines: vec![line("look into it", Tone::You, "m0"), blank()],
    };
    assert_eq!(shell.next().await, turn("c1", "c1-r2"));
    assert_eq!(shell.next().await, opened);
    assert_eq!(shell.next().await, rows("c1", Vec::new()));
    reread(&shell, 1).await;
    node.flush(&working, 0, "found").await;
    let delta = Update::Item {
        session: "c1".to_string(),
        line: line("found", Tone::Reply, "m1"),
    };
    let read = shell.until(is_item).await;
    assert_eq!(read.last(), Some(&delta));
}

#[tokio::test]
async fn a_first_send_starts_a_session_then_a_turn() {
    let node = ChatNode::new().await;
    let shell = open_shell(node.work());
    shell.ready().await;

    shell.intend(send(7, None, &did("a"), "hello"));

    let Update::Started { tab: 7, session } = shell.next().await else {
        panic!("Started first");
    };
    let started = shell.next().await;
    let request = sent_in(&node, &session).await;
    assert_eq!(started, turn(&session, &request.request_id));
    reread(&shell, 1).await;
    assert_eq!(request.content.as_deref(), Some("hello"));
    let input = request.input.clone().unwrap_or_default();
    let queue = input.queue.expect("a user message is queued");
    assert_eq!(
        queue.delivery,
        gents_protocol::request_input::QueueDelivery::Steer,
        "it steers a running turn, as the desktop's does"
    );
    node.text(&request, 0, MessageRole::User, "hello").await;
    let said = item(&session, line("hello", Tone::You, "m0"));
    let read = shell.until(is_item).await;
    assert_eq!(read.last(), Some(&said));
}

#[tokio::test]
async fn a_reply_streams_as_deltas() {
    let node = ChatNode::new().await;
    let shell = open_shell(node.work());
    shell.ready().await;

    shell.intend(send(0, None, &did("a"), "hi"));
    let Update::Started { session, .. } = shell.next().await else {
        panic!("Started first");
    };
    let started = shell.next().await;
    let request = sent_in(&node, &session).await;
    assert_eq!(started, turn(&session, &request.request_id));
    reread(&shell, 1).await;
    node.update(&request, &claimed(&request)).await;
    node.text(&request, 0, MessageRole::User, "hi").await;
    shell.until(is_item).await;

    node.flush(&request, 0, "Hel").await;
    let first = item(&session, line("Hel", Tone::Reply, "m1"));
    assert_eq!(shell.until(is_item).await.last(), Some(&first));
    node.flush(&request, 1, "lo").await;
    let delta = |update: &Update| matches!(update, Update::Delta { .. });
    let more = Update::Delta {
        session: session.clone(),
        item: "m1".to_string(),
        text: "lo".to_string(),
        tone: Tone::Reply,
    };
    assert_eq!(shell.until(delta).await.last(), Some(&more));

    node.text(&request, 1, MessageRole::Assistant, "Hello")
        .await;
    node.update(&request, r#"lifecycle_state: "completed""#)
        .await;
    let done = ended(&session, &request.request_id);
    let turn_ended = |update: &Update| matches!(update, Update::TurnEnded { .. });
    let read = shell.until(turn_ended).await;
    assert_eq!(read.last(), Some(&done));
}

fn interrupt(session: &str, turn: &str) -> Intent {
    Intent::Interrupt {
        session: session.to_string(),
        agent: did("b"),
        turn: turn.to_string(),
    }
}

#[tokio::test]
async fn interrupt_stops_the_running_turn() {
    let node = ChatNode::new().await;
    node.session("s1", "a").await;
    node.request("s1", "t1", "processing").await;
    let shell = open_shell(node.work());
    shell.ready().await;

    shell.intend(interrupt("s1", "t1"));

    assert_eq!(shell.next().await, ended("s1", "t1"));
    let stopped = node.request_row("t1").await;
    assert!(
        stopped.interrupt_requested_at.is_some(),
        "the request is latched: {stopped:?}"
    );
}

/// Stopping exactly the tab's turn leaves another client's newer turn running.
#[tokio::test]
async fn interrupt_clears_a_turn_that_has_ended_without_stopping_another() {
    let node = ChatNode::new().await;
    node.session("s1", "a").await;
    node.request("s1", "t1", "completed").await;
    node.request("s1", "t2", "processing").await;
    let shell = open_shell(node.work());
    shell.ready().await;

    shell.intend(interrupt("s1", "t1"));

    assert_eq!(shell.next().await, ended("s1", "t1"));
    for request in ["t1", "t2"] {
        let row = node.request_row(request).await;
        assert_eq!(row.interrupt_requested_at, None, "{request}");
    }
}

#[tokio::test]
async fn a_refused_send_becomes_a_start_failure_in_the_owners_words() {
    let node = ChatNode::new().await;
    let shell = open_shell(node.work());
    shell.ready().await;
    let agent = escape_graphql_string(&did("a"));
    node.run(&format!(
        r#"mutation {{ update_Agent(filter: {{ agent_did: {{ _eq: "{agent}" }} }}, input: {{ enabled: false }}) {{ _docID }} }}"#
    ))
    .await;

    shell.intend(send(3, Some("s1"), &did("a"), "hi"));

    // A failure the tab is told of, so it stops showing the turn as running.
    let refused = Update::StartFailed {
        tab: 3,
        message: "request agent is disabled".to_string(),
    };
    let read = shell
        .until(|update| matches!(update, Update::StartFailed { .. }))
        .await;
    assert_eq!(read.last(), Some(&refused));
    shell.intend(send(4, Some("s1"), "did:key:z:nobody", "hi"));
    let unknown = Update::StartFailed {
        tab: 4,
        message: "agent did:key:z:nobody is not open in this shell".to_string(),
    };
    let read = shell
        .until(|update| matches!(update, Update::StartFailed { .. }))
        .await;
    assert_eq!(read.last(), Some(&unknown));
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

/// `node_settings_of(did)` with agent `a` on context `main`, which lists `commit` and `review`,
/// and agent `b` on no context. `old` is listed too, and turned off.
fn skill_settings(did: &str) -> Value {
    let mut config = node_settings_of(did);
    config["node"]["default_agent_did"] = json!("a");
    config["contexts"] = json!([
        { "node_did": did, "context_id": "main", "skill_ids": ["commit", "review", "old"] },
    ]);
    let skills = config["skills"].as_array_mut().unwrap();
    skills.push(json!({ "node_did": did, "skill_id": "commit", "name": "commit", "description": "Writes commits" }));
    skills.push(json!({ "node_did": did, "skill_id": "old", "name": "old", "description": "Retired", "enabled": false }));
    let agent = |id: &str| json!({ "node_did": did, "agent_did": id, "display_name": id, "inference_profile_id": "profile" });
    let mut a = agent("a");
    a["context_id"] = json!("main");
    config["agents"]
        .as_array_mut()
        .unwrap()
        .extend([a, agent("b")]);
    config
}

/// The shell on a node served with `skill_settings`, as on a home `gents init` set up.
struct OnNode {
    shell: Shell,
    node: Arc<defra_node::Node>,
    work: Work,
}

async fn on_node() -> OnNode {
    let (node, work, _home) = served(skill_settings(OWNER)).await;
    let runtime = ConfigAccess::local_as(node.clone(), ::identity::Did::new(OWNER).unwrap());
    publish_ready(&runtime, &["a", "b"]).await;
    let shell = open_shell(work.clone());
    shell.ready().await;
    OnNode { shell, node, work }
}

fn read_skills(agent: &str) -> Intent {
    Intent::ReadSkills {
        agent: agent.to_string(),
    }
}

#[tokio::test]
async fn skills_list_only_what_the_agents_context_allows_and_is_on() {
    let on = on_node().await;

    on.shell.intend(read_skills(&did("a")));
    let told = on.shell.until(is_skills).await;

    let allowed = Update::Skills {
        agent: did("a"),
        skills: vec![
            shown_skill("commit", "Writes commits"),
            shown_skill("review", "review"),
        ],
    };
    assert_eq!(told.last(), Some(&allowed));
    on.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn an_agent_with_no_context_offers_no_skills() {
    let on = on_node().await;

    on.shell.intend(read_skills(&did("b")));
    let told = on.shell.until(is_skills).await;

    let none = Update::Skills {
        agent: did("b"),
        skills: Vec::new(),
    };
    assert_eq!(told.last(), Some(&none));
    on.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_skill_turn_sends_the_listed_skill() {
    let on = on_node().await;
    let send = |session: &str, skill: &str, text: &str| Intent::SendSkill {
        tab: 7,
        session: Some(session.to_string()),
        agent: did("a"),
        skill: skill.to_string(),
        text: text.to_string(),
    };
    let is_turn = |update: &Update| matches!(update, Update::Turn { .. });

    on.shell.intend(read_skills(&did("a")));
    on.shell.until(is_skills).await;
    on.shell.intend(send("s1", "commit", "fix it"));
    on.shell.until(is_turn).await;
    on.shell.intend(send("s2", "commit", ""));
    on.shell.until(is_turn).await;
    on.shell.intend(send("s3", "nope", "fix it"));
    let missed = on
        .shell
        .until(|update| matches!(update, Update::StartFailed { .. }))
        .await;

    let unknown = Update::StartFailed {
        tab: 7,
        message: "unknown skill /nope".to_string(),
    };
    assert_eq!(missed.last(), Some(&unknown));
    let access = on.work.access();
    for (session, content) in [("s1", "fix it"), ("s2", "")] {
        let query = format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ content input }} }}"#
        );
        let requests = access.execute(&query).await.unwrap();
        let request = &requests["data"]["AgentRequest"][0];
        assert_eq!(request["content"], json!(content), "{requests}");
        let input: gents_protocol::request_input::RequestInput =
            serde_json::from_value(request["input"].clone()).unwrap();
        assert_eq!(input.selected_skill_ids, ["commit"]);
    }
    on.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_failed_skills_read_leaves_no_rows_to_show_or_send() {
    let node = ChatNode::new().await;
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let read = reads.clone();
    let fake = FakeGraphql::start(node, move |query| {
        let skills = query.contains("Skill(") && !query.contains("DesktopSnapshot");
        match skills && read.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
            true => Some(
                json!({ "data": null, "errors": [{ "message": "the skills are unreadable" }] }),
            ),
            false => None,
        }
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;

    shell.intend(read_skills(&did("a")));
    shell.until(is_skills).await;
    shell.intend(read_skills(&did("a")));
    let told = shell.until(is_skills).await;
    shell.intend(Intent::SendSkill {
        tab: 7,
        session: Some("s1".to_string()),
        agent: did("a"),
        skill: "review".to_string(),
        text: String::new(),
    });
    let missed = shell
        .until(|update| matches!(update, Update::StartFailed { .. }))
        .await;

    let notice = told.iter().find_map(|update| match update {
        Update::Notice(notice) => Some(notice),
        _ => None,
    });
    assert!(
        notice.is_some_and(|notice| notice.contains("the skills are unreadable")),
        "{told:?}"
    );
    let cleared = Update::Skills {
        agent: did("a"),
        skills: Vec::new(),
    };
    assert_eq!(told.last(), Some(&cleared));
    let unknown = Update::StartFailed {
        tab: 7,
        message: "unknown skill /review".to_string(),
    };
    assert_eq!(missed.last(), Some(&unknown));
}

fn set_goal(agent: &str, session: Option<&str>, objective: &str) -> Intent {
    Intent::SetGoal {
        tab: 7,
        session: session.map(str::to_string),
        agent: agent.to_string(),
        objective: objective.to_string(),
    }
}

/// A goal set opens its session, whose rows end the opening; a refused one fails the tab's
/// start.
fn is_goal_answer(update: &Update) -> bool {
    matches!(update, Update::Running { .. } | Update::StartFailed { .. })
}

#[tokio::test]
async fn a_goal_starts_with_its_objective_as_the_sessions_next_message() {
    let on = on_node().await;

    on.shell.intend(set_goal(&did("a"), Some("s1"), "ship it"));
    let told = on.shell.until(is_goal_answer).await;

    let opened = told.iter().position(|update| is_opened(update, "s1"));
    let running = told
        .iter()
        .position(|update| matches!(update, Update::Turn { session, .. } if session == "s1"));
    assert!(
        running
            .zip(opened)
            .is_some_and(|(running, opened)| running < opened),
        "its request runs as the session's turn: {told:?}"
    );
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
        escape_graphql_string(root)
    );
    let requests = access.execute(&query).await.unwrap();
    let request = &requests["data"]["AgentRequest"][0];
    assert_eq!(
        (&request["content"], &request["session_id"]),
        (&json!("ship it"), &json!("s1")),
        "{requests}"
    );
    on.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_goal_with_no_session_starts_one_first() {
    let on = on_node().await;

    on.shell.intend(set_goal(&did("a"), None, "ship it"));
    let mut told = on.shell.until(is_goal_answer).await;
    told.extend(
        on.shell
            .until(|update| matches!(update, Update::Sessions(_)))
            .await,
    );

    let Some(Update::Started { tab: 7, session }) = told.first().cloned() else {
        panic!("{told:?}");
    };
    let opened = told.iter().position(|update| is_opened(update, &session));
    let lists_new = |update: &Update| match update {
        Update::Sessions(sessions) => sessions.iter().any(|listed| listed.id == session),
        _ => false,
    };
    let listed = told.iter().position(lists_new);
    assert!(
        opened
            .zip(listed)
            .is_some_and(|(opened, listed)| opened < listed),
        "the new session joins the list: {told:?}"
    );
    on.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_refused_goal_shows_the_owners_reason() {
    let on = on_node().await;
    on.shell.intend(set_goal(&did("a"), Some("s1"), "ship it"));
    let first = on.shell.until(is_goal_answer).await;
    assert!(
        matches!(first.last(), Some(Update::Running { .. })),
        "the first goal is set: {first:?}"
    );

    on.shell
        .intend(set_goal(&did("a"), Some("s1"), "ship more"));
    let told = on.shell.until(is_goal_answer).await;

    let Some(Update::StartFailed { tab: 7, message }) = told.last() else {
        panic!("{told:?}");
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let options = crate::RequestSubmitOptions {
        retry_key: Some(format!("goal-request:{request_id}")),
        ..Default::default()
    };
    let agent = did("a");
    let work = &on.work;
    let prepared = crate::prepare_agent_request_with_input(
        &work.endpoint,
        OWNER,
        "ship more",
        Some("s1"),
        Some(agent.as_str()),
        Some(request_id),
        options,
    );
    let create = prepared.await.unwrap().create;
    let access = work.access();
    let owner = gents_automation::goal::submit_goal_backed_request(
        &access,
        OWNER,
        "s1",
        "ship more",
        None,
        &create,
    );
    let owner = owner.await.unwrap_err();
    assert_eq!(*message, format!("Couldn’t set the goal: {owner:#}"));
    on.node.shutdown().await.expect("node shutdown");
}

/// The chat streams over the feed, so its end leaves the shell nothing to show: it is lost.
#[tokio::test]
async fn a_lost_feed_names_the_next_step() {
    let node = ChatNode::new().await;
    let fake = FakeGraphql::start(node, |_| None).await;
    let shell = open_shell(fake.work());
    shell.ready().await;

    fake.end_streams(is_chat);

    let lost = "runtime closed: check `gents status`, then reopen the shell";
    let read = shell
        .until(|update| matches!(update, Update::Connection(_)))
        .await;
    assert_eq!(
        read.last(),
        Some(&Update::Connection(Connection::Lost(lost.to_string())))
    );
}

#[tokio::test]
async fn a_broken_feed_names_its_error() {
    let node = ChatNode::new().await;
    let fake = FakeGraphql::start(node, |_| None).await;
    let shell = open_shell(fake.work());
    shell.ready().await;

    fake.break_streams(is_chat);

    let read = shell
        .until(|update| matches!(update, Update::Connection(_)))
        .await;
    let Some(Update::Connection(Connection::Lost(lost))) = read.last() else {
        panic!("{read:?}");
    };
    assert!(
        lost.starts_with("runtime closed: check `gents status`, then reopen the shell (")
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
async fn an_unreadable_runtime_is_lost_before_anything_is_read() {
    let off = "no runtime recorded".to_string();
    let shell = open_shell_with(Err(off), Launch::default());

    let lost = "no runtime: check `gents status`, then reopen the shell (no runtime recorded)";
    assert_eq!(
        shell.next().await,
        Update::Connection(Connection::Lost(lost.to_string()))
    );
    let unanswering = Work {
        endpoint: user_endpoint("http://127.0.0.1:1/api/v0/graphql"),
        home: std::path::PathBuf::new(),
    };
    let shell = open_shell(unanswering);
    let update = shell.next().await;
    let Update::Connection(Connection::Lost(lost)) = &update else {
        panic!("{update:?}");
    };
    assert!(
        lost.starts_with("no runtime: check `gents status`, then reopen the shell ("),
        "{lost}"
    );
}

/// A repository in a new folder, on branch `trunk`.
fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-b", "trunk"]);
    git(repo.path(), &["commit", "--allow-empty", "-m", "start"]);
    repo
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[tokio::test]
async fn the_branch_comes_from_git_in_the_working_directory() {
    let repo = repository();
    let node = ChatNode::new().await;
    let opened = Launch {
        cwd: Some(repo.path().into()),
        ..Default::default()
    };
    let shell = open_shell_with(Ok(node.work()), opened);

    assert!(matches!(shell.next().await, Update::Agents(_)));
    assert_eq!(shell.next().await, Update::Connection(Connection::Ready));
    let branch = Update::Branch("trunk".to_string());
    // `next` panics when nothing more arrives within its wait.
    while shell.next().await != branch {}
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
            let identity = gents_store::identity::KeyIdentity::load_or_create(key, None).unwrap();
            let did = gents_store::identity::NodeIdentity::did(&identity).to_string();
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
        None => OWNER.to_string(),
    };
    crate::home_state::write_runtime_state(
        home.path(),
        &crate::shared::StoredRuntimeState {
            home: path,
            graphql: graphql.to_string(),
            node_name: "fixture".into(),
            node_did: node_did.clone(),
            default_agent_did: "a".into(),
            p2p_transport: "none".into(),
            p2p_peer_id: None,
            p2p_listen_addresses: Vec::new(),
            p2p_admission: None,
        },
    )
    .unwrap();
    (home, node_did)
}

#[tokio::test]
async fn work_for_signs_as_this_clis_user() {
    let graphql = "http://127.0.0.1:39999/api/v0/graphql";
    let (home, _) = home_serving(graphql, None);
    let user = crate::shared::test_support::cli_user().await;

    let work = work::work_for(home.path()).expect("this CLI's User signs");

    assert_eq!(work.access().actor_did().as_deref(), Some(user.did()));
}

/// With no runtime recorded there is nothing to read, so the shell has nothing to show.
#[tokio::test]
async fn work_for_turns_off_with_no_runtime_recorded() {
    let bare = tempfile::tempdir().unwrap();
    let off = work::work_for(bare.path())
        .err()
        .expect("no runtime recorded");
    assert!(off.contains("gents runtime server"), "{off}");
    let shell = open_shell_with(Err(off.clone()), Launch::default());
    let lost = format!("no runtime: check `gents status`, then reopen the shell ({off})");
    assert_eq!(
        shell.next().await,
        Update::Connection(Connection::Lost(lost))
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

/// A posted query as the scripts match it: a typed query sends its values as
/// variables, so they are written back in as literals.
fn posted_query(posted: &Value) -> String {
    let mut query = posted["query"].as_str().unwrap_or_default().to_string();
    if let Some(variables) = posted["variables"].as_object() {
        let mut names: Vec<_> = variables.keys().collect();
        names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        for name in names {
            query = query.replace(&format!("${name}"), &variables[name].to_string());
        }
    }
    query
}

fn without_whitespace(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// The filter that opens `list_jobs`' row read.
const JOB_LIST: &str = "AgentToolCall(filter: { job_pin: { _ne: null }";

fn is_job_list(query: &str) -> bool {
    without_whitespace(query).contains(&without_whitespace(JOB_LIST))
}

/// The background owner's read of the running calls.
fn is_call_list(query: &str) -> bool {
    query.contains("await_mode") && query.contains(RUNNING_CALLS)
}

/// Answers `list_jobs`' query with the `jobs` of the session it names, as the node filters
/// them, and the background owner's with `calls`. The node answers the rest.
fn work_answer(query: &str, jobs: &[Value], calls: &[Value]) -> Option<Value> {
    if is_job_list(query) {
        let asked = without_whitespace(query);
        let named = |job: &&Value| {
            asked.contains(&without_whitespace(&format!(
                r#"session_id: {{ _eq: {} }}"#,
                job["session_id"]
            )))
        };
        let jobs: Vec<_> = jobs.iter().filter(named).collect();
        Some(json!({ "data": { "AgentToolCall": jobs } }))
    } else if is_call_list(query) {
        Some(json!({ "data": { "AgentToolCall": calls } }))
    } else {
        None
    }
}

/// The work views' reads: the job rows and the running calls.
fn work_reads(fake: &FakeGraphql) -> usize {
    let queries = fake.queries();
    let reads = queries
        .iter()
        .filter(|query| is_job_list(query) || is_call_list(query));
    reads.count()
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
    let fake = FakeGraphql::start(ChatNode::new().await, |query| {
        let calls = [background_row("bash", "pending")];
        work_answer(query, &[job_row("s1", "call-1")], &calls)
    })
    .await;
    let shell = open_shell(fake.work());
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
    let watched = queries.iter().position(|query| query == source::WATCHED);
    let read = queries.iter().position(|query| is_call_list(query));
    assert!(
        watched.is_some() && watched < read,
        "it subscribes before it reads: {queries:?}"
    );
    let background = &queries[read.unwrap()];
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

/// The `list_jobs` row reads.
fn job_reads(fake: &FakeGraphql) -> Vec<String> {
    let queries = fake.queries().into_iter();
    queries.filter(|query| is_job_list(query)).collect()
}

fn call_reads(fake: &FakeGraphql) -> usize {
    let queries = fake.queries();
    queries
        .iter()
        .filter(|query| query.contains(RUNNING_CALLS))
        .count()
}

#[tokio::test]
async fn jobs_are_read_per_open_session_in_every_state() {
    let fake = FakeGraphql::start(ChatNode::new().await, |query| {
        let jobs = [
            job_row("s1", "call-1"),
            ended_row("s1", "call-2", "completed"),
            job_row("s2", "call-3"),
            ended_row("s1", "call-4", "failed"),
            ended_row("s1", "call-5", "cancelled"),
            ended_row("s1", "call-6", "timedOut"),
        ];
        work_answer(query, &jobs, &[])
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;
    assert_eq!(job_reads(&fake), Vec::<String>::new(), "no session is open");

    open(&shell, "s1");
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
        without_whitespace(&asked[0]).contains(r#"session_id:{_eq:"s1"}"#),
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
        let named = format!(r#"session_id:{{_eq:"{session}"}}"#);
        let reads = asked
            .iter()
            .filter(|query| without_whitespace(query).contains(&named))
            .count();
        assert_eq!(reads, 1, "{session}: {asked:?}");
    }
}

/// A session opens when a tab picks it or sends to it, and either reads its jobs.
#[tokio::test]
async fn opening_a_session_rereads_work() {
    let fake = FakeGraphql::start(ChatNode::new().await, |query| {
        let jobs = [job_row("s1", "call-1"), job_row("s2", "call-2")];
        work_answer(query, &jobs, &[])
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;

    open(&shell, "s1");
    opening(&shell, "s1").await;

    let read = reread(&shell, 1).await;
    let s1 = Update::Jobs {
        jobs: vec![shown_job("s1", "call-1")],
        as_of: "live".to_string(),
    };
    assert_eq!(read[0], s1);

    shell.intend(send(1, Some("s2"), &did("a"), "hi"));
    let update = shell.next().await;
    assert!(
        matches!(&update, Update::Turn { session, .. } if session == "s2"),
        "{update:?}"
    );

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
    let fake = FakeGraphql::start(ChatNode::new().await, |_| None).await;
    let shell = open_shell(fake.work());
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
    let kept = fake.subscribed(source::WATCHED);
    assert_eq!(kept, 1, "a live subscription is kept");
}

#[tokio::test]
async fn a_tool_call_change_rereads_the_jobs() {
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let job = started.clone();
    let fake = FakeGraphql::start(ChatNode::new().await, move |query| {
        let jobs = match job.load(std::sync::atomic::Ordering::SeqCst) {
            true => vec![job_row("s1", "call-1")],
            false => Vec::new(),
        };
        work_answer(query, &jobs, &[])
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;
    open(&shell, "s1");
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

/// A request changing, as a turn starting or ending does.
fn turn_changed(fake: &FakeGraphql) {
    fake.chat_changed("AgentRequest");
}

/// The node sends no keep-alives and replays nothing, so an ended subscription is only noticed,
/// and the next turn subscribes again and rereads, once.
#[tokio::test]
async fn an_ended_subscription_goes_stale_until_the_next_turn() {
    let fake = FakeGraphql::start(ChatNode::new().await, |_| None).await;
    let shell = open_shell(fake.work());
    shell.ready().await;
    let read = work_reads(&fake);

    turn_changed(&fake);
    assert_eq!(shell.quiet().await, None, "a live view needs no reread");
    assert_eq!(work_reads(&fake), read);

    fake.end_streams(is_work);
    for _ in 0..2 {
        let update = shell.next().await;
        let (Update::Jobs { as_of, .. } | Update::Background { as_of, .. }) = &update else {
            panic!("{update:?}");
        };
        assert!(as_of.starts_with("as of "), "{update:?}");
    }
    assert_eq!(work_reads(&fake), read, "ending reads nothing");

    turn_changed(&fake);
    let live = |update: &Update| matches!(update, Update::Jobs { as_of, .. } | Update::Background { as_of, .. } if as_of == "live");
    assert!(live(&shell.next().await));
    assert!(live(&shell.next().await));
    assert_eq!(fake.subscribed(source::WATCHED), 2);
    let queries = fake.queries();
    let again = queries.iter().rposition(|query| query == source::WATCHED);
    let reread = queries.iter().rposition(|query| is_call_list(query));
    assert!(again < reread, "it subscribes before it reads: {queries:?}");

    turn_changed(&fake);
    assert_eq!(shell.quiet().await, None, "live again");
    assert_eq!(work_reads(&fake), read + 1);
}

#[tokio::test]
async fn a_sessions_jobs_follow_its_subagents_in_its_running_rows() {
    let node = ChatNode::new().await;
    spawned(&node, &[("c1", "completed")]).await;
    let fake = FakeGraphql::start(node, |query| {
        let jobs = [
            job_row("s1", "call-1"),
            ended_row("s1", "call-3", "completed"),
            job_row("s2", "call-2"),
        ];
        work_answer(query, &jobs, &[])
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;

    open(&shell, "s1");

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
    let fake = FakeGraphql::start(ChatNode::new().await, |query| match is_job_list(query) {
        true => Some(json!({ "data": null, "errors": [{ "message": "the job store is closed" }] })),
        false => work_answer(query, &[], &[background_row("bash", "running")]),
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;

    open(&shell, "s1");
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
    let fake = FakeGraphql::start(ChatNode::new().await, move |query| {
        let failed = fails.load(std::sync::atomic::Ordering::SeqCst);
        match failed && is_job_list(query) {
            true => {
                Some(json!({ "data": null, "errors": [{ "message": "the job store is closed" }] }))
            }
            false => work_answer(query, &[job_row("s1", "call-1")], &[]),
        }
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;
    open(&shell, "s1");
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

/// A reread waits on the node, never the chat: transcripts and intents go on meanwhile.
#[tokio::test]
async fn a_slow_reread_holds_back_no_chat() {
    let node = ChatNode::new().await;
    node.session("s1", "a").await;
    let request = node.request("s1", "t1", "processing").await;
    let fake = FakeGraphql::start(node, |query| work_answer(query, &[], &[])).await;
    let shell = open_shell(fake.work());
    shell.ready().await;
    open(&shell, "s1");
    shell.until(|update| is_rows_of(update, "s1")).await;
    reread(&shell, 1).await;
    let jobs = job_reads(&fake).len();
    fake.hold(true);
    fake.changed();
    fake.posted(JOB_LIST, jobs + 1).await;

    fake.node
        .text(&request, 0, MessageRole::Assistant, "Hel")
        .await;
    fake.chat_changed("AgentMessage");
    let update = shell.next().await;
    assert!(matches!(update, Update::Item { .. }), "{update:?}");
    shell.intend(send(0, Some("s2"), &did("a"), "hi"));
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
    let fake = FakeGraphql::start(ChatNode::new().await, |query| {
        work_answer(query, &[job_row("s1", "call-1")], &[])
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;
    open(&shell, "s1");
    opening(&shell, "s1").await;
    reread(&shell, 1).await;
    let (jobs, calls) = (job_reads(&fake).len(), call_reads(&fake));
    fake.hold(true);
    fake.changed();
    fake.posted(JOB_LIST, jobs + 1).await;

    for _ in 0..3 {
        fake.changed();
    }
    let more = tokio::time::timeout(SHORT_WAIT, fake.posted(JOB_LIST, jobs + 2)).await;
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
        "requester_did": OWNER,
        "node_did": OWNER,
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
        "target_agent_did": "a",
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
/// each `_docID` read with its item. The node answers every other query.
fn mailbox_answer(query: &str, items: &[Value]) -> Option<Value> {
    if !query.trim_start().starts_with("{ MailboxItem(") {
        return None;
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
    Some(json!({ "data": { "MailboxItem": rows } }))
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

const MAILBOX_LIST: &str = r#"MailboxItem(filter: { status: { _eq: "open" } },"#;

/// The chat node, with the mailbox `graphql` answers.
async fn with_mailbox<G>(graphql: G) -> FakeGraphql
where
    G: Fn(&str) -> Option<Value> + Send + Sync + 'static,
{
    FakeGraphql::start(ChatNode::new().await, graphql).await
}

#[tokio::test]
async fn an_open_session_shows_its_ask_and_gate_items_oldest_first() {
    let items = [
        mailbox_item("i5", "s1", "ask", "ack", "did:key:zOther", 5),
        mailbox_item("i4", "s1", "finished", "ack", OWNER, 4),
        mailbox_item("i3", "s2", "ask", "ack", OWNER, 3),
        mailbox_item("i2", "s1", "gate", "ack", OWNER, 2),
        mailbox_item("i1", "s1", "ask", "start_request", OWNER, 1),
    ];
    let fake = with_mailbox(move |query| mailbox_answer(query, &items)).await;
    let shell = open_shell(fake.work());
    shell.ready().await;

    open(&shell, "s1");

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
        let mut item = mailbox_item("p1", "s1", "gate", "ack", OWNER, 1);
        if elsewhere.load(std::sync::atomic::Ordering::SeqCst) {
            item["status"] = json!("acted");
        }
        mailbox_answer(query, &[item])
    })
    .await;
    let shell = open_shell(fake.work());
    let (mut app, _intended) = connected_app();
    app.tabs[0].session = Some("s1".to_string());
    let ready = |update: &Update| matches!(update, Update::Background { .. });
    shell.apply_through(&mut app, ready).await;
    open(&shell, "s1");
    let mut read = opening(&shell, "s1").await;
    read.extend(reread_and(&shell, is_prompts).await);
    for update in read {
        app.apply(update);
    }
    assert_eq!(app.tabs[0].prompts.len(), 1);
    closed.store(true, std::sync::atomic::Ordering::SeqCst);

    open(&shell, "s1");
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
    let node = ChatNode::new().await;
    spawned(&node, &[("c1", "completed")]).await;
    let fake = FakeGraphql::start(node, |query| {
        let asked = mailbox_item("i1", "c1", "ask", "start_request", OWNER, 1);
        mailbox_answer(query, &[asked])
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;

    open(&shell, "s1");

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
    use gents_documents::mailbox_write_decl::{MailboxAction, MailboxKind};
    let (node, work, _home) = served_settings().await;
    let gate = filed(
        &node,
        &work,
        "i1 title",
        MailboxKind::Gate,
        MailboxAction::Ack,
    )
    .await;
    let ask = MailboxAction::StartRequest;
    let question = filed(&node, &work, "i2 title", MailboxKind::Ask, ask).await;
    let access = work.access();
    let shell = shell_on_served(&node, work).await;
    open(&shell, "s1");
    opening(&shell, "s1").await;
    let read = reread_and(&shell, is_prompts).await;
    let titled = |prompt: Prompt, item: &str, title: &str| Prompt {
        item: item.to_string(),
        title: title.to_string(),
        ..prompt
    };
    let gated = titled(
        asked("i1", PromptKind::Gate, PromptAnswer::Ack),
        &gate,
        "i1 title",
    );
    let asking = asked("i2", PromptKind::Ask, PromptAnswer::Reply);
    let asking = titled(asking, &question, "i2 title");
    let shown: Vec<_> = read.iter().filter(|update| is_prompts(update)).collect();
    assert_eq!(shown, [&prompts("s1", vec![gated, asking.clone()])]);

    shell.intend(Intent::Dismiss {
        session: "s1".to_string(),
        item: gate.clone(),
    });

    let read = shell.until(is_prompts).await;
    assert_eq!(read, [prompts("s1", vec![asking])], "no notice: {read:?}");
    let stored =
        gents_session::session::mailbox::load_mailbox_item_with_access(&access, &gate).await;
    assert_eq!(stored.unwrap().unwrap().status, "dismissed");
    node.shutdown().await.expect("node shutdown");
}

/// An item closed elsewhere while the mailbox feed is quiet: the refused reply rereads, so the
/// tab is not left answering a prompt that is gone.
#[tokio::test]
async fn a_reply_to_a_closed_item_is_a_notice_in_the_owners_words() {
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let elsewhere = closed.clone();
    let fake = with_mailbox(move |query| {
        let mut item = mailbox_item("i1", "s1", "ask", "start_request", OWNER, 1);
        if elsewhere.load(std::sync::atomic::Ordering::SeqCst) {
            item["status"] = json!("acted");
        }
        mailbox_answer(query, &[item])
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;
    open(&shell, "s1");
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
}

/// A reply is a request in the item's session, caused by the item, which the session's feed
/// shows like any other request.
#[tokio::test]
async fn a_reply_is_a_request_caused_by_its_item() {
    use gents_documents::mailbox_write_decl::{MailboxAction, MailboxKind};
    let (node, work, _home) = served_settings().await;
    let ask = MailboxAction::StartRequest;
    let question = filed(&node, &work, "i1 title", MailboxKind::Ask, ask).await;
    let access = work.access();
    let shell = shell_on_served(&node, work).await;
    open(&shell, "s1");
    shell.until(is_prompts).await;

    shell.intend(Intent::Reply {
        session: "s1".to_string(),
        agent: did("engineer"),
        item: question.clone(),
        text: "ship it".to_string(),
    });

    // Intents are answered in order, so the reply is in once the next is answered.
    shell.intend(Intent::Refresh);
    let read = shell
        .until(|update| matches!(update, Update::Background { .. }))
        .await;
    assert!(
        !read
            .iter()
            .any(|update| matches!(update, Update::Notice(_))),
        "{read:?}"
    );
    let query = r#"{ AgentRequest(filter: { session_id: { _eq: "s1" } }) { content caused_by_source_doc_id } }"#;
    let requests = access.execute(query).await.unwrap();
    let requests = requests["data"]["AgentRequest"].as_array().unwrap().clone();
    assert_eq!(
        requests,
        [json!({ "content": "ship it", "caused_by_source_doc_id": question })]
    );
    node.shutdown().await.expect("node shutdown");
}

/// The mailbox subscription is the only reason to read the prompts again.
#[tokio::test]
async fn a_mailbox_change_rereads_the_open_sessions_prompts() {
    let asked_now = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let asking = asked_now.clone();
    let fake = with_mailbox(move |query| {
        let items = match asking.load(std::sync::atomic::Ordering::SeqCst) {
            true => vec![mailbox_item("i1", "s1", "gate", "ack", OWNER, 1)],
            false => Vec::new(),
        };
        mailbox_answer(query, &items)
    })
    .await;
    let shell = open_shell(fake.work());
    shell.ready().await;
    open(&shell, "s1");
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
    let shell = open_shell(fake.work());
    shell.ready().await;
    open(&shell, "s1");
    fake.posted(MAILBOX_LIST, 1).await;

    fake.end_streams(is_work);

    let not_live = |update: &Update| matches!(update, Update::Notice(notice) if notice.starts_with("prompts are not live"));
    shell.until(not_live).await;
    turn_changed(&fake);
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
        mailbox_item("i2", "s1", "ask", "ack", OWNER, 2),
        mailbox_item("i1", "s1", "finished", "ack", OWNER, 1),
    ];
    let fake = with_mailbox(move |query| mailbox_answer(query, &items)).await;
    let shell = open_shell(fake.work());
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
    let off: Result<Work, String> = Err("no runtime recorded".to_string());
    let home = tempfile::tempdir().unwrap();

    let mailbox = super::menus::read_menu(&off, home.path(), OWNER, MenuId::Mailbox).await;
    let installed = super::menus::read_menu(&off, home.path(), OWNER, MenuId::Workflows).await;

    assert_eq!(mailbox, Err("no runtime recorded".to_string()));
    assert_eq!(installed, Ok(Vec::new()));
}

fn dismiss_from_the_mailbox(item: &str) -> Intent {
    Intent::Act {
        menu: MenuId::Mailbox,
        act: MenuAct::Dismiss {
            item: item.to_string(),
        },
    }
}

/// One open item of `kind` and `action` in session `s1` of the served node, titled `title`,
/// filed by its agent `engineer` for `work`'s User, as the runtime files one. Returns its id.
async fn filed(
    node: &Arc<defra_node::Node>,
    work: &Work,
    title: &str,
    kind: gents_documents::mailbox_write_decl::MailboxKind,
    action: gents_documents::mailbox_write_decl::MailboxAction,
) -> String {
    use gents_session::session::mailbox::{
        FileMailboxItemArgs, MailboxSourceKind, MailboxStampContext,
    };
    let context = MailboxStampContext {
        requester_did: work.access().actor_did().unwrap(),
        node_did: OWNER.to_string(),
        agent_did: did("engineer"),
        session_id: Some("s1".to_string()),
    };
    let args = FileMailboxItemArgs {
        kind,
        action,
        title: title.to_string(),
        summary: None,
        payload: None,
        source_kind: MailboxSourceKind::Session,
        source_id: "s1".to_string(),
        session_id: Some("s1".to_string()),
        request_id: None,
        cause_doc_id: None,
        expected_collection: None,
        parent_item_id: None,
        deadline_at: None,
    };
    let root = crate::caused_sessions::fixtures::fixture_signer();
    let binding = crate::shared::test_support::fixture_agent_named("engineer");
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let actor = ::identity::Did::new(agent.did()).unwrap();
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        actor,
        agent.as_ref(),
    );
    let access = access.unwrap();
    // The runtime projects an Agent's baseline from its live document; no runtime runs here.
    let runtime = ::identity::Did::new(OWNER).unwrap();
    let runtime = gents_store::config_client::ConfigAccess::local_as(node.clone(), runtime);
    let baseline = gents_store::identity::GrantSet::AgentBaseline;
    let agent_did = ::identity::Did::new(agent.did()).unwrap();
    baseline.grant(&runtime, &agent_did).await.unwrap();
    let close = gents_session::session::mailbox::MAILBOX_CLOSE_COLLECTIONS;
    let item = gents_session::session::mailbox::stamp_create_with_access(
        node, &access, &context, args, close,
    );
    item.await.unwrap().doc_id
}

/// The shell on `node`, served as `served` serves one, once its runtime says its agent
/// `engineer` is ready.
async fn shell_on_served(node: &Arc<defra_node::Node>, work: Work) -> Shell {
    let runtime = ConfigAccess::local_as(node.clone(), ::identity::Did::new(OWNER).unwrap());
    publish_ready(&runtime, &["engineer"]).await;
    let shell = open_shell(work);
    shell.opened_as(&[&did("engineer")]).await;
    let update = shell.next().await;
    assert!(matches!(update, Update::Jobs { .. }), "{update:?}");
    let update = shell.next().await;
    assert!(matches!(update, Update::Background { .. }), "{update:?}");
    shell
}

#[tokio::test]
async fn an_act_notices_and_rereads_its_menu() {
    use gents_documents::mailbox_write_decl::{MailboxAction, MailboxKind};
    let (node, work, _home) = served_settings().await;
    let item = filed(
        &node,
        &work,
        "i1 title",
        MailboxKind::Finished,
        MailboxAction::Ack,
    )
    .await;
    let access = work.access();
    let shell = shell_on_served(&node, work).await;

    shell.intend(dismiss_from_the_mailbox(&item));

    let reread = Update::Menu {
        menu: MenuId::Mailbox,
        rows: Ok(Vec::new()),
    };
    assert_eq!(
        shell.until(is_menu).await,
        [Update::Notice("dismissed".to_string()), reread]
    );
    let stored =
        gents_session::session::mailbox::load_mailbox_item_with_access(&access, &item).await;
    assert_eq!(stored.unwrap().unwrap().status, "dismissed");
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_refused_act_notices_why_and_rereads() {
    let fake = with_mailbox(|query| {
        let mut question = mailbox_item("i1", "s1", "ask", "start_request", OWNER, 1);
        question["payload"] = json!(r#"{"version":2}"#);
        mailbox_answer(query, &[question])
    })
    .await;
    let shell = open_shell(fake.work());
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

fn read_tree() -> Intent {
    Intent::WorkingTree { agent: did("a") }
}

fn is_tree(update: &Update) -> bool {
    matches!(update, Update::WorkingTree(_))
}

fn on_tree(node: &ChatNode, tree: &std::path::Path) -> Shell {
    let launch = Launch {
        tree: Some(tree.into()),
        ..Default::default()
    };
    open_shell_with(Ok(node.work()), launch)
}

#[tokio::test]
async fn the_working_tree_is_read_from_git_in_the_tool_root() {
    let repo = repository();
    std::fs::write(repo.path().join("a.rs"), "a\n").unwrap();
    git(repo.path(), &["add", "a.rs"]);
    git(repo.path(), &["commit", "-m", "a"]);
    std::fs::write(repo.path().join("a.rs"), "b\n").unwrap();
    let node = ChatNode::new().await;
    let shell = on_tree(&node, repo.path());
    shell.ready().await;

    shell.intend(read_tree());

    let read = shell.until(is_tree).await;
    let edited = Change::new(
        String::new(),
        "a.rs".to_string(),
        ChangeKind::Update,
        "@@ -1 +1 @@\n-a\n+b\n".to_string(),
    );
    assert_eq!(read.last(), Some(&Update::WorkingTree(Ok(vec![edited]))));
}

#[tokio::test]
async fn a_refused_working_tree_read_says_why() {
    let plain = tempfile::tempdir().unwrap();
    let node = ChatNode::new().await;
    let shell = on_tree(&node, plain.path());
    shell.ready().await;

    shell.intend(read_tree());
    let read = shell.until(is_tree).await;
    let Some(Update::WorkingTree(Err(why))) = read.last() else {
        panic!("{read:?}");
    };
    assert!(
        why.starts_with("git rev-parse --show-toplevel failed: "),
        "{why}"
    );

    let shell = open_shell(node.work());
    shell.ready().await;
    shell.intend(read_tree());
    let read = shell.until(is_tree).await;
    let unread = Update::WorkingTree(Err("no working directory to read".to_string()));
    assert_eq!(read.last(), Some(&unread));
}

/// A repository whose git waits on its config until `release` lets it go: the config
/// includes a named pipe, which git blocks opening until a writer opens it.
#[cfg(unix)]
struct Held {
    repo: tempfile::TempDir,
    /// Outside the repository, which would list it as an untracked file and read it.
    _pipes: tempfile::TempDir,
    pipe: std::path::PathBuf,
    config: String,
}

#[cfg(unix)]
impl Held {
    fn new() -> Held {
        let repo = repository();
        let pipes = tempfile::tempdir().unwrap();
        let pipe = pipes.path().join("held");
        let made = std::process::Command::new("mkfifo").arg(&pipe).status();
        assert!(made.unwrap().success());
        let config = repo.path().join(".git/config");
        let original = std::fs::read_to_string(&config).unwrap();
        let held = format!("{original}[include]\n\tpath = {}\n", pipe.display());
        std::fs::write(&config, held).unwrap();
        Held {
            repo,
            _pipes: pipes,
            pipe,
            config: original,
        }
    }

    /// Waits until git has opened the pipe, restores the config so no later git waits, then
    /// ends the pipe empty, so the waiting git reads on.
    async fn release(&self) {
        let (pipe, repo) = (self.pipe.clone(), self.repo.path().to_path_buf());
        let config = self.config.clone();
        let released = test_runtime_handle().spawn_blocking(move || {
            let writer = std::fs::OpenOptions::new().write(true).open(&pipe).unwrap();
            std::fs::write(repo.join(".git/config"), config).unwrap();
            drop(writer);
        });
        within(released).await.unwrap();
    }
}

/// A read waits on git, never the chat: another intent is answered meanwhile.
#[cfg(unix)]
#[tokio::test]
async fn a_working_tree_read_holds_up_no_other_intent() {
    let held = Held::new();
    let node = ChatNode::new().await;
    let shell = on_tree(&node, held.repo.path());
    shell.ready().await;

    shell.intend(read_tree());
    shell.intend(Intent::Refresh);

    let update = shell.next().await;
    assert!(matches!(update, Update::Jobs { .. }), "{update:?}");
    held.release().await;
    shell.until(is_tree).await;
}

/// The next two asked for while the first is out become one read, so the older answer never
/// lands after the newer one.
#[cfg(unix)]
#[tokio::test]
async fn working_tree_reads_land_in_order() {
    let held = Held::new();
    let node = ChatNode::new().await;
    let shell = on_tree(&node, held.repo.path());
    shell.ready().await;

    for _ in 0..3 {
        shell.intend(read_tree());
    }
    held.release().await;

    let first = shell.until(is_tree).await;
    assert_eq!(first.last(), Some(&Update::WorkingTree(Ok(Vec::new()))));
    let second = shell.until(is_tree).await;
    assert_eq!(second.last(), Some(&Update::WorkingTree(Ok(Vec::new()))));
    assert_eq!(
        shell.quiet().await,
        None,
        "the two asked meanwhile are read once"
    );
}
fn node_settings() -> Value {
    node_settings_of(OWNER)
}

fn node_settings_of(did: &str) -> Value {
    json!({
        "node": { "node_did": did, "default_agent_did": "engineer" },
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
            "agent_did": "engineer",
            "display_name": "Engineer",
            "inference_profile_id": "profile",
        }],
        "tasks": [{
            "node_did": did,
            "task_id": "triage",
            "display_name": "Triage",
            "agent_did": "engineer",
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
async fn served_settings() -> (Arc<defra_node::Node>, Work, tempfile::TempDir) {
    served(node_settings()).await
}

/// A node served with `config`, and the shell's work on it as the User who owns it.
async fn served(config: Value) -> (Arc<defra_node::Node>, Work, tempfile::TempDir) {
    let served = crate::shared::test_support::served_config(config).await;
    let home = tempfile::tempdir().unwrap();
    let work = Work {
        endpoint: served.user.clone(),
        home: home.path().into(),
    };
    (served.node.clone(), work, home)
}

async fn menu_texts(work: &Work, menu: MenuId) -> Vec<String> {
    let rows = super::menus::read_menu(&Ok(work.clone()), &work.home, OWNER, menu).await;
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

    let turned = super::menus::act(&work, home.path(), OWNER, &off).await;

    assert_eq!(turned, Ok("Review is off".to_string()));
    assert_eq!(
        menu_texts(&work, MenuId::Skills).await,
        [
            " + New skill",
            " ● lint   0 tools",
            " ○ Review   2 tools · disabled"
        ]
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_skill_copy_is_renamed_as_the_desktop_does() {
    let (node, work, home) = served_settings().await;
    let duplicate = |id: &str| MenuAct::Duplicate(Doc::Skill, id.to_string());

    let named = super::menus::act(&work, home.path(), OWNER, &duplicate("review")).await;
    let bare = super::menus::act(&work, home.path(), OWNER, &duplicate("lint")).await;

    assert_eq!(named, Ok("Duplicated Review".to_string()));
    assert_eq!(bare, Ok("Duplicated lint".to_string()));
    let skills = query_collection_sorted(&work.access(), SKILL_SPEC, OWNER).await;
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_duplicate_is_a_new_document_off() {
    let (node, work, home) = served_settings().await;
    let duplicate = MenuAct::Duplicate(Doc::Trigger, "nightly".to_string());

    let duplicated = super::menus::act(&work, home.path(), OWNER, &duplicate).await;

    assert_eq!(duplicated, Ok("Duplicated Nightly".to_string()));
    let work = Ok(work);
    let rows = super::menus::read_menu(&work, home.path(), OWNER, MenuId::Triggers).await;
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_trigger_reads_its_own_nodes_schedule() {
    let (node, work, _home) = served_settings().await;
    let keys = tempfile::tempdir().unwrap();
    let other =
        gents_store::identity::KeyIdentity::load_or_create(keys.path().join("other.key"), None)
            .unwrap();
    let other = gents_store::identity::NodeIdentity::did(&other).to_string();
    let theirs = json!({
        "node": { "node_did": other },
        "schedules": [{
            "node_did": other,
            "schedule_id": "daily",
            "cadence": { "kind": "interval", "interval_secs": 60 },
        }],
    });
    let plan = gents_documents::DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value(theirs).unwrap(),
    )
    .unwrap();
    work.access()
        .transact_write("test.other_node", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let theirs = query_collection_sorted(&work.access(), SCHEDULE_SPEC, &other).await;
    assert_eq!(theirs.unwrap().len(), 1);

    assert_eq!(
        menu_texts(&work, MenuId::Triggers).await,
        [
            " + New trigger",
            " ● Nightly   Every day at 02:00 · run Triage with Engineer"
        ]
    );
    node.shutdown().await.expect("node shutdown");
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

    let refused = super::menus::act(&work, home.path(), OWNER, &daily).await;

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
    let trigger = super::menus::act(&work, home.path(), OWNER, &nightly).await;
    let schedule = super::menus::act(&work, home.path(), OWNER, &daily).await;
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
    node.shutdown().await.expect("node shutdown");
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
    let grouped = super::forms::open(&access, OWNER, &grouping, "evsrc-a").await;
    let grouped = with(
        as_opened(&grouped.unwrap()),
        "correlation_field",
        "session_id",
    );
    let grouped = with(grouped, "expected_count", "2");
    let grouped = with(grouped, "timeout_secs", "60");
    let grouped = with(grouped, "min_count", "3");
    let cron = super::forms::open(&access, OWNER, &cadence, "sched-a").await;
    let cron = with(as_opened(&cron.unwrap()), "expression", "every day");

    let over = super::forms::save(&access, &work.home, OWNER, &grouping, &grouped, "evsrc-a").await;
    let bad = super::forms::save(&access, &work.home, OWNER, &cadence, &cron, "sched-a").await;

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
    let owner = gents_documents::schedule_cron::validate_cron_schedule("every day", "UTC", None);
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn an_edit_keeps_what_the_form_does_not_show() {
    let mut config = node_settings();
    config["contexts"] = json!([
        { "node_did": OWNER, "context_id": "main", "skill_ids": ["commit"] },
    ]);
    config["agents"][0]["context_id"] = json!("main");
    config["skills"].as_array_mut().unwrap().push(json!({
        "node_did": OWNER,
        "skill_id": "commit",
        "name": "commit",
        "display_name": "Commit",
        "instructions": "Stage, then commit.",
        "created_at": "2026-01-01T00:00:00Z",
        "tags": ["gents:pack:acme/git", "vcs"],
    }));
    let (node, work, _home) = served(config).await;
    let access = work.access();
    let commit = FormRef::Edit(Doc::Skill, "commit".to_string());

    let form = super::forms::open(&access, OWNER, &commit, "commit").await;
    let form = form.unwrap();
    let values = with(as_opened(&form), "display_name", "Commit changes");
    let values = with(values, "tags", "vcs, daily");
    let saved = super::forms::save(&access, &work.home, OWNER, &commit, &values, "commit").await;

    assert_eq!(form.note, ["Used by Engineer · From acme/git"]);
    assert_eq!(saved, Ok("Saved".to_string()));
    let stored = crate::commands::config::crud::load_one(&access, SKILL_SPEC, OWNER, "commit");
    let stored = stored.await.unwrap();
    assert_eq!(stored["display_name"], json!("Commit changes"));
    assert_eq!(stored["instructions"], json!("Stage, then commit."));
    assert_eq!(
        stored["tags"],
        json!(["gents:pack:acme/git", "vcs", "daily"])
    );
    assert_eq!(stored["created_at"], json!("2026-01-01T00:00:00Z"));
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn new_then_nothing_writes_nothing() {
    let (node, work, _home) = served_settings().await;
    let access = work.access();
    let skills = || query_collection_sorted(&access, SKILL_SPEC, OWNER);
    let before = skills().await.unwrap();

    let new = FormRef::New(Doc::Skill);
    let form = super::forms::open(&access, OWNER, &new, "skill-new").await;

    assert_eq!(form.unwrap().title, "New skill");
    assert_eq!(skills().await.unwrap(), before);
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_new_form_saves_under_the_id_it_showed() {
    let (node, work, _home) = served(node_settings()).await;
    let access = work.access();
    let shell = shell_on_served(&node, work).await;
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
    let skills = query_collection_sorted(&access, SKILL_SPEC, OWNER).await;
    let drafts = skills
        .unwrap()
        .into_iter()
        .find(|skill| skill["skill_id"] == *id);
    let drafts = drafts.expect("the skill saved under the id its form showed");
    assert_eq!(
        (&drafts["name"], &drafts["enabled"]),
        (&json!("drafts"), &json!(false))
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_schedule_runs_now_through_its_trigger() {
    let did = OWNER.to_string();
    let mut config = node_settings_of(&did);
    config["schedules"].as_array_mut().unwrap().push(json!({
        "node_did": did,
        "schedule_id": "weekly",
        "cadence": { "kind": "cron", "expression": "0 9 * * 1", "timezone": "UTC" },
    }));
    let (node, served, _) = served(config).await;
    let (home, _) = home_serving(served.endpoint.url(), None);
    let work = Work {
        home: home.path().into(),
        ..served
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
        gents_store::graphql::escape_graphql_string(request)
    );
    let written = access.execute(&query).await.unwrap();
    let fired = &written["data"]["AgentRequest"][0];
    let task = fired["retry_key"].as_str().unwrap_or_default();
    assert_eq!(fired["caused_by_trigger_kind"], "manual", "{written}");
    assert!(task.contains(":manual:triage:"), "{written}");
    let no_trigger = "Couldn’t run the schedule: schedule weekly has no enabled Trigger";
    assert_eq!(refused, (no_trigger.to_string(), Tone::Warn));
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_new_trigger_on_a_preset_writes_schedule_task_and_trigger() {
    let (node, work, _home) = served_settings().await;
    let access = work.access();
    let new = FormRef::New(Doc::Trigger);

    let form = super::forms::open(&access, OWNER, &new, "trig-abc").await;
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
    let save = |values| super::forms::save(&access, &work.home, OWNER, &new, values, "trig-abc");
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
    let owner = gents_documents::schedule_cron::validate_cron_schedule("every day", &zone, None);
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
    let stored = |spec, id| load_one(&access, spec, OWNER, id);
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
            .map(|task| shown(task, &["display_name", "agent_did", "prompt_template"]))
            .collect::<Vec<_>>(),
        [[
            json!("Summarise the inbox."),
            json!(did("engineer")),
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_schedule_trigger_refuses_a_concurrency_it_cannot_honour() {
    let (node, work, _home) = served_settings().await;
    let access = work.access();
    let nightly = FormRef::Edit(Doc::Trigger, "nightly".to_string());
    let form = super::forms::open(&access, OWNER, &nightly, "nightly").await;
    let opened = as_opened(&form.unwrap());
    let save = |concurrency: &str| {
        let values = with(opened.clone(), "concurrency", concurrency);
        let (access, home, nightly) = (&access, &work.home, &nightly);
        async move { super::forms::save(access, home, OWNER, nightly, &values, "nightly").await }
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
    let stored = load_one(&access, TRIGGER_SPEC, OWNER, "nightly").await;
    assert_eq!(stored.unwrap()["concurrency"], json!("serial"));
    node.shutdown().await.expect("node shutdown");
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
    let form = super::forms::open(&access, OWNER, &new, "mcp-a").await;
    let no_host = with(as_opened(&form.unwrap()), "hostname", " ");
    let save = |values| super::forms::save(&access, home.path(), OWNER, &new, values, "mcp-a");

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
    let services = query_collection_sorted(&access, MCP_SPEC, OWNER)
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn test_connection_uses_the_forms_values() {
    let home = tempfile::tempdir().unwrap();
    let access = offline(home.path()).access();
    let new = FormRef::New(Doc::RemoteTool);
    let form = super::forms::open(&access, OWNER, &new, "mcp-a").await;
    let opened = as_opened(&form.unwrap());
    let test = gents_shell::source::FormAct::TestConnection;
    let tested =
        |values| super::forms::act(&access, home.path(), OWNER, &new, &test, values, "mcp-a");
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
        "node_did": OWNER,
        "display_name": "Studio",
        "avatar": "7",
        "default_agent_did": "engineer",
        "created_at": "2026-01-01T00:00:00Z",
        "created_by": "did:key:zMaker",
        "tags": ["gents:pack:home", "desk"],
    });
    let (node, work, home) = served(config).await;
    let access = work.access();
    let stored = || async {
        let nodes = query_collection(&access, NODE_SPEC, OWNER, None).await;
        nodes.unwrap().remove(0)
    };
    let before = stored().await;

    let form = super::forms::open(&access, OWNER, &FormRef::Node, "").await;
    let form = form.unwrap();
    let values = with(as_opened(&form), "display_name", " Workshop ");
    let values = with(values, "tags", "desk, bench");
    let saved = super::forms::save(&access, home.path(), OWNER, &FormRef::Node, &values, "").await;

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
        "default_agent_did",
        "enabled",
    ] {
        assert_eq!(after[kept], before[kept], "{kept}");
    }
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn changing_the_default_agent_enables_it() {
    let mut config = node_settings();
    config["agents"].as_array_mut().unwrap().push(json!({
        "node_did": OWNER,
        "agent_did": "scout",
        "display_name": "Scout",
        "inference_profile_id": "profile",
        "enabled": false,
    }));
    let (node, work, home) = served(config).await;
    let access = work.access();
    let scout = || async {
        let state = gents_desktop_core::inference::load_inference_setup_state(&access, OWNER);
        let agents = state.await.unwrap().agents;
        let scout = agents
            .into_iter()
            .find(|agent| agent.agent_did == did("scout"));
        let scout = scout.unwrap();
        (scout.enabled, scout.is_default)
    };
    let before = scout().await;

    let form = super::forms::open(&access, OWNER, &FormRef::Node, "").await;
    let form = form.unwrap();
    let values = with(as_opened(&form), "default_agent_did", &did("scout"));
    let values = with(values, "display_name", "Studio");
    let saved = super::forms::save(&access, home.path(), OWNER, &FormRef::Node, &values, "").await;

    let default = form
        .fields
        .iter()
        .find(|field| field.key == "default_agent_did");
    let gents_shell::source::FieldKind::Choice(agents) = &default.unwrap().kind else {
        panic!("{default:?}");
    };
    let scout_label = (
        did("scout"),
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn folder_rows_toggle_and_remove() {
    use gents_pack::pack::BindAccess;
    use gents_pack::plugin::allowed;
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
        super::menus::read_menu(&off, home.path(), OWNER, MenuId::AllowedFolders).await
    };
    let (work, at) = (&work, home.path());
    let act = |act: MenuAct| async move { super::menus::act(work, at, OWNER, &act).await };
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
    use gents_pack::pack::BindAccess;
    use gents_pack::plugin::allowed;
    let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let access = offline(home.path()).access();
    let add = FormRef::AddFolder;
    let typed = |path: String| vec![("path".to_string(), path)];

    let form = super::forms::open(&access, OWNER, &add, "").await.unwrap();
    let saved = super::forms::save(
        &access,
        home.path(),
        OWNER,
        &add,
        &typed(format!(" {} ", folder.path().display())),
        "",
    )
    .await;
    let missing = folder.path().join("missing");
    let refused = super::forms::save(
        &access,
        home.path(),
        OWNER,
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
) -> gents_documents::InferenceProfile {
    let mut profile = json!({
        "node_did": OWNER, "profile_id": id, "backend_id": "backend", "model_name": model,
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
            "node_did": OWNER, "profile_id": "other", "backend_id": "backend",
            "model_name": "other-model", "display_name": "Other",
        }));
    config["agents"].as_array_mut().unwrap().push(json!({
        "node_did": OWNER, "agent_did": "scout", "display_name": "Scout",
        "inference_profile_id": "profile",
    }));
    config
}

async fn profile_rows(work: &Work, agent: &str) -> Vec<gents_shell::source::MenuRow> {
    super::menus::read_profiles(&Ok(work.clone()), OWNER, agent)
        .await
        .unwrap()
}

#[tokio::test]
async fn choosing_a_profile_moves_only_that_agent() {
    let (node, work, home) = served(two_profiles()).await;
    let rows = super::menus::read_menu(&Ok(work.clone()), &work.home, OWNER, MenuId::Agents).await;
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
    assert_eq!(rows[0].more, choose(&did("engineer"), "Engineer"));
    assert_eq!(rows[1].more, choose(&did("scout"), "Scout"));
    let engineer = profile_rows(&work, &did("engineer")).await;
    assert_eq!(
        shown_rows(&engineer),
        [
            ("   Other · other-model", Tone::Plain),
            (" ● profile · model", Tone::Accent)
        ]
    );
    let choose = profile_rows(&work, &did("scout"))
        .await
        .remove(0)
        .enter
        .unwrap();

    let done = super::menus::act(&work, home.path(), OWNER, &choose).await;

    assert_eq!(done, Ok("Saved".to_string()));
    let scout = profile_rows(&work, &did("scout")).await;
    assert_eq!(
        shown_rows(&scout),
        [
            (" ● Other · other-model", Tone::Accent),
            ("   profile · model", Tone::Plain)
        ]
    );
    assert_eq!(profile_rows(&work, &did("engineer")).await, engineer);
    node.shutdown().await.expect("node shutdown");
}

#[test]
fn the_key_set_survives_a_restart() {
    use super::prefs::{read_keys, save_keys};
    let home = tempfile::tempdir().unwrap();
    assert_eq!(read_keys(home.path()), KeySet::Regular);
    for set in [KeySet::Vim, KeySet::Emacs, KeySet::Regular] {
        save_keys(home.path(), set).unwrap();
        assert_eq!(read_keys(home.path()), set);
    }
    std::fs::write(home.path().join("shell.json"), "garbage").unwrap();
    assert_eq!(read_keys(home.path()), KeySet::Regular);
}

#[tokio::test]
async fn a_failed_key_save_says_so() {
    let node = ChatNode::new().await;
    let missing = tempfile::tempdir().unwrap().path().join("gone");
    let launch = Launch {
        home: Some(missing),
        ..Default::default()
    };
    let shell = open_shell_with(Ok(node.work()), launch);
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
    let mut config = node_settings_of("did:key:z");
    config["inference_profiles"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "node_did": "did:key:z", "profile_id": "other", "backend_id": "backend",
            "model_name": "other-model", "display_name": "Other",
        }));
    let (node, work, _home) = served(config).await;
    let shell = shell_on_served(&node, work.clone()).await;
    let choose = MenuAct::SetAgentProfile {
        agent: did("engineer"),
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
    node.shutdown().await.expect("node shutdown");
}

/// A registry search's answer.
fn found(packs: Value, has_more: bool) -> Value {
    json!({ "packs": packs, "has_more": has_more })
}

#[test]
fn pack_rows_mirror_the_desktop() {
    use super::packs::{adds, pack_rows};
    use Tone::{Label, Plain, Warn};
    let git = || vec!["gents:pack:acme/git".to_string()];
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
    crate::shared::test_support::cli_user().await;
    let work = work::work_for(home.path());
    assert!(
        work.is_ok(),
        "this CLI's User signs: {:?}",
        work.as_ref().err()
    );
    let off: Result<Work, String> = Err("no runtime recorded".to_string());
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
    use gents_pack::pack::SlotBindingError;
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
