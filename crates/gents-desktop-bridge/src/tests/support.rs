use std::sync::Arc;

use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_protocol::row::AgentRequestRow;
use tempfile::TempDir;

pub async fn boot_core() -> (Arc<ClientCore>, TempDir) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = DesktopPaths::from_root(tempdir.path());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core starts");
    (Arc::new(core), tempdir)
}

pub async fn seed_standalone_fixture() -> (Arc<ClientCore>, TempDir) {
    let (core, tmp) = boot_core().await;

    let mutation = r#"mutation {
        create_AgentRequest(input: { purpose: "normal",
            request_id: "req_solo",
            agent_did: "did:test:operator",
            behavior_id: "test-behavior",
            session_id: "sess_solo",
            content: "standalone fixture",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:00:00Z",
            retry_count: 0
        }) { _docID }
    }"#;

    let response = core.node().execute(mutation).await;
    assert!(
        !response.has_errors(),
        "seed standalone AgentRequest failed: {:?}",
        response.errors
    );

    (core, tmp)
}

pub const OPERATOR: &str = "did:test:operator";

/// Read counter over the sanctioned access owner, so tests can pin how many
/// database round trips a projection issues. Instrumentation only: it adds no
/// retry, authorization or filtering of its own.
pub struct CountingRead {
    inner: gents::config_client::ConfigAccess,
    queries: std::sync::atomic::AtomicUsize,
}

impl CountingRead {
    pub fn new(inner: gents::config_client::ConfigAccess) -> Self {
        Self {
            inner,
            queries: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn queries(&self) -> usize {
        self.queries.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl gents::config_client::ConfigRead for CountingRead {
    async fn execute_read(&self, document: &str) -> anyhow::Result<serde_json::Value> {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.execute_read(document).await
    }
}

/// `req_parent` in `sess_parent` made four agents-tool calls:
/// - `tc_start` (`agent_new`) started `sess_child` (`req_child`), then
///   `tc_message` (`agent_message`) messaged it again (`req_child_2`);
/// - `tc_peer` (`agent_new`) started `sess_peer` on another agent (`req_peer`);
/// - `tc_existing` (`agent_message`) messaged `sess_existing`, which the
///   person started (`req_existing_1`), as `req_existing_2`.
///
/// Every session's requester is the operator principal, as the runtime writes
/// a started session. Returns the core and `req_parent`'s document id.
pub async fn seed_provenance_fixture() -> (Arc<ClientCore>, TempDir, String) {
    let (core, tmp) = boot_core().await;
    let parent = create_request(
        &core,
        "req_parent",
        OPERATOR,
        "sess_parent",
        "processing",
        "2026-05-20T00:00:00Z",
        None,
    )
    .await;
    create_session(&core, OPERATOR, "sess_parent", None).await;
    create_request(
        &core,
        "req_existing_1",
        OPERATOR,
        "sess_existing",
        "completed",
        "2026-05-20T00:00:30Z",
        None,
    )
    .await;
    create_session(&core, OPERATOR, "sess_existing", None).await;
    create_session(&core, OPERATOR, "sess_unrelated", None).await;
    create_request(
        &core,
        "req_unrelated",
        OPERATOR,
        "sess_unrelated",
        "processing",
        "2026-05-20T00:06:00Z",
        None,
    )
    .await;

    for (call, tool, request_id, agent_did, session_id, state, created_at, started) in [
        (
            "tc_start",
            "agent_new",
            "req_child",
            OPERATOR,
            "sess_child",
            "completed",
            "2026-05-20T00:01:00Z",
            true,
        ),
        (
            "tc_message",
            "agent_message",
            "req_child_2",
            OPERATOR,
            "sess_child",
            "processing",
            "2026-05-20T00:02:00Z",
            false,
        ),
        (
            "tc_peer",
            "agent_new",
            "req_peer",
            "did:test:other",
            "sess_peer",
            "processing",
            "2026-05-20T00:03:00Z",
            true,
        ),
        (
            "tc_existing",
            "agent_message",
            "req_existing_2",
            OPERATOR,
            "sess_existing",
            "processing",
            "2026-05-20T00:04:00Z",
            false,
        ),
    ] {
        let call_doc = create_tool_call(&core, call, tool).await;
        create_request(
            &core,
            request_id,
            agent_did,
            session_id,
            state,
            created_at,
            Some((&parent, call, &call_doc)),
        )
        .await;
        if started {
            create_session(&core, agent_did, session_id, Some(&parent)).await;
        }
    }
    (core, tmp, parent)
}

/// A fleet-shaped provenance fixture for the snapshot starter join: two parent
/// requests in their own sessions, two children started by the first parent,
/// one by the second, one child whose stored parent names no stored request,
/// and one session with no provenance. Returns the core and both parent
/// request document ids.
pub async fn seed_fleet_starter_fixture() -> (Arc<ClientCore>, TempDir, String, String) {
    let (core, tmp) = boot_core().await;
    let parent_a = create_request(
        &core,
        "req_fleet_parent_a",
        OPERATOR,
        "sess_fleet_parent_a",
        "completed",
        "2026-05-20T00:00:00Z",
        None,
    )
    .await;
    create_session(&core, OPERATOR, "sess_fleet_parent_a", None).await;
    let parent_b = create_request(
        &core,
        "req_fleet_parent_b",
        OPERATOR,
        "sess_fleet_parent_b",
        "completed",
        "2026-05-20T00:00:10Z",
        None,
    )
    .await;
    create_session(&core, OPERATOR, "sess_fleet_parent_b", None).await;
    create_session(
        &core,
        OPERATOR,
        "sess_fleet_child_shared_1",
        Some(&parent_a),
    )
    .await;
    create_session(
        &core,
        OPERATOR,
        "sess_fleet_child_shared_2",
        Some(&parent_a),
    )
    .await;
    create_session(&core, OPERATOR, "sess_fleet_child_b", Some(&parent_b)).await;
    create_session(
        &core,
        OPERATOR,
        "sess_fleet_child_dangling",
        Some("doc_id_of_no_stored_request"),
    )
    .await;
    create_session(&core, OPERATOR, "sess_fleet_plain", None).await;
    (core, tmp, parent_a, parent_b)
}

async fn created_doc_id(core: &Arc<ClientCore>, mutation: &str, collection: &str) -> String {
    let response = core.node().execute(mutation).await;
    assert!(
        !response.has_errors(),
        "seed {collection}: {:?}",
        response.errors
    );
    gents::graphql::single_mutation_document(&response, &format!("create_{collection}"))
        .expect("created document")
        .and_then(|document| document["_docID"].as_str())
        .expect("created document id")
        .to_owned()
}

/// Create one public request of the operator; `cause` is the causing
/// request's document id, and the causing call's logical and physical ids.
#[allow(clippy::too_many_arguments)]
async fn create_request(
    core: &Arc<ClientCore>,
    request_id: &str,
    agent_did: &str,
    session_id: &str,
    lifecycle_state: &str,
    created_at: &str,
    cause: Option<(&str, &str, &str)>,
) -> String {
    let caused = cause
        .map(|(doc, call, call_doc)| {
            format!(
                r#"subagent_depth: 1, caused_by_parent_request_id: "req_parent", caused_by_parent_request_doc_id: "{doc}", caused_by_parent_tool_call_id: "{call}", caused_by_parent_tool_call_doc_id: "{call_doc}","#
            )
        })
        .unwrap_or_default();
    created_doc_id(
        core,
        &format!(
            r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", agent_did: "{agent_did}", requester_did: "{OPERATOR}", behavior_id: "worker", session_id: "{session_id}", {caused} content: "work", lifecycle_state: "{lifecycle_state}", backend_id: "", created_at: "{created_at}", retry_count: 0 }}) {{ _docID }} }}"#
        ),
        "AgentRequest",
    )
    .await
}

async fn create_session(
    core: &Arc<ClientCore>,
    agent_did: &str,
    session_id: &str,
    started_by: Option<&str>,
) {
    let session = gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        agent_did: agent_did.into(),
        requester_did: Some(OPERATOR.into()),
        behavior_id: "worker".into(),
        created_at: "2026-05-20T00:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: started_by.map(|doc_id| gents_protocol::session::SessionProvenance {
            parent_request_doc_id: Some(doc_id.into()),
            ..Default::default()
        }),
        observation: None,
    };
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(&session).expect("session json"),
    )
    .expect("session input");
    created_doc_id(
        core,
        &format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
        "AgentSession",
    )
    .await;
}

async fn create_tool_call(core: &Arc<ClientCore>, call: &str, tool: &str) -> String {
    created_doc_id(
        core,
        &format!(
            r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "sess_parent:{call}", agent_did: "{OPERATOR}", requester_did: "{OPERATOR}", session_id: "sess_parent", request_id: "req_parent", message_sequence: 1, tool_name: "{tool}", tool_call_id: "{call}", args: "{{}}", result: "", status: "called", lifecycle_state: "running", await_mode: "background" }}) {{ _docID }} }}"#
        ),
        "AgentToolCall",
    )
    .await
}

pub async fn fetch_request_row(core: &Arc<ClientCore>, request_id: &str) -> AgentRequestRow {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                request_id
                interrupt_requested_at
            }}
        }}"#
    );

    let response = core.node().execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch_request_row query failed for {request_id}: {:?}",
        response.errors
    );

    let data = response.data.unwrap_or(serde_json::Value::Null);
    let row = data
        .get("AgentRequest")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .cloned()
        .unwrap_or_else(|| panic!("fetch_request_row: request {request_id} not found"));
    serde_json::from_value(row)
        .unwrap_or_else(|error| panic!("fetch_request_row: invalid request {request_id}: {error}"))
}
