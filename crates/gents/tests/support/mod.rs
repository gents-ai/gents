#![allow(dead_code)]

use std::sync::Arc;

use anyhow::Context;
use gents::defra_node::{EmbeddedNode, P2PConfig, QueryResponse};
use gents::graphql::escape_graphql_string;
use gents::{ensure_runtime_schemas, watcher::AgentRequest, AgentIdentity, KeyIdentity};
use serde::Deserialize;
use tempfile::TempDir;

pub mod accepted_turn;
pub mod conformance_consumers;
pub mod enrollment;
pub mod fixtures;
pub mod http_mock;
pub(crate) mod identity_stubs;
pub mod interrupt;
pub mod live_inference;
pub mod mock_endpoint;
pub mod mock_subscription;
pub mod native_remote_spawn;
pub mod p2p_waits;
pub mod pairing_conformance;
pub mod r5_conformance;
pub mod r5_cross_principal_runtime;
pub mod snapshots;
pub mod streaming_backend;
pub mod waits;

pub const AGENT_DID: &str = "did:test:test";
pub const AGENT_NAME: &str = "test";
pub const BACKEND_ID: &str = "backend-test";
pub const DEADLINE_SECS: u64 = 300;

pub fn materialization_identity() -> Arc<dyn AgentIdentity> {
    materialization_identity_for(AGENT_DID)
}

pub fn materialization_identity_for(did: &str) -> Arc<dyn AgentIdentity> {
    identity_stubs::SigningStubAgentIdentity::arc(did)
}

pub struct TestDb {
    pub node: Arc<EmbeddedNode>,
    pub node_identity: Arc<dyn AgentIdentity>,
    pub process_generation: u64,
    node_identity_did: String,
    p2p_reopen: Option<TestP2pAdmission>,
    tempdir: TempDir,
}

impl TestDb {
    pub fn data_path(&self) -> &std::path::Path {
        self.tempdir.path()
    }

    pub async fn simulate_process_crash(&mut self) -> anyhow::Result<()> {
        let data_path = self.tempdir.path().to_path_buf();
        let before = self.process_generation;

        // Stopping the embedded process closes its subscriptions and other
        // node-owned services. Those tasks may retain Arc handles briefly
        // after the top-level agent task has been aborted, so wait for their
        // actual release before claiming an exclusive durable-store reopen.
        self.node.shutdown().await;
        let release_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while Arc::strong_count(&self.node) != 1 && tokio::time::Instant::now() < release_deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let strong = Arc::strong_count(&self.node);
        if strong != 1 {
            anyhow::bail!(
                "simulate_process_crash: stopped EmbeddedNode retained live owners \
                 (strong_count={strong}); crash boundary would not clear process state"
            );
        }

        let stand_in = Arc::new(
            EmbeddedNode::builder()
                .build()
                .await
                .map_err(|e| anyhow::anyhow!("simulate_process_crash: stand-in node: {e}"))?,
        );
        let old = std::mem::replace(&mut self.node, stand_in);

        match Arc::try_unwrap(old) {
            Ok(owned) => drop(owned),
            Err(shared) => {
                let count = Arc::strong_count(&shared);
                self.node = shared;
                anyhow::bail!(
                    "simulate_process_crash: cannot exclusively drop EmbeddedNode \
                     (strong_count={count} after replace); restored handle is shut \
                     down and unusable — fix outstanding Arc clones before Crash"
                );
            }
        }

        let mut reopen_builder = EmbeddedNode::builder()
            .data_path(&data_path)
            .with_node_identity_did(&self.node_identity_did);
        if let Some(admission) = &self.p2p_reopen {
            reopen_builder = reopen_builder.with_p2p(test_p2p_config(admission, &data_path));
        }
        let reopened = reopen_builder.build().await.map_err(|e| {
            anyhow::anyhow!(
                "simulate_process_crash: reopen durable store at {} failed: {e}",
                data_path.display()
            )
        })?;
        self.node = Arc::new(reopened);

        ensure_runtime_schemas(&self.node)
            .await
            .map_err(|e| anyhow::anyhow!("simulate_process_crash: ensure schemas: {e}"))?;

        self.process_generation = before + 1;
        Ok(())
    }
}

pub async fn test_db(name: &str) -> TestDb {
    let tempdir = tempfile::Builder::new()
        .prefix(&format!("gents-{name}-"))
        .tempdir()
        .expect("tempdir");
    test_db_in(tempdir).await
}

pub async fn test_db_in(tempdir: TempDir) -> TestDb {
    let node_identity: Arc<dyn AgentIdentity> = Arc::new(
        KeyIdentity::load_or_create(tempdir.path().join("node.key"), None).expect("node identity"),
    );
    let node_identity_did = node_identity.did().to_string();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(tempdir.path())
            .with_node_identity_did(&node_identity_did)
            .build()
            .await
            .expect("embedded node"),
    );
    ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");
    TestDb {
        node,
        node_identity,
        process_generation: 0,
        node_identity_did,
        p2p_reopen: None,
        tempdir,
    }
}

#[derive(Debug, Clone)]
pub struct TestP2pAdmission {
    pub max_concurrent_push_tasks: usize,
    pub max_concurrent_dag_fetches: usize,
    pub max_pending_dags: usize,
    pub rate_limit_burst: u32,
    pub rate_limit_rate: f64,
}

impl Default for TestP2pAdmission {
    fn default() -> Self {
        Self {
            max_concurrent_push_tasks: p2p::sync::DEFAULT_MAX_CONCURRENT_PUSH_TASKS,
            max_concurrent_dag_fetches: p2p::sync::DEFAULT_MAX_CONCURRENT_DAG_FETCHES,
            max_pending_dags: p2p::sync::DEFAULT_MAX_PENDING_DAGS,
            rate_limit_burst: p2p::sync::DEFAULT_RATE_LIMIT_BURST,
            rate_limit_rate: p2p::sync::DEFAULT_RATE_LIMIT_RATE,
        }
    }
}

impl TestP2pAdmission {
    pub fn single_push_worker() -> Self {
        Self {
            max_concurrent_push_tasks: 1,
            ..Self::default()
        }
    }
}

pub async fn test_p2p_db(name: &str) -> TestDb {
    test_p2p_db_with_admission(name, TestP2pAdmission::default()).await
}

pub async fn test_p2p_db_with_admission(name: &str, admission: TestP2pAdmission) -> TestDb {
    let tempdir = tempfile::Builder::new()
        .prefix(&format!("gents-{name}-"))
        .tempdir()
        .expect("tempdir");
    let node_identity: Arc<dyn AgentIdentity> = Arc::new(
        KeyIdentity::load_or_create(tempdir.path().join("node.key"), None).expect("node identity"),
    );
    let node_identity_did = node_identity.did().to_string();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(tempdir.path())
            .with_node_identity_did(&node_identity_did)
            .with_p2p(test_p2p_config(&admission, tempdir.path()))
            .build()
            .await
            .expect("embedded p2p node"),
    );
    ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");
    TestDb {
        node,
        node_identity,
        process_generation: 0,
        node_identity_did,
        p2p_reopen: Some(admission),
        tempdir,
    }
}

fn test_p2p_config(admission: &TestP2pAdmission, data_path: &std::path::Path) -> P2PConfig {
    P2PConfig {
        port: 0,
        bind_addr: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        relay_mode: p2p::iroh::IrohRelayModeConfig::Disabled,
        discovery: p2p::iroh::IrohDiscoveryConfig::Disabled,
        allowlist: p2p::iroh::IrohAllowlistConfig::AcceptAll,
        max_concurrent_multipath_paths: None,
        secret_key_path: Some(data_path.join("p2p.key")),
        load_persisted_collections: true,
        max_concurrent_dag_fetches: admission.max_concurrent_dag_fetches,
        max_concurrent_push_tasks: admission.max_concurrent_push_tasks,
        rate_limit_burst: admission.rate_limit_burst,
        rate_limit_rate: admission.rate_limit_rate,
        max_doc_sync_request_doc_ids: p2p::sync::DEFAULT_MAX_DOC_SYNC_REQUEST_DOC_IDS,
        max_pending_dags: admission.max_pending_dags,
        rebroadcast_on_merge: false,
    }
}

pub async fn create_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
) -> String {
    create_request_with_valid_until(node, request_id, session_id, status, created_at, None).await
}

pub async fn create_request_with_valid_until(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
    valid_until: Option<&str>,
) -> String {
    create_request_with_signed_fields(
        node,
        request_id,
        session_id,
        status,
        created_at,
        valid_until,
        None,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn create_request_with_signed_fields(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
    valid_until: Option<&str>,
    input: Option<&str>,
    retry_parent_request: Option<&str>,
    retry_root_request: Option<&str>,
) -> String {
    create_request_for_agent_with_signed_fields(
        node,
        AGENT_DID,
        request_id,
        session_id,
        status,
        created_at,
        valid_until,
        input,
        retry_parent_request,
        retry_root_request,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn create_request_for_agent_with_signed_fields(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    lifecycle_state: &str,
    created_at: &str,
    valid_until: Option<&str>,
    input: Option<&str>,
    retry_parent_request: Option<&str>,
    retry_root_request: Option<&str>,
) -> String {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let created_at = escape_graphql_string(created_at);
    let agent_did = escape_graphql_string(agent_did);
    let valid_until = valid_until
        .map(escape_graphql_string)
        .map(|value| format!(r#"valid_until: "{value}","#))
        .unwrap_or_default();
    let input = input
        .map(|value| serde_json::from_str::<serde_json::Value>(value).expect("request input JSON"))
        .map(|value| {
            gents_protocol::graphql::graphql_input_literal(&value).expect("request input GraphQL")
        })
        .map(|value| format!("input: {value},"))
        .unwrap_or_default();
    let retry_parent_request = retry_parent_request
        .map(escape_graphql_string)
        .unwrap_or_default();
    let retry_root_request = escape_graphql_string(retry_root_request.unwrap_or(&request_id));
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{session_id}",
                retry_parent_request: "{retry_parent_request}",
                retry_root_request: "{retry_root_request}",
                superseded_by_request: "",
                content: "hello",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                {valid_until}
                {input}
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: 0
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create request failed: {:?}",
        resp.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                _docID
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_row::<DocIdRow>(&resp, "AgentRequest").doc_id
}

/// Resolve a logical request id only at test setup boundaries, rejecting
/// ambiguous fixtures instead of silently selecting an arbitrary document.
pub async fn exact_request_doc_id(node: &EmbeddedNode, request_id: &str) -> String {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 2
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "exact request document lookup failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("AgentRequest rows");
    assert_eq!(
        rows.len(),
        1,
        "request id must resolve to exactly one document"
    );
    rows[0]["_docID"]
        .as_str()
        .expect("AgentRequest _docID")
        .to_string()
}

pub async fn create_retry_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    retry_parent_request: &str,
    retry_root_request: &str,
    content: &str,
    created_at: &str,
    valid_until: Option<&str>,
) -> String {
    let request_id_escaped = escape_graphql_string(request_id);
    let session_id_escaped = escape_graphql_string(session_id);
    let retry_parent_escaped = escape_graphql_string(retry_parent_request);
    let retry_root_escaped = escape_graphql_string(retry_root_request);
    let content_escaped = escape_graphql_string(content);
    let created_at_escaped = escape_graphql_string(created_at);
    let valid_until = valid_until
        .map(escape_graphql_string)
        .map(|value| format!(r#"valid_until: "{value}","#))
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id_escaped}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{session_id_escaped}",
                retry_parent_request: "{retry_parent_escaped}",
                retry_root_request: "{retry_root_escaped}",
                superseded_by_request: "",
                content: "{content_escaped}",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at_escaped}",
                {valid_until}
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create retry request failed: {:?}",
        resp.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id_escaped}" }} }}) {{
                _docID
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_row::<DocIdRow>(&resp, "AgentRequest").doc_id
}

pub async fn set_interrupt_requested_at(node: &EmbeddedNode, doc_id: &str, at: &str) {
    let doc_id = escape_graphql_string(doc_id);
    let at = escape_graphql_string(at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ interrupt_requested_at: "{at}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set_interrupt_requested_at failed: {:?}",
        resp.errors
    );
}

pub async fn set_request_lifecycle_state(node: &EmbeddedNode, doc_id: &str, lifecycle_state: &str) {
    let doc_id = escape_graphql_string(doc_id);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ lifecycle_state: "{lifecycle_state}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set_request_lifecycle_state failed: {:?}",
        resp.errors
    );
}

pub async fn try_set_valid_until(node: &EmbeddedNode, doc_id: &str, at: &str) -> QueryResponse {
    let doc_id = escape_graphql_string(doc_id);
    let at = escape_graphql_string(at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ valid_until: "{at}" }}
            ) {{ _docID }}
        }}"#
    );
    node.execute(&mutation).await
}

pub fn build_request(
    doc_id: String,
    request_id: String,
    session_id: String,
    created_at: String,
) -> AgentRequest {
    AgentRequest {
        doc_id,
        request_id,
        agent_did: AGENT_DID.into(),
        requester_did: None,
        behavior_id: AGENT_NAME.into(),
        session_id,
        content: "hello".into(),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: None,
        created_at,
        deadline: None,
        execution_generation: None,
        execution_lease_secs: None,
        execution_lease_expires_at: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_authority: None,
        workspace_owner_agent_did: None,
        workspace_seal_hash: None,
    }
}

/// Canonical target SDL fixture. This specifies the nested schema needed by the
/// runtime layer; it does not implement or claim to test a storage conversion.
pub async fn create_session_document(
    node: &EmbeddedNode,
    session: &gents_protocol::session::AgentSession,
) {
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(session).expect("serialize canonical AgentSession"),
    )
    .expect("render session fixture");
    let response = node
        .execute(&format!(
            "mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"
        ))
        .await;
    assert!(
        !response.has_errors(),
        "create canonical session: {:?}",
        response.errors
    );
}

/// Seed an observation from a real authoritative request row; this does not run
/// a transition or stand in for the runtime observation owner.
pub async fn seed_session_observation_from_request(
    node: &EmbeddedNode,
    session_id: &str,
    request_id: &str,
    preview: &str,
) {
    let session = escape_graphql_string(session_id);
    let request = escape_graphql_string(request_id);
    let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }}, request_id: {{ _eq: "{request}" }} }}, limit: 2) {{ _docID request_id lifecycle_state created_at }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response
        .data
        .as_ref()
        .and_then(|v| v.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("request rows");
    assert_eq!(
        rows.len(),
        1,
        "fixture needs an unambiguous physical request"
    );
    let row = &rows[0];
    let observation = gents_protocol::session::SessionObservation {
        last_activity_at: row["created_at"]
            .as_str()
            .expect("creation time")
            .to_owned(),
        preview: Some(preview.to_owned()),
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: row["_docID"].as_str().expect("physical request").to_owned(),
            request_id: request_id.to_owned(),
            lifecycle_state: serde_json::from_value(row["lifecycle_state"].clone())
                .expect("canonical lifecycle"),
        }),
    };
    seed_session_observation(node, session_id, &observation).await;
}

pub async fn seed_session_observation(
    node: &EmbeddedNode,
    session_id: &str,
    observation: &gents_protocol::session::SessionObservation,
) {
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::json!({"observation": observation}),
    )
    .expect("render session observation fixture");
    let session_id = escape_graphql_string(session_id);
    let response = node.execute(&format!(
        "mutation {{ update_AgentSession(filter: {{session_id: {{_eq: \"{session_id}\"}}}}, input: {input}) {{_docID}} }}"
    )).await;
    assert!(
        !response.has_errors(),
        "seed session observation: {:?}",
        response.errors
    );
}

pub fn session_document(
    session_id: &str,
    behavior_id: &str,
    created_at: &str,
) -> gents_protocol::session::AgentSession {
    gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        agent_did: AGENT_DID.into(),
        requester_did: None,
        behavior_id: behavior_id.into(),
        created_at: created_at.into(),
        closed_at: None,
        title: None,
        tags: vec![],
        provenance: None,
        observation: None,
    }
}

pub fn session_document_in_scope(
    agent_did: &str,
    session_id: &str,
    behavior_id: &str,
    created_at: &str,
) -> gents_protocol::session::AgentSession {
    gents_protocol::session::AgentSession {
        agent_did: agent_did.into(),
        ..session_document(session_id, behavior_id, created_at)
    }
}

pub async fn create_agent_session(
    node: &EmbeddedNode,
    session_id: &str,
    behavior_id: &str,
    created_at: &str,
) {
    create_session_document(node, &session_document(session_id, behavior_id, created_at)).await;
}

pub async fn create_agent_session_in_scope(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    behavior_id: &str,
    created_at: &str,
) {
    create_session_document(
        node,
        &session_document_in_scope(agent_did, session_id, behavior_id, created_at),
    )
    .await;
}

pub async fn create_agent_message(
    node: &EmbeddedNode,
    session_id: &str,
    sequence: u32,
    role: &str,
    content: &str,
    timestamp: &str,
) {
    create_agent_message_in_scope(
        node, AGENT_DID, None, session_id, sequence, role, content, timestamp,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn create_agent_message_in_scope(
    node: &EmbeddedNode,
    agent_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    sequence: u32,
    role: &str,
    content: &str,
    timestamp: &str,
) {
    use gents::defra_node::{ExecuteRetryPolicy, QueryRequest};
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let role = match role {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        other => panic!("unsupported message role in create_agent_message_in_scope: {other}"),
    };

    let request_doc_id = format!("fixture-request:{session_id}:{sequence}");
    let segment = OutputSegment {
        agent_did: agent_did.into(),
        requester_did: requester_did.map(Into::into),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.clone(),
        source: if role == MessageRole::Assistant {
            OutputSource::ProviderTurn {
                scope: CaptureScope {
                    kind: CaptureScopeKind::Inference,
                    seq: u64::from(sequence),
                },
                turn_index: 0,
                attempt: 0,
            }
        } else {
            OutputSource::Authored {
                key: format!("fixture:{sequence}"),
            }
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: "fixture-generation".into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: content.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: content.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![content.len() as u64],
        }),
        created_at: timestamp.into(),
    };
    let segment_response = node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !segment_response.has_errors(),
        "create_AgentOutputSegment failed: {:?}",
        segment_response.errors
    );
    let close_doc_id =
        gents::graphql::single_mutation_document(&segment_response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned();

    let message = TranscriptMessage {
        message_key: gents::session::sequence_message_key(
            agent_did,
            session_id,
            requester_did,
            sequence,
        ),
        session_id: session_id.into(),
        agent_did: agent_did.into(),
        requester_did: requester_did.map(Into::into),
        request_doc_id: Some(request_doc_id),
        publication: MessagePublication::RequestExecution {
            execution_generation: "fixture-generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role,
        native_id: (role == MessageRole::Assistant).then(|| format!("native-{sequence}")),
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id,
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at: timestamp.into(),
    };
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !response.has_errors(),
        "create_AgentMessage failed: {:?}",
        response.errors
    );
}

#[allow(clippy::too_many_arguments)]
pub async fn create_agent_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    message_sequence: u32,
    tool_call_id: &str,
    tool_name: &str,
    args: &str,
    result: &str,
    status: &str,
    started_at: &str,
    completed_at: &str,
) -> String {
    let session_id_escaped = escape_graphql_string(session_id);
    let tool_call_id_escaped = escape_graphql_string(tool_call_id);
    let tool_name_escaped = escape_graphql_string(tool_name);
    let args_escaped = escape_graphql_string(args);
    let result_escaped = escape_graphql_string(result);
    let status_escaped = escape_graphql_string(status);
    let started_escaped = escape_graphql_string(started_at);
    let completed_escaped = escape_graphql_string(completed_at);
    let tool_call_key = format!("{session_id_escaped}:{tool_call_id_escaped}");
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                session_id: "{session_id_escaped}",
                agent_did: "{AGENT_DID}",
                requester_did: null,
                message_sequence: {message_sequence},
                tool_name: "{tool_name_escaped}",
                tool_call_id: "{tool_call_id_escaped}",
                args: "{args_escaped}",
                result: "{result_escaped}",
                status: "{status_escaped}",
                started_at: "{started_escaped}",
                completed_at: "{completed_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentToolCall failed: {:?}",
        resp.errors
    );
    gents::graphql::single_mutation_document(&resp, "create_AgentToolCall")
        .expect("mutation envelope")
        .expect("created call")["_docID"]
        .as_str()
        .expect("physical call ID")
        .to_owned()
}

pub async fn create_compaction_entry(
    node: &EmbeddedNode,
    session_id: &str,
    sequence: u32,
    summary: &str,
    messages_compacted: u32,
    compacted_through_sequence: u32,
    created_at: &str,
) {
    let session_id_escaped = escape_graphql_string(session_id);
    let summary_escaped = escape_graphql_string(summary);
    let created_at_escaped = escape_graphql_string(created_at);
    let compaction_key = escape_graphql_string(&gents::session::compaction_key(
        AGENT_DID, session_id, None, sequence,
    ));
    let mutation = format!(
        r#"mutation {{
            create_CompactionEntry(input: {{
                compaction_key: "{compaction_key}",
                session_id: "{session_id_escaped}",
                agent_did: "{AGENT_DID}",
                requester_did: null,
                sequence: {sequence},
                summary: "{summary_escaped}",
                files_read: "[]",
                files_modified: "[]",
                messages_compacted: {messages_compacted},
                compacted_through_sequence: {compacted_through_sequence},
                original_tokens: 100,
                compacted_tokens: 50,
                created_at: "{created_at_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_CompactionEntry failed: {:?}",
        resp.errors
    );
}

pub async fn create_agent_behavior(node: &EmbeddedNode, behavior_id: &str, agent_did: &str) {
    // No context means literal-empty instructions/tools and default compaction.
    // Provider/model selection has one owner: the referenced inference profile.
    let profile_id = format!("{behavior_id}-inference");
    let profile: gents::document_config::InferenceProfile = serde_json::from_value(
        serde_json::json!({"agent_did": agent_did, "profile_id": profile_id,
            "backend_id": BACKEND_ID, "model_name": "test-model"}),
    )
    .expect("canonical inference fixture");
    let behavior: gents::document_config::AgentBehavior = serde_json::from_value(
        serde_json::json!({"agent_did": agent_did, "behavior_id": behavior_id,
            "display_name": "test behavior", "inference_profile_id": profile_id,
            "created_at": "2026-04-21T00:00:00Z"}),
    )
    .expect("canonical behavior fixture");
    for (collection, document) in [
        ("InferenceProfile", serde_json::to_value(profile).unwrap()),
        ("AgentBehavior", serde_json::to_value(behavior).unwrap()),
    ] {
        let input = gents_protocol::graphql::graphql_input_literal(&document)
            .expect("render canonical config fixture");
        let mutation = format!("mutation {{ create_{collection}(input: {input}) {{ _docID }} }}");
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "create_{collection}: {:?}",
            response.errors
        );
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DocIdRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
}

pub fn first_row<T>(resp: &QueryResponse, key: &str) -> T
where
    T: for<'de> Deserialize<'de>,
{
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let value = resp
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or_else(|| panic!("missing row for {key}"));
    serde_json::from_value(value).unwrap_or_else(|err| panic!("decode {key} failed: {err}"))
}

pub fn first_optional_row<T>(resp: &QueryResponse, key: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .map(|value| {
            serde_json::from_value(value).unwrap_or_else(|err| panic!("decode {key} failed: {err}"))
        })
}

/// Drive fixture requests through the production atomic begin transition.
/// Fixtures that write streamed content must retain their own writer instead.
pub async fn begin_owned_execution(
    lifecycle: &mut gents::RequestLifecycle,
    node: &std::sync::Arc<gents::defra_node::EmbeddedNode>,
) -> anyhow::Result<String> {
    let writer = gents::DefraStreamWriter::new(
        node.clone(),
        &lifecycle.request().agent_did,
        std::time::Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await?;
    Ok(lifecycle.request().doc_id.clone())
}

/// Complete one already-admitted pending request through the real execution,
/// provider-publication, and terminal-selection owners. Integration fixtures
/// use this instead of writing canonical segment/header rows or guessing an
/// execution generation.
pub async fn complete_pending_request_with_canonical_output(
    db: &TestDb,
    row: gents_protocol::row::AgentRequestRow,
    text: &str,
) -> anyhow::Result<String> {
    let behavior_id = row
        .behavior_id
        .clone()
        .context("completion fixture request omitted behavior")?;
    let prompt = row
        .content
        .clone()
        .context("completion fixture request omitted prompt")?;
    let session_id = row
        .session_id
        .clone()
        .context("completion fixture request omitted session")?;
    anyhow::ensure!(
        row.agent_did.as_deref() == Some(db.node_identity.did()),
        "completion fixture request is not owned by the fixture runtime identity"
    );

    let backend_id = format!("fixture-completion-{}", row.request_id);
    let backend = streaming_backend::MockStreamingBackend::start_with_plans(
        "fixture-completion-model",
        vec![streaming_backend::StreamPlan::current_authored_user(
            prompt.clone(),
            vec![streaming_backend::StreamResponse::streams(
                prompt,
                vec![streaming_backend::StreamChunk::text(text)],
            )],
        )],
    )?;
    fixtures::bind_behavior_backend(
        db.node.as_ref(),
        db.node_identity.did(),
        &behavior_id,
        &backend_id,
        backend.endpoint(),
        "fixture-completion-model",
    )
    .await;
    let escaped_session_id = escape_graphql_string(&session_id);
    let title = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "text": "fixture-title",
        "source": "generated"
    }))?;
    let title_response = db
        .node
        .execute(&format!(
            r#"mutation {{ update_AgentSession(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, input: {{ title: {title} }}) {{ _docID }} }}"#
        ))
        .await;
    anyhow::ensure!(
        !title_response.has_errors(),
        "prepopulating completion fixture title failed: {:?}",
        title_response.errors
    );
    let identity: std::sync::Arc<dyn gents::AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions::default(),
    )
    .await?;
    let prepared = accepted_turn::PreparedAcceptedTurn { backend };
    let runtime = accepted_turn::boot_prepared_accepted_turn(db, prepared, agent).await;
    live_inference::wait_for_request_terminal(
        db.node.as_ref(),
        &row.request_id,
        std::time::Duration::from_secs(20),
    )
    .await;
    runtime.shutdown().await;
    let request_id = escape_graphql_string(&row.request_id);
    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 2) {{ terminal_output }} }}"#
        ))
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "reading completed fixture output failed: {:?}",
        response.errors
    );
    #[derive(Deserialize)]
    struct TerminalRow {
        terminal_output: Option<gents_protocol::output::TerminalOutput>,
    }
    let terminal: TerminalRow = first_row(&response, "AgentRequest");
    match terminal.terminal_output {
        Some(gents_protocol::output::TerminalOutput::Message { message_doc_id }) => {
            Ok(message_doc_id)
        }
        other => anyhow::bail!("completion fixture selected no assistant message: {other:?}"),
    }
}

pub async fn load_request_row_by_logical_id(
    node: &EmbeddedNode,
    request_id: &str,
) -> gents_protocol::row::AgentRequestRow {
    let request_id = escape_graphql_string(request_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 2) {{
                _docID request_id agent_did requester_did behavior_id session_id content input
                execution_origin created_at deadline valid_until subagent_depth
                caused_by_parent_request_id caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id lifecycle_state
                interrupt_requested_at execution_generation execution_lease_expires_at
                terminal_output
            }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("AgentRequest query omitted rows");
    assert_eq!(
        rows.len(),
        1,
        "logical request identity must resolve to exactly one physical row"
    );
    serde_json::from_value(rows[0].clone()).expect("decode exact AgentRequest row")
}
