use super::*;
use crate::identity::AgentIdentity;
use std::sync::Arc;
use tempfile::TempDir;

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
    metadata: Option<String>,
    lifecycle_state: Option<String>,
    execution_origin: String,
    superseded_by_request: Option<String>,
    superseded_by_request_doc_id: Option<String>,
    subagent_depth: Option<u32>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
}

fn hints(source: QueueSource, policy: QueuePolicy) -> QueueHints {
    QueueHints {
        source,
        policy,
        key: Some("session:sess-1".to_string()),
        queued_after_request_id: Some("req-1".to_string()),
        interrupted_request_id: None,
    }
}

fn parent_request(agent_did: &str, session_id: &str) -> AgentRequest {
    AgentRequest {
        doc_id: "parent-doc".to_string(),
        request_id: "parent-request".to_string(),
        agent_did: agent_did.to_string(),
        requester_did: None,
        behavior_id: Some(TEST_BEHAVIOR_ID.to_string()),
        session_id: session_id.to_string(),
        content: "parent".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        seed: None,
        max_tokens: None,
        max_total_tokens: None,
        metadata: None,
        execution_origin: Some("interactive".to_string()),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        deadline: None,
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
        workspace_authority: None,
        workspace_owner_deployment_id: None,
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
                metadata
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
    metadata: &str,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_metadata = escape_graphql_string(metadata);
    let escaped_agent_did = escape_graphql_string(agent_did);
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
                metadata: "{escaped_metadata}",
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
    let response =
        session::execute_mutation_with_retry(node, &mutation, "insert_raw_queue_request")
            .await
            .unwrap();
    extract_single_doc_id(&response, "create_AgentRequest")
        .expect("raw queue create returns _docID")
}

#[tokio::test]
async fn request_doc_lookup_rejects_duplicate_logical_request_ids() {
    let db = test_db("ambiguous-request-doc-lookup").await;
    let metadata = queue_metadata_json(&hints(QueueSource::User, QueuePolicy::Append));
    insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "duplicate-logical-id",
        "session-a",
        &metadata,
    )
    .await;
    insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "duplicate-logical-id",
        "session-b",
        &metadata,
    )
    .await;

    let error = lookup_request_doc_id_optional(&db.node, "duplicate-logical-id")
        .await
        .expect_err("duplicate logical ids must not resolve to an arbitrary document");
    assert!(error.to_string().contains("ambiguous across 2 documents"));
}

mod background_completion;
mod coalescing;
mod metadata;

/// Pins today's `AgentRequestCreate::graphql_input_fields()` output for the
/// `lifecycle::queue` production writers (#1336 Task 1), before they are
/// switched onto `build_signed_request` (#1336 Task 2). Both writers take
/// `created_at` (and, for the session-mutation site, `request_id` and
/// `retry_key`) as caller-supplied parameters, so — with a deterministic
/// signing identity — their output is fully stable across runs; no
/// normalization is needed here (contrast `lifecycle::materialize::pin_tests`,
/// where `created_at` is generated internally).
mod pin_tests {
    use super::*;
    use crate::identity::AgentIdentity;

    /// Same fixed Ed25519 identity material as
    /// `lifecycle::materialize::pin_tests`, duplicated here rather than
    /// shared so each pinning module stays a self-contained fixture (this
    /// crate's existing convention: see `test_db` above).
    const PIN_FIXED_KEY_HEX: &str = "4cbf8c1186d2fcb70559342fd142650a5ec5938d26a187d87e2c061b530d7be46edb79d5f548207182f7911b55709c9e4b9961c709486e5ce920e306470fe6d6";
    const PIN_FIXED_DID: &str = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";

    fn pin_fixed_signing_identity(dir: &std::path::Path) -> crate::identity::KeyIdentity {
        let key_bytes: Vec<u8> = (0..PIN_FIXED_KEY_HEX.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&PIN_FIXED_KEY_HEX[offset..offset + 2], 16).unwrap())
            .collect();
        let path = dir.join("pinning.key");
        std::fs::write(&path, &key_bytes).expect("write fixed pinning key");
        let identity =
            crate::identity::KeyIdentity::load_or_create(&path, None).expect("load fixed identity");
        assert_eq!(identity.did(), PIN_FIXED_DID);
        identity
    }

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
            r#"{"queue":{"source":"steering"}}"#,
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

        assert_eq!(
            fields,
            "request_id: \"req-session-mutation-1\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"behavior-1\", session_id: \"sess-pin-parent\", retry_root_request: \"req-session-mutation-1\", retry_key: \"retry-key-session-1\", content: \"steering content\", metadata: \"{\\\"queue\\\":{\\\"source\\\":\\\"steering\\\"}}\", execution_origin: \"interactive\", caused_by_correlation: \"corr-parent\", caused_by_trigger_context: \"{\\\"a\\\":\\\"b\\\"}\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, subagent_depth: 2, caused_by_parent_request_id: \"pin-parent-request\", caused_by_parent_request_doc_id: \"pin-parent-doc\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"5Tvz6LV31BdgtSqE22FqJHXnatDd6BkkvuVtKmEGJPai2Md5ooHQWKb4UZ4vc2q7oyh2cXh3UhkeqjFp9oqsUgjh\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"pin-parent-request\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }

    // --- Site 4: `lifecycle::queue::goal_continuation::enqueue_goal_continuation` ---
    // This persists to a live node and returns only the doc/request/session
    // ids, not the `AgentRequestCreate` it built, and its retry-key dedupe
    // lookup also requires a node. The block below reproduces its
    // DTO-construction statements verbatim (see the production function),
    // substituting a fixed `now` for `Utc::now()` and inlining
    // `parent_behavior_id`'s no-node-needed fast path (returning
    // `parent.behavior_id` directly, since it is set here).

    #[tokio::test]
    async fn pin_enqueue_goal_continuation_dto_construction() {
        use sha2::{Digest, Sha256};

        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let mut parent = pin_parent_request();
        parent.workspace_id = Some("ws-goal-1".to_string());
        parent.workspace_authority = Some("readWrite".to_string());
        parent.workspace_owner_deployment_id = Some("dep-goal-1".to_string());
        parent.workspace_seal_hash = Some("seal-goal-1".to_string());

        let goal_id = "goal-1";
        let continuation_sequence: i64 = 3;
        let content = "continue the goal";
        let behavior_id = parent.behavior_id.clone().unwrap();

        let digest = Sha256::digest(format!("{goal_id}\0{}", parent.request_id).as_bytes());
        let digest_hex = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let request_id = format!("goal-cont-{continuation_sequence:020}-{digest_hex}");
        let retry_key = format!("goal-continuation:{digest_hex}");
        let now = "2030-01-01T00:00:00Z".to_string();
        let queue_hints = QueueHints {
            source: QueueSource::Goal,
            policy: QueuePolicy::Coalesce,
            key: Some(format!("goal:{digest_hex}")),
            queued_after_request_id: Some(parent.request_id.clone()),
            interrupted_request_id: None,
        };
        let metadata = serde_json::json!({
            "queue": queue_hints,
            "goal": {
                "goal_id": goal_id,
                "parent_request_id": parent.request_id,
                "continuation_sequence": continuation_sequence,
                "wrapup": false,
            }
        })
        .to_string();
        let admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
                &parent.agent_did,
                &parent.request_id,
            );
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            request_id.clone(),
            &parent.agent_did,
            &parent.agent_did,
            behavior_id,
            &parent.session_id,
            content,
            "scheduled",
            now,
            admission,
        );
        create.metadata = Some(metadata);
        create.retry_key = Some(retry_key.clone());
        create.caused_by_trigger_id = Some(goal_id.to_string());
        create.caused_by_trigger_kind = Some("goal".to_string());
        create.caused_by_correlation = parent.caused_by_correlation.clone();
        create.caused_by_trigger_context = parent.caused_by_trigger_context.clone();
        create.caused_by_parent_request_id = Some(parent.request_id.clone());
        create.caused_by_parent_request_doc_id = Some(parent.doc_id.clone());
        create.max_retries = i64::from(DEFAULT_REQUEST_MAX_RETRIES);
        create.subagent_depth = parent.subagent_depth;
        create.workspace_id = parent.workspace_id.clone();
        create.workspace_authority = parent.workspace_authority.clone();
        create.workspace_owner_deployment_id = parent.workspace_owner_deployment_id.clone();
        create.workspace_seal_hash = parent.workspace_seal_hash.clone();
        crate::sign_agent_request_create_as_registered_target(&mut create)
            .await
            .expect("sign goal continuation request");

        let fields = create.graphql_input_fields().expect("graphql_input_fields");
        assert_eq!(
            fields,
            "request_id: \"goal-cont-00000000000000000003-355ac0cefea9f3d0afab06ffb90fd1ec\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"general\", session_id: \"sess-pin-parent\", retry_root_request: \"goal-cont-00000000000000000003-355ac0cefea9f3d0afab06ffb90fd1ec\", retry_key: \"goal-continuation:355ac0cefea9f3d0afab06ffb90fd1ec\", content: \"continue the goal\", metadata: \"{\\\"goal\\\":{\\\"continuation_sequence\\\":3,\\\"goal_id\\\":\\\"goal-1\\\",\\\"parent_request_id\\\":\\\"pin-parent-request\\\",\\\"wrapup\\\":false},\\\"queue\\\":{\\\"key\\\":\\\"goal:355ac0cefea9f3d0afab06ffb90fd1ec\\\",\\\"policy\\\":\\\"coalesce\\\",\\\"queued_after_request_id\\\":\\\"pin-parent-request\\\",\\\"source\\\":\\\"goal\\\"}}\", execution_origin: \"scheduled\", caused_by_trigger_id: \"goal-1\", caused_by_trigger_kind: \"goal\", caused_by_correlation: \"corr-parent\", caused_by_trigger_context: \"{\\\"a\\\":\\\"b\\\"}\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, subagent_depth: 2, caused_by_parent_request_id: \"pin-parent-request\", caused_by_parent_request_doc_id: \"pin-parent-doc\", workspace_id: \"ws-goal-1\", workspace_authority: \"readWrite\", workspace_owner_deployment_id: \"dep-goal-1\", workspace_seal_hash: \"seal-goal-1\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"2eoh9K96ztMwPRNFSEiQuJAqNSxY92o9UamCjFePL3YFFpmKmAUnvT5ccGjq49RpmxskKT215bnirUwsXz9SHgUP\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"pin-parent-request\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }
}
