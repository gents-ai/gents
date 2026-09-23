use super::*;
use crate::identity::AgentIdentity;
use crate::lifecycle::{
    extract_single_doc_id, ClaimOutcome, RequestLifecycle, DEFAULT_REQUEST_MAX_RETRIES,
};
use crate::streaming::DefraStreamWriter;
use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::{
    message::{AssistantContent, Message, ToolCall, ToolFunction},
    output::PresentationPart,
};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

mod background_goal_repair;
mod steering;

const TEST_AGENT_DID: &str = "did:test:queue-test";
const TEST_BEHAVIOR_ID: &str = "general";

struct TestDb {
    node: Arc<EmbeddedNode>,
    identity: Arc<crate::identity::KeyIdentity>,
    _tempdir: TempDir,
}

impl TestDb {
    fn agent_did(&self) -> &str {
        self.identity.did()
    }
}

#[derive(Debug, Deserialize)]
struct QueueRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    session_id: String,
    behavior_id: String,
    content: String,
    #[serde(default)]
    input: Option<RequestInput>,
    #[serde(default)]
    lifecycle_state: Option<RequestLifecycleState>,
    execution_origin: String,
    superseded_by_request: Option<String>,
    superseded_by_request_doc_id: Option<String>,
    subagent_depth: Option<u32>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
}

fn hints(source: QueueSource, policy: QueuePolicy) -> RequestQueue {
    RequestQueue {
        source,
        policy,
        key: Some("session:sess-1".to_string()),
        queued_after_request_id: Some("req-1".to_string()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

fn wake_queue_input(queue: RequestQueue) -> RequestInput {
    RequestInput {
        queue: Some(queue),
        ..Default::default()
    }
}

fn parent_request(agent_did: &str, session_id: &str) -> AgentRequest {
    AgentRequest {
        doc_id: "parent-doc".to_string(),
        request_id: "parent-request".to_string(),
        agent_did: agent_did.to_string(),
        requester_did: None,
        behavior_id: TEST_BEHAVIOR_ID.to_string(),
        session_id: session_id.to_string(),
        content: "parent".to_string(),
        max_total_tokens: None,
        input: gents_protocol::request_input::RequestInput::default(),
        execution_origin: Some("interactive".to_string()),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        subagent_depth: 2,
        caused_by_parent_request_id: Some("root-parent-request".to_string()),
        caused_by_parent_request_doc_id: Some("root-parent-request-doc".to_string()),
        caused_by_parent_tool_call_id: Some("root-parent-tool-call".to_string()),
        caused_by_parent_tool_call_doc_id: Some("root-parent-tool-call-doc".to_string()),
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_owner_agent_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    }
}

#[test]
fn transaction_create_doc_id_accepts_both_defradb_response_shapes() {
    for response in [
        serde_json::json!({
            "data": { "create_AgentRequest": { "_docID": "doc-object" } }
        }),
        serde_json::json!({
            "data": { "add_AgentRequest": [{ "_docID": "doc-array" }] }
        }),
    ] {
        let doc_id = transaction_created_doc_id(&response, "AgentRequest").unwrap();
        assert!(doc_id == "doc-object" || doc_id == "doc-array");
    }
}

async fn test_db(name: &str) -> TestDb {
    let tempdir = tempfile::Builder::new()
        .prefix(&format!("gents-queue-{name}-"))
        .tempdir()
        .expect("tempdir");
    let identity = Arc::new(
        crate::identity::KeyIdentity::load_or_create(tempdir.path().join("agent.key"), None)
            .expect("test identity"),
    );
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(tempdir.path())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .expect("embedded node"),
    );
    crate::schema::ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");
    crate::test_support::install_test_behavior(&node, identity.did(), TEST_BEHAVIOR_ID).await;
    TestDb {
        node,
        identity,
        _tempdir: tempdir,
    }
}

async fn queue_rows(node: &EmbeddedNode, session_id: &str) -> Vec<QueueRow> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                _docID
                request_id
                session_id
                behavior_id
                content
                input
                lifecycle_state
                execution_origin
                superseded_by_request
                superseded_by_request_doc_id
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "queue row query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

async fn insert_raw_queue_request(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    input: &RequestInput,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let input_literal = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(input).expect("canonical input serializes"),
    )
    .expect("canonical input is a GraphQL literal");
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{TEST_BEHAVIOR_ID}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "raw duplicate",
                input: {input_literal},
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "scheduled",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: 0
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "test.insert_raw_queue_request",
        &mutation,
    )
    .await
    .unwrap();
    extract_single_doc_id(&response, "create_AgentRequest")
        .expect("raw queue create returns _docID")
}

/// A real accepted background tool and its closed `ToolExecution` source for
/// queue-owner tests.  This deliberately goes through provider publication,
/// accepted-tool adoption, running CAS, and terminal output closure: queue
/// notification tests must not manufacture an `AgentToolCall` authority row.
struct CanonicalBackgroundFixture {
    node: Arc<EmbeddedNode>,
    parent: AgentRequest,
    lifecycle: RequestLifecycle,
    writer: DefraStreamWriter,
    next_turn: usize,
}

async fn canonical_background_fixture(db: &TestDb, session_id: &str) -> CanonicalBackgroundFixture {
    let request_id = format!("background-parent-{}", uuid::Uuid::new_v4());
    insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        &request_id,
        session_id,
        &RequestInput::default(),
    )
    .await;
    let request = crate::request_binding::load_agent_request(&db.node, &request_id)
        .await
        .expect("load canonical fixture parent")
        .expect("canonical fixture parent exists");
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        db.node.clone(),
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        request,
        60,
    );
    assert_eq!(
        lifecycle
            .claim()
            .await
            .expect("claim canonical fixture parent"),
        ClaimOutcome::Claimed
    );
    let writer = DefraStreamWriter::new(db.node.clone(), db.agent_did(), Duration::ZERO);
    lifecycle
        .begin_owned_execution(&writer)
        .await
        .expect("begin canonical fixture parent");
    CanonicalBackgroundFixture {
        node: db.node.clone(),
        parent: lifecycle.request().clone(),
        lifecycle,
        writer,
        next_turn: 0,
    }
}

impl CanonicalBackgroundFixture {
    async fn persist_notification(
        &mut self,
        notification_content: &str,
        message_key: &str,
        wake_content: &str,
        queue: RequestQueue,
        existing_notification_doc_id: Option<&str>,
    ) -> anyhow::Result<EnqueuedBackgroundCompletionInput> {
        anyhow::ensure!(
            queue.source == QueueSource::BackgroundCompletion
                && queue.policy == QueuePolicy::Coalesce
                && queue
                    .key
                    .as_deref()
                    .is_some_and(|key| !key.trim().is_empty()),
            "fixture notification requires a keyed coalescing background queue"
        );
        let turn = self.next_turn;
        self.next_turn += 1;
        self.writer
            .start_provider_attempt(
                &self.parent.doc_id,
                turn,
                0,
                format!("inference.{}", turn + 1).parse()?,
            )
            .await;
        let native_id = format!("fixture-native-tool-{turn}");
        let message = Message::Assistant {
            id: Some(format!("fixture-provider-message-{turn}")),
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: native_id.clone(),
                call_id: Some(format!("fixture-provider-call-{turn}")),
                function: ToolFunction::new(
                    crate::toolset::SPAWN_PROCESS_TOOL_NAME.into(),
                    serde_json::json!({"tool_name": "fixture", "args": {}}),
                ),
                signature: None,
                additional_params: None,
            })],
        };
        let mut accepted = self
            .writer
            .publish_native_turn(&self.lifecycle, turn, 0, &message)
            .await?
            .accepted_tools;
        anyhow::ensure!(accepted.len() == 1, "fixture expected one accepted tool");
        let deadline = self
            .lifecycle
            .claimed_deadline_at()
            .context("fixture parent claim has no deadline")?;
        let mut tool = ToolCallLifecycle::from_accepted(
            self.node.clone(),
            self.parent.agent_did.clone(),
            self.parent.requester_did.clone(),
            accepted.pop().expect("one accepted tool"),
            deadline,
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
        )?;
        tool.start_running().await?;
        let mut tool = tool
            .admit_spawned_background(
                crate::tool_call_lifecycle::SpawnedBackgroundToolAdmission {
                    tool_name: "fixture".to_owned(),
                    deadline_at: deadline,
                },
                "background work started",
            )
            .await?;
        tool.start_running().await?;
        let binding = tool
            .tool_output_binding()
            .context("accepted fixture tool has output binding")?;
        crate::tool_call_lifecycle::delivery::append_tool_output(&binding, notification_content)
            .await?;
        anyhow::ensure!(
            tool.complete_owned(notification_content, None).await?,
            "fixture tool terminal CAS lost"
        );
        let tool_call_doc_id = tool
            .doc_id()
            .context("fixture tool has physical document")?;
        persist_background_completion_with_message_canonical(
            &self.node,
            &self.parent,
            notification_content,
            message_key,
            wake_content,
            queue,
            existing_notification_doc_id,
            &ToolNotificationPublication {
                tool_call_doc_id: tool_call_doc_id.to_owned(),
                presentation: vec![PresentationPart::OutputRange {
                    start_byte: 0,
                    end_byte: notification_content.len() as u64,
                }],
            },
        )
        .await
    }
}

#[tokio::test]
async fn request_doc_lookup_rejects_duplicate_logical_request_ids() {
    let db = test_db("ambiguous-request-doc-lookup").await;
    let input = wake_queue_input(hints(QueueSource::User, QueuePolicy::Append));
    insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "duplicate-logical-id",
        "session-a",
        &input,
    )
    .await;
    insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "duplicate-logical-id",
        "session-b",
        &input,
    )
    .await;

    let error = crate::request_binding::resolve_request_doc_id(&db.node, "duplicate-logical-id")
        .await
        .expect_err("duplicate logical ids must not resolve to an arbitrary document");
    assert!(error.to_string().contains("ambiguous across 2 documents"));
}

/// Replace the admission signature field (owned and pinned by the protocol
/// crate's canonical encoder tests) with a placeholder so the runtime pin
/// stays stable while still asserting the exact field set.
fn normalize_pin_fields(fields: &str) -> String {
    let start = fields
        .find("admission_signature: \"")
        .expect("admission signature field present");
    let rest = &fields[start + "admission_signature: \"".len()..];
    let end = rest.find('"').expect("signature value closes");
    let mut normalized = String::with_capacity(fields.len());
    normalized.push_str(&fields[..start + "admission_signature: \"".len()]);
    normalized.push_str("<SIGNATURE>");
    normalized.push_str(&rest[end..]);
    normalized
}

mod background_completion;
mod coalescing;
mod input;

/// Pin signed GraphQL fields through the production preparation functions.
/// Fixed timestamps and a deterministic signing identity keep these wire
/// compatibility checks stable without duplicating request construction.
mod pin_tests {
    use super::*;
    use crate::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

    fn pin_parent_request() -> AgentRequest {
        let mut parent = parent_request(PIN_FIXED_DID, "sess-pin-parent");
        parent.request_id = "pin-parent-request".to_string();
        parent.doc_id = "pin-parent-doc".to_string();
        parent.subagent_depth = 2;
        parent.caused_by_correlation = Some("corr-parent".to_string());
        parent.caused_by_trigger_context = Some(r#"{"a":"b"}"#.to_string());
        parent
    }

    // --- Site 3: `lifecycle::queue::mutation::session_request_create_mutation` ---
    // Pure (no node) and already returns the full mutation string built
    // from `create.graphql_mutation()`; the input fields are extracted from
    // that known wrapper.

    #[tokio::test]
    async fn pin_session_request_create_mutation() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let parent = pin_parent_request();
        let mutation = session_request_create_mutation(
            &parent,
            "behavior-1",
            "steering content",
            ExecutionOrigin::Interactive,
            wake_queue_input(RequestQueue {
                source: QueueSource::Steering,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: None,
                interrupted_request_id: None,
                background_completion_wake_version: None,
            }),
            "req-session-mutation-1",
            "2030-01-01T00:00:00Z",
            Some("retry-key-session-1"),
        )
        .await
        .expect("build session request create mutation");

        let fields = mutation
            .strip_prefix("mutation { create_AgentRequest(input: { ")
            .and_then(|rest| rest.strip_suffix(" }) { _docID } }"))
            .expect("mutation wraps create_AgentRequest input fields");

        let normalized = normalize_pin_fields(&fields);
        assert_eq!(
            normalized,
            "request_id: \"req-session-mutation-1\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"behavior-1\", session_id: \"sess-pin-parent\", retry_root_request: \"req-session-mutation-1\", retry_key: \"retry-key-session-1\", content: \"steering content\", input: { queue: { policy: \"append\", source: \"steering\" } }, execution_origin: \"interactive\", caused_by_correlation: \"corr-parent\", caused_by_trigger_context: \"{\\\"a\\\":\\\"b\\\"}\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, subagent_depth: 2, caused_by_parent_request_id: \"pin-parent-request\", caused_by_parent_request_doc_id: \"pin-parent-doc\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"<SIGNATURE>\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"pin-parent-request\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }

    // Pin the production preparation seam; do not reconstruct its DTO here.
    #[tokio::test]
    async fn pin_goal_continuation_preparation() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let mut parent = pin_parent_request();
        parent.workspace_id = Some("ws-goal-1".to_string());
        parent.workspace_owner_agent_did = Some("did:key:workspace-owner".to_string());
        parent.workspace_authority = Some("readWrite".to_string());
        parent.workspace_seal_hash = Some("seal-goal-1".to_string());

        let goal_id = "goal-1";
        let continuation_sequence: i64 = 3;
        let content = "continue the goal";
        let behavior_id = parent.behavior_id.clone();

        let mut create = prepare_goal_continuation(
            &parent,
            behavior_id,
            goal_id,
            content,
            continuation_sequence,
            false,
            "2030-01-01T00:00:00Z",
        )
        .expect("prepare goal continuation");
        crate::lifecycle::materialize::sign_request(
            &mut create,
            crate::lifecycle::materialize::RequestSigner::RegisteredTarget,
        )
        .await
        .expect("sign goal continuation request");

        let fields = create.graphql_input_fields().expect("graphql_input_fields");
        assert_eq!(
            normalize_pin_fields(&fields),
            "request_id: \"goal-cont-00000000000000000003-355ac0cefea9f3d0afab06ffb90fd1ec\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"general\", session_id: \"sess-pin-parent\", retry_root_request: \"goal-cont-00000000000000000003-355ac0cefea9f3d0afab06ffb90fd1ec\", retry_key: \"goal-continuation:355ac0cefea9f3d0afab06ffb90fd1ec\", content: \"continue the goal\", input: { goal_continuation: { sequence: 3, wrapup: false }, queue: { key: \"goal:355ac0cefea9f3d0afab06ffb90fd1ec\", policy: \"coalesce\", queued_after_request_id: \"pin-parent-request\", source: \"goal\" } }, execution_origin: \"scheduled\", caused_by_trigger_id: \"goal-1\", caused_by_trigger_kind: \"goal\", caused_by_correlation: \"corr-parent\", caused_by_trigger_context: \"{\\\"a\\\":\\\"b\\\"}\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, subagent_depth: 2, caused_by_parent_request_id: \"pin-parent-request\", caused_by_parent_request_doc_id: \"pin-parent-doc\", workspace_id: \"ws-goal-1\", workspace_owner_agent_did: \"did:key:workspace-owner\", workspace_authority: \"readWrite\", workspace_seal_hash: \"seal-goal-1\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"<SIGNATURE>\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"pin-parent-request\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }
}
