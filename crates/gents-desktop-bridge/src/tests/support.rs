use std::sync::Arc;

use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_protocol::job::JobProgress;
use gents_protocol::row::AgentRequestRow;
use tempfile::TempDir;

/// A test's `#[tokio::test]` runtime is its entry point; code under test takes
/// that runtime's handle from here.
#[allow(
    clippy::disallowed_methods,
    reason = "a test's #[tokio::test] runtime is its entry-point runtime"
)]
#[cfg(feature = "tauri-host")]
pub fn runtime_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

pub fn node_builder() -> gents::defra_node::NodeBuilder {
    use gents::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    gents::defra_node::EmbeddedNode::builder().with_node_identity_did(identity.did())
}

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
            node_did: "did:test:operator",
            agent_id: "test-agent",
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

/// The DIDs of [`fixture_signer`]`(7)` and `(9)`; the sessions they start
/// carry receipts those keys signed.
pub const OPERATOR: &str = "did:key:z6MkvDqGT54cXesYGvABpF1UapVNwjCqRcafi4Px6Thv5T3Z";
pub const OTHER: &str = "did:key:z6MkwVDfCg9LbbY6xjH3EZk8YSFQZujV5Y4y1ZWeER9tDiN3";

/// A fixed test key, so fixture rows can name its DID as a constant.
pub fn fixture_signer(seed: u8) -> gents::KeyIdentity {
    use std::io::Write as _;
    // A named temporary file is created private to its owner, as a key must be.
    let mut key = tempfile::NamedTempFile::new().unwrap();
    key.write_all(&crypto::ed25519_key_from_seed(&[seed; 32]).unwrap())
        .unwrap();
    gents::KeyIdentity::load_existing(key.path(), None).unwrap()
}

/// Write `session` as creation writes a started session: the immutable
/// starter index at create, then the receipt `signer` signs for the created
/// physical document, binding `source`, `parent` and `opening`.
pub async fn create_started_session(
    core: &Arc<ClientCore>,
    signer: &gents::KeyIdentity,
    mut session: gents_protocol::session::AgentSession,
    source: &gents_protocol::session::SessionReference,
    parent: &str,
    opening: &str,
) {
    use gents::NodeIdentity as _;
    use gents_protocol::session::{
        SessionCreationCause, SessionCreationReceipt, SessionCreationStarter,
    };
    assert_eq!(
        session.node_did,
        signer.did(),
        "the session's node signs its receipt"
    );
    let mut provenance = session.provenance.take().unwrap_or_default();
    provenance.parent_request_doc_id = Some(parent.into());
    provenance.opening_request_doc_id = Some(opening.into());
    provenance.started_by = Some(source.clone());
    let mut input = serde_json::to_value(&gents_protocol::session::AgentSession {
        provenance: Some(provenance.clone()),
        ..session.clone()
    })
    .unwrap();
    input["started_by_node_did"] = source.node_did.clone().into();
    input["started_by_session_id"] = source.session_id.clone().into();
    let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let doc_id = created_doc_id(
        core,
        &format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
        "AgentSession",
    )
    .await;
    let mut receipt = SessionCreationReceipt {
        node_did: session.node_did.clone(),
        session_id: session.session_id.clone(),
        session_doc_id: doc_id,
        requester_did: session.requester_did.clone(),
        agent_id: session.agent_id.clone(),
        opening_request_doc_id: opening.into(),
        cause: SessionCreationCause::AgentNew,
        cause_binding: "fixture-call".into(),
        starter: Some(SessionCreationStarter {
            source: source.clone(),
            parent_request_doc_id: parent.into(),
        }),
        signature: Vec::new(),
    };
    receipt.signature = signer.sign(&receipt.signing_payload()).await.unwrap();
    provenance.creation_receipt = Some(receipt);
    write_provenance(core, &session.node_did, &session.session_id, &provenance).await;
}

pub async fn write_provenance(
    core: &Arc<ClientCore>,
    node_did: &str,
    session_id: &str,
    provenance: &gents_protocol::session::SessionProvenance,
) {
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::json!({ "provenance": provenance }),
    )
    .unwrap();
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{ update_AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}, input: {input}) {{ _docID }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(session_id),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

/// A fixture session on `node_did` with no provenance.
pub fn fixture_session(node_did: &str, session_id: &str) -> gents_protocol::session::AgentSession {
    gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: Some(OPERATOR.into()),
        agent_id: "worker".into(),
        created_at: "2026-05-20T00:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    }
}

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
    async fn execute_read(
        &self,
        document: &str,
    ) -> Result<serde_json::Value, gents::config_client::ConfigClientError> {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.execute_read(document).await
    }
}

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

    for (call, tool, request_id, node_did, session_id, state, created_at, started) in [
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
            OTHER,
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
        let opening = create_request(
            &core,
            request_id,
            node_did,
            session_id,
            state,
            created_at,
            Some((&parent, call, &call_doc)),
        )
        .await;
        if started {
            let signer = fixture_signer(if node_did == OTHER { 9 } else { 7 });
            create_started_session(
                &core,
                &signer,
                fixture_session(node_did, session_id),
                &gents_protocol::session::SessionReference {
                    node_did: OPERATOR.into(),
                    session_id: "sess_parent".into(),
                    requester_did: Some(OPERATOR.into()),
                },
                &parent,
                &opening,
            )
            .await;
        }
    }
    (core, tmp, parent)
}

pub async fn refresh_request_and_assert_fresh_projection(
    core: &ClientCore,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_id: &str,
) {
    let projection = core
        .refresh_local_request(node_did, session_id, Some(request_id), None, &[])
        .await
        .expect("refresh request");
    let projection = projection.unwrap_or_else(|| {
        panic!("request {request_id} has no fresh projection for node {node_did}")
    });
    assert_eq!(projection.requester_did.as_deref(), requester_did);
    assert!(
        projection.requests.iter().any(|row| {
            row.node_did.as_deref() == Some(node_did)
                && row.session_id.as_deref() == Some(session_id)
                && row.requester_did.as_deref() == requester_did
                && row.request_id == request_id
        }),
        "fresh request projection omitted {request_id} for node {node_did}"
    );
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

#[allow(clippy::too_many_arguments)]
pub async fn create_request(
    core: &Arc<ClientCore>,
    request_id: &str,
    node_did: &str,
    session_id: &str,
    lifecycle_state: &str,
    created_at: &str,
    cause: Option<(&str, &str, &str)>,
) -> String {
    let caused = cause
        .map(|(doc, call, call_doc)| {
            format!(
                r#"request_hop: 1, caused_by_parent_request_id: "req_parent", caused_by_parent_request_doc_id: "{doc}", caused_by_parent_tool_call_id: "{call}", caused_by_parent_tool_call_doc_id: "{call_doc}","#
            )
        })
        .unwrap_or_default();
    created_doc_id(
        core,
        &format!(
            r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "{node_did}", requester_did: "{OPERATOR}", agent_id: "worker", session_id: "{session_id}", {caused} content: "work", lifecycle_state: "{lifecycle_state}", backend_id: "", created_at: "{created_at}", retry_count: 0 }}) {{ _docID }} }}"#
        ),
        "AgentRequest",
    )
    .await
}

pub async fn create_session(
    core: &Arc<ClientCore>,
    node_did: &str,
    session_id: &str,
    started_by: Option<&str>,
) {
    let session = gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: Some(OPERATOR.into()),
        agent_id: "worker".into(),
        created_at: "2026-05-20T00:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
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

pub async fn create_tool_call(core: &Arc<ClientCore>, call: &str, tool: &str) -> String {
    created_doc_id(
        core,
        &format!(
            r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "sess_parent:{call}", node_did: "{OPERATOR}", requester_did: "{OPERATOR}", session_id: "sess_parent", request_id: "req_parent", message_sequence: 1, tool_name: "{tool}", tool_call_id: "{call}", status: "called", lifecycle_state: "running", await_mode: "background" }}) {{ _docID }} }}"#
        ),
        "AgentToolCall",
    )
    .await
}

/// Seed one plugin job row of a fresh request, optionally with the closed
/// terminal output a failed or timed-out job carries. Returns the row's `_docID`.
pub async fn seed_job_row(
    core: &Arc<ClientCore>,
    session_id: &str,
    tool_call_id: &str,
    lifecycle_state: &str,
    progress: &JobProgress,
    coded_output: Option<(&str, &str)>,
) -> String {
    seed_job_row_for_node(
        core,
        OPERATOR,
        session_id,
        tool_call_id,
        lifecycle_state,
        progress,
        coded_output,
    )
    .await
}

pub async fn seed_job_row_for_node(
    core: &Arc<ClientCore>,
    node_did: &str,
    session_id: &str,
    tool_call_id: &str,
    lifecycle_state: &str,
    progress: &JobProgress,
    coded_output: Option<(&str, &str)>,
) -> String {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::job::{JobCeiling, JobOrigin, JobPin};
    use gents_protocol::output::{
        OutputOutcome, OutputSegment, OutputSource, OutputWriter, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload,
    };

    let access = ConfigAccess::local(core.node_arc());
    let request_doc_id = create_request(
        core,
        &format!("req-{tool_call_id}"),
        node_did,
        session_id,
        "completed",
        "2026-05-20T00:00:00Z",
        None,
    )
    .await;
    let mut pin = JobPin {
        script_id: "fixture-script".into(),
        source_digest: "sha256:source".into(),
        origin: JobOrigin::Installed,
        engine_digest: "sha256:engine".into(),
        plugin_ref: "fixture/workflow".into(),
        agent_budget: 4,
        handle: tool_call_id.into(),
        cwd: "/fixture/cwd".into(),
        adopted_at: 1_780_000_000_000,
        args_digest: "sha256:args".into(),
        chain_root_doc_id: String::new(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: JobCeiling {
            targets: vec!["general".into()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    };
    let completed_at = if lifecycle_state == "running" {
        String::new()
    } else {
        r#"completed_at: "2026-05-20T00:05:00Z","#.to_owned()
    };
    let mutation = format!(
        r#"mutation($job_pin: JSON, $job_progress: JSON) {{ create_AgentToolCall(input: {{
            tool_call_key: "{key}", request_id: "req-{call}", request_doc_id: "{request_doc_id}",
            session_id: "{session}", node_did: "{node}", requester_did: "{OPERATOR}",
            tool_name: "workflow", tool_call_id: "{call}", message_sequence: 1,
            await_mode: "background", status: "{lifecycle}", lifecycle_state: "{lifecycle}",
            started_at: "2026-05-20T00:00:00Z", {completed_at}
            job_pin: $job_pin, job_progress: $job_progress
        }}) {{ _docID }} }}"#,
        node = escape_graphql_string(node_did),
        key = escape_graphql_string(&format!("{session_id}:{tool_call_id}")),
        request_doc_id = escape_graphql_string(&request_doc_id),
        session = escape_graphql_string(session_id),
        call = escape_graphql_string(tool_call_id),
        lifecycle = escape_graphql_string(lifecycle_state),
    );
    let variables = serde_json::json!({ "job_pin": pin, "job_progress": progress });
    let response = access
        .transact_write("test.fixture.job_row", |txn| {
            let (mutation, variables) = (mutation.clone(), variables.clone());
            Box::pin(async move { txn.execute_with_variables(&mutation, &variables).await })
        })
        .await
        .expect("seed job row");
    let row_doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentToolCall")
        .expect("job row doc id");
    pin.chain_root_doc_id = row_doc_id.clone();
    let update = format!(
        r#"mutation($job_pin: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ job_pin: $job_pin }}) {{ _docID }} }}"#,
        escape_graphql_string(&row_doc_id)
    );
    let variables = serde_json::json!({ "job_pin": pin });
    access
        .transact_write("test.fixture.job_pin", |txn| {
            let (update, variables) = (update.clone(), variables.clone());
            Box::pin(async move { txn.execute_with_variables(&update, &variables).await })
        })
        .await
        .expect("pin the job chain root");

    if let Some((code, message)) = coded_output {
        let payload = serde_json::json!({ "code": code, "message": message }).to_string();
        let segment = OutputSegment {
            node_did: node_did.into(),
            requester_did: Some(OPERATOR.into()),
            session_id: session_id.into(),
            request_doc_id,
            source: OutputSource::ToolCall {
                tool_call_doc_id: row_doc_id.clone(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: row_doc_id.clone(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: payload.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolOutput,
                }),
            }],
            payload: payload.clone(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![payload.len() as u64],
            }),
            created_at: "2026-05-20T00:05:00Z".into(),
        };
        let variables = output_segment_create_variables(&segment).expect("segment variables");
        access
            .transact_write("test.fixture.job_output", |txn| {
                let variables = variables.clone();
                Box::pin(async move {
                    txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables)
                        .await
                })
            })
            .await
            .expect("seed job output");
    }
    row_doc_id
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
