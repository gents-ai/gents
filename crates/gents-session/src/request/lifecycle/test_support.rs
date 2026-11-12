use crate::request::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle, TriggerLineage};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use std::{sync::Arc, time::Duration};

pub use gents_store::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

pub async fn native_queue_image(
    node: &std::sync::Arc<defra_node::Node>,
    node_did: &str,
    session_id: &str,
    selection: &[crate::request::lifecycle::queue::FoldedInput],
) -> gents_lean_vocab::QueueImage {
    native_queue_image_with_access(
        &gents_store::config_client::ConfigAccess::local(node.clone()),
        node_did,
        session_id,
        selection,
    )
    .await
}

/// The queue image observed by `access`, such as the Agent that owns the rows.
pub async fn native_queue_image_with_access(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    selection: &[crate::request::lifecycle::queue::FoldedInput],
) -> gents_lean_vocab::QueueImage {
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    #[derive(serde::Deserialize)]
    struct Row {
        request_id: String,
        lifecycle_state: RequestLifecycleState,
    }
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ request_id lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(session_id)
        ))
        .await
        .unwrap();
    let rows: Vec<Row> = serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
    let state = |id: &str| {
        rows.iter()
            .find(|row| row.request_id == id)
            .map(|row| row.lifecycle_state)
    };
    let active = rows
        .iter()
        .filter(|row| {
            matches!(
                row.lifecycle_state,
                RequestLifecycleState::Claimed | RequestLifecycleState::Processing
            )
        })
        .map(|row| row.request_id.clone())
        .collect::<Vec<_>>();
    let folding = selection
        .iter()
        .map(|folded| folded.request_id.clone())
        .filter(|id| state(id) == Some(RequestLifecycleState::Pending))
        .collect::<Vec<_>>();
    let pending =
        crate::request::lifecycle::pending_user_queue(access, node_did, session_id, node_did)
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.request_id)
            .filter(|id| !folding.contains(id))
            .collect::<Vec<_>>();
    let terminal = rows
        .iter()
        .filter(|row| row.lifecycle_state.is_terminal())
        .map(|row| row.request_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(active.len() <= 1, "one active request: {active:?}");
    assert_eq!(
        rows.len(),
        active.len() + folding.len() + pending.len() + terminal.len(),
        "every request is active, queued or terminal"
    );
    gents_lean_vocab::QueueImage {
        active: active.into_iter().next(),
        folding,
        pending,
        terminal,
    }
}

pub fn hints(
    source: gents_protocol::request_input::QueueSource,
    policy: gents_protocol::request_input::QueuePolicy,
) -> gents_protocol::request_input::RequestQueue {
    gents_protocol::request_input::RequestQueue {
        delivery: Default::default(),
        position: None,
        source,
        policy,
        key: Some("session:sess-1".to_string()),
        queued_after_request_id: Some("req-1".to_string()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

pub fn wake_queue_input(
    queue: gents_protocol::request_input::RequestQueue,
) -> gents_protocol::request_input::RequestInput {
    gents_protocol::request_input::RequestInput {
        queue: Some(queue),
        ..Default::default()
    }
}

pub fn parent_request(
    node_did: &str,
    agent_did: &str,
    session_id: &str,
) -> gents_protocol::row::AgentRequest {
    gents_protocol::row::AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: "parent-doc".to_string(),
        request_id: "parent-request".to_string(),
        node_did: node_did.to_string(),
        requester_did: None,
        agent_did: agent_did.to_string(),
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
        request_hop: 2,
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
        workspace_owner_node_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    }
}

pub fn normalize_pin_fields(fields: &str) -> String {
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

pub fn pin_parent_request() -> gents_protocol::row::AgentRequest {
    let mut parent = parent_request(PIN_FIXED_DID, PIN_FIXED_DID, "sess-pin-parent");
    parent.request_id = "pin-parent-request".to_string();
    parent.doc_id = "pin-parent-doc".to_string();
    parent.request_hop = 2;
    parent.caused_by_correlation = Some("corr-parent".to_string());
    parent.caused_by_trigger_context = Some(r#"{"a":"b"}"#.to_string());
    parent
}

pub async fn request_row(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
) -> gents_protocol::row::AgentRequestRow {
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ _docID request_id purpose node_did requester_did agent_did session_id lifecycle_state execution_generation execution_lease_expires_at terminal_output terminalized_at failure_reason }} }}"#,
            gents_store::graphql::escape_graphql_string(doc_id),
        ))
        .await
        .expect("AgentRequest read as its Agent");
    serde_json::from_value::<Vec<gents_protocol::row::AgentRequestRow>>(
        response["data"]["AgentRequest"].clone(),
    )
    .unwrap()
    .pop()
    .expect("AgentRequest visible to its Agent")
}

pub async fn observed_execution(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
) -> gents_protocol::row::AgentRequestRow {
    let mut row = request_row(access, doc_id).await;
    row.doc_id = Some(doc_id.to_owned());
    row
}

pub async fn expire_observed_execution(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
) -> gents_protocol::row::AgentRequestRow {
    let expiry = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    access
        .write(
            "test.expire_observed_execution",
            &format!(
                r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(doc_id),
                gents_store::graphql::escape_graphql_string(&expiry),
            ),
        )
        .await
        .expect("expire observed execution as its Agent");
    observed_execution(access, doc_id).await
}

pub async fn output_segment_snapshot(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
) -> serde_json::Value {
    let response = access
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID ordinal payload close }} }}"#,
            gents_store::graphql::escape_graphql_string(doc_id),
        ))
        .await
        .expect("AgentOutputSegment read as its Agent");
    response["data"]["AgentOutputSegment"].clone()
}

/// A signed node with its user and the managed Agent access that owns the queue rows.
pub struct TestDb {
    pub node: std::sync::Arc<defra_node::Node>,
    pub identity: std::sync::Arc<dyn NodeIdentity>,
    pub agent_identity: std::sync::Arc<dyn NodeIdentity>,
    pub user: gents_store::identity::KeyIdentity,
    pub user_access: gents_store::config_client::ConfigAccess,
    pub agent_access: gents_store::config_client::ConfigAccess,
    pub _db: gents_inference::test_support::TestDb,
    pub _tempdir: tempfile::TempDir,
}

impl TestDb {
    pub fn node_did(&self) -> &str {
        self.identity.did()
    }

    pub fn agent_did(&self) -> &str {
        self.agent_identity.did()
    }
}

pub async fn test_db(name: &str) -> TestDb {
    test_db_with(name, crate::test_support::no_lifecycle_plugins()).await
}

/// A signed test node composed with `composition`, for tests that need a
/// plugin (such as Goals) that the session crate cannot name.
pub async fn test_db_with(
    name: &str,
    composition: gents_store::node_extension::Extensions,
) -> TestDb {
    let (db, user, user_access, binding) =
        gents_inference::test_support::signed_test_node(name, composition).await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let agent_identity = gents_store::identity::resolve_agent_identity(identity.as_ref(), &binding)
        .expect("derived test agent identity");
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent_identity.did()).unwrap(),
        agent_identity.as_ref(),
    )
    .unwrap();
    TestDb {
        node,
        identity,
        agent_identity,
        user,
        user_access,
        agent_access,
        _db: db,
        _tempdir: tempfile::tempdir().expect("queue fixture directory"),
    }
}

pub async fn test_node() -> (
    Arc<EmbeddedNode>,
    gents_inference::test_support::TestDb,
    Arc<dyn gents_store::identity::NodeIdentity>,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let db = gents_inference::test_support::test_db(
        "execution-lease-fixture",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let (db, _user, _access, agent) =
        gents_inference::test_support::signed_test_node_from_db(db, "execution-lease-owner").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    (node, db, identity, agent)
}

pub async fn claimed_owner(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn gents_store::identity::NodeIdentity>,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> RequestLifecycle {
    claimed_owner_caused_by(node, identity, agent, TriggerLineage::default()).await
}

pub async fn claimed_owner_caused_by(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn gents_store::identity::NodeIdentity>,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
    lineage: TriggerLineage,
) -> RequestLifecycle {
    let agent_identity =
        gents_store::identity::resolve_agent_identity(identity.as_ref(), agent).unwrap();
    let mut lifecycle = RequestLifecycle::materialize_pending_with_execution_binding(
        node.clone(),
        &agent.agent_did,
        identity.clone(),
        "hello",
        60,
        ExecutionOrigin::Interactive,
        "lease-test",
        lineage,
        Some(agent_identity),
    )
    .await
    .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    lifecycle
}

pub async fn owner(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn gents_store::identity::NodeIdentity>,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> RequestLifecycle {
    let mut lifecycle = claimed_owner(node, identity, agent).await;
    let writer = crate::request::streaming::DefraStreamWriter::new_managed(
        node.clone(),
        gents_store::identity::resolve_agent_identity(identity.as_ref(), agent)
            .unwrap()
            .as_ref(),
        Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    lifecycle
}

pub fn agent_identity(
    identity: &Arc<dyn gents_store::identity::NodeIdentity>,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> Arc<dyn gents_store::identity::NodeIdentity> {
    gents_store::identity::resolve_agent_identity(identity.as_ref(), agent).unwrap()
}

/// Request, output and transcript rows are owned by the Agent, so fixtures read
/// and write them through the Agent's own signed access, never the node identity.
pub fn agent_access(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn gents_store::identity::NodeIdentity>,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> gents_store::config_client::ConfigAccess {
    gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(&agent.agent_did).unwrap(),
        agent_identity(identity, agent).as_ref(),
    )
    .unwrap()
}

pub fn agent_signer(
    identity: &Arc<dyn gents_store::identity::NodeIdentity>,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> gents_store::identity::ManagedAgentSigner {
    agent_identity(identity, agent)
        .managed_agent_signer()
        .expect("managed Agent signer")
}

pub async fn observed_case_request(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn gents_store::identity::NodeIdentity>,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
    case: &gents_lean_vocab::LeanRequestExecutionObservedCase,
    epoch: DateTime<Utc>,
) -> RequestLifecycle {
    let lifecycle = owner(node, identity, agent).await;
    let doc_id = escape_graphql_string(&lifecycle.request.doc_id);
    let deadline =
        epoch + chrono::Duration::seconds(case.pre.lease.explicit_deadline.unwrap() as i64);
    agent_access(node, identity, agent).write("test.observed_lease_seed", &format!(
        r#"mutation {{ update_AgentRequest(docID: "{doc_id}", input: {{ execution_lease_secs: {}, execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
        case.pre.lease.duration.unwrap(), escape_graphql_string(&deadline.to_rfc3339()),
    )).await.unwrap();
    lifecycle
}

use tracing::field::{Field, Visit};

pub async fn agent_write_value(
    access: &gents_store::config_client::ConfigAccess,
    operation: &'static str,
    mutation: &'static str,
    variables: serde_json::Value,
) -> serde_json::Value {
    access
        .transact_write(operation, |txn| {
            let variables = &variables;
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
        })
        .await
        .expect("Agent-signed canonical write")
}

#[derive(Default)]
pub struct PendingNativeOutcome(pub bool);

impl Visit for PendingNativeOutcome {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "outcome" && value == "pending_native" {
            self.0 = true;
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "outcome" && format!("{value:?}").trim_matches('"') == "pending_native" {
            self.0 = true;
        }
    }
}
