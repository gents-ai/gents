use serde::Deserialize;

use super::*;
use crate::request::lifecycle::test_support::{
    hints, parent_request, test_db, wake_queue_input, TestDb,
};

use crate::request::lifecycle::{
    extract_single_doc_id, ClaimOutcome, RequestLifecycle, DEFAULT_REQUEST_MAX_RETRIES,
};
use crate::request::streaming::DefraStreamWriter;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::tool_call::AwaitMode;
use gents_protocol::{
    message::{AssistantContent, Message, ToolCall, ToolFunction},
    output::PresentationPart,
};
use std::time::Duration;

mod folding;
mod goal_assignment_wakes;
mod management;
mod steering;

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

mod background_completion;
mod coalescing;
mod input;

mod pin_tests {
    use super::*;
    use crate::request::lifecycle::test_support::{
        hints, normalize_pin_fields, parent_request, pin_fixed_signing_identity,
        pin_parent_request, wake_queue_input, PIN_FIXED_DID,
    };

    /// The fixed pin root cannot mint a managed Agent signer, so the Agent is a
    /// fresh derivation under it. Expectations substitute that Agent DID for
    /// the fixed DID in Agent-owned positions; the node position stays fixed.
    fn pin_agent_signer(
        identity: &gents_store::identity::KeyIdentity,
    ) -> (String, gents_store::identity::ManagedAgentSigner) {
        let binding = gents_store::identity::provision_agent_identity(identity).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(identity, &binding).unwrap();
        let signer = agent.managed_agent_signer().unwrap();
        (binding.agent_did, signer)
    }

    #[tokio::test]
    async fn pin_session_request_create_mutation() {
        let tempdir = tempfile::tempdir().unwrap();
        let identity = pin_fixed_signing_identity(tempdir.path());
        let (agent_did, signer) = pin_agent_signer(&identity);

        let mut parent = pin_parent_request();
        parent.agent_did = agent_did.clone();
        let mutation = session_request_create_mutation(
            &parent,
            SessionRequestFields {
                agent_did: &agent_did,
                content: "steering content",
                execution_origin: ExecutionOrigin::Interactive,
                input: wake_queue_input(RequestQueue {
                    delivery: Default::default(),
                    position: None,
                    source: QueueSource::Steering,
                    policy: QueuePolicy::Append,
                    key: None,
                    queued_after_request_id: None,
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                request_id: "req-session-mutation-1",
                created_at: "2030-01-01T00:00:00Z",
                retry_key: Some("retry-key-session-1"),
            },
            &signer,
        )
        .await
        .expect("build session request create mutation");

        let fields = mutation
            .strip_prefix("mutation { create_AgentRequest(input: { ")
            .and_then(|rest| rest.strip_suffix(" }) { _docID } }"))
            .expect("mutation wraps create_AgentRequest input fields");

        let normalized = normalize_pin_fields(fields);
        assert_eq!(
            normalized,
            "request_id: \"req-session-mutation-1\", purpose: \"normal\", node_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"<AGENT>\", agent_did: \"<AGENT>\", session_id: \"sess-pin-parent\", retry_root_request: \"req-session-mutation-1\", retry_key: \"retry-key-session-1\", content: \"steering content\", input: { queue: { policy: \"append\", source: \"steering\" } }, execution_origin: \"interactive\", caused_by_correlation: \"corr-parent\", caused_by_trigger_context: \"{\\\"a\\\":\\\"b\\\"}\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, request_hop: 2, caused_by_parent_request_id: \"pin-parent-request\", caused_by_parent_request_doc_id: \"pin-parent-doc\", admission_kind: \"runtime-internal\", admission_signer_did: \"<AGENT>\", admission_signature: \"<SIGNATURE>\", runtime_issuer_did: \"<AGENT>\", runtime_source_request_id: \"pin-parent-request\", runtime_source_kind: \"local-control\", lifecycle_state: \"pending\", failure_reason: \"\""
            .replace("<AGENT>", &agent_did)
            .as_str(),
        );
    }

    #[tokio::test]
    async fn runtime_controls_keep_the_parent_workspace_binding() {
        let tempdir = tempfile::tempdir().unwrap();
        let identity = pin_fixed_signing_identity(tempdir.path());
        let (agent_did, signer) = pin_agent_signer(&identity);
        let mut parent = pin_parent_request();
        parent.agent_did = agent_did.clone();
        parent.workspace_id = Some("workspace".into());
        parent.workspace_owner_node_did = Some("did:key:workspace-owner".into());
        parent.workspace_authority = Some("readOnly".into());
        parent.workspace_seal_hash = Some("seal".into());
        for source in [QueueSource::BackgroundCompletion, QueueSource::Steering] {
            let mutation = session_request_create_mutation(
                &parent,
                SessionRequestFields {
                    agent_did: &agent_did,
                    content: "continue",
                    execution_origin: ExecutionOrigin::Scheduled,
                    input: wake_queue_input(hints(source, QueuePolicy::Append)),
                    request_id: "runtime-control",
                    created_at: "2030-01-01T00:00:00Z",
                    retry_key: None,
                },
                &signer,
            )
            .await
            .unwrap();
            for field in [
                "workspace_id: \"workspace\"",
                "workspace_owner_node_did: \"did:key:workspace-owner\"",
                "workspace_authority: \"readOnly\"",
                "workspace_seal_hash: \"seal\"",
            ] {
                assert!(mutation.contains(field), "{field}: {mutation}");
            }
        }
    }
}

fn raw_queue_create_mutation(
    db: &TestDb,
    request_id: &str,
    session_id: &str,
    input: &RequestInput,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_node_did = escape_graphql_string(db.node_did());
    let escaped_agent_did = escape_graphql_string(db.agent_did());
    let input_literal = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(input).expect("canonical input serializes"),
    )
    .expect("canonical input is a GraphQL literal");
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                purpose: "normal",
                node_did: "{escaped_node_did}",
                agent_did: "{escaped_agent_did}",
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
                request_hop: 0
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    );
    mutation
}

async fn insert_raw_queue_request(
    db: &TestDb,
    request_id: &str,
    session_id: &str,
    input: &RequestInput,
) -> String {
    let mutation = raw_queue_create_mutation(db, request_id, session_id, input);
    let response = gents_store::config_client::ConfigAccess::write_local_response(
        &db.node,
        "test.insert_raw_queue_request",
        &mutation,
    )
    .await
    .unwrap();
    extract_single_doc_id(&response, "create_AgentRequest")
        .expect("raw queue create returns _docID")
}

/// Agent-owned queue fixture: written through the Agent's own signed access so
/// the Agent's reads and transactions see the row it owns.
async fn insert_agent_queue_request(
    db: &TestDb,
    request_id: &str,
    session_id: &str,
    input: &RequestInput,
) -> String {
    let mutation = raw_queue_create_mutation(db, request_id, session_id, input);
    let response = db
        .agent_access
        .transact_write("test.insert_agent_queue_request", |txn| {
            let mutation = &mutation;
            Box::pin(async move { Ok::<_, anyhow::Error>(txn.execute(mutation).await?) })
        })
        .await
        .unwrap();
    gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap()
}

async fn publish_signed_queue_request(
    db: &TestDb,
    create: &mut gents_protocol::request_admission::AgentRequestCreate,
) -> String {
    create.admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(db.agent_did());
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(db.agent_did()).unwrap(),
        db.agent_identity.as_ref(),
    )
    .unwrap();
    create.session_doc_id = Some(
        crate::request::lifecycle::client::reserve_goal_session(&access, create)
            .await
            .expect("reserve queue fixture session"),
    );
    let agent = escape_graphql_string(db.agent_did());
    let response = access
        .execute(&format!(
            r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{agent}" }} }}, limit: 2) {{ _docID }} }}"#,
        ))
        .await
        .expect("query fixture agent");
    let [row] = response["data"]["Agent"].as_array().unwrap().as_slice() else {
        panic!("fixture agent must be unique");
    };
    create.agent_doc_id = Some(row["_docID"].as_str().unwrap().to_owned());
    crate::request::admission::sign_agent_request_create(db.agent_identity.as_ref(), create)
        .await
        .unwrap();
    crate::request::lifecycle::client::submit_client_request(&access, create)
        .await
        .expect("publish queue fixture request");
    let request_id = escape_graphql_string(&create.request_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 2) {{ _docID }} }}"#,
        ))
        .await
        .unwrap();
    let [row] = response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("published queue request must be unique");
    };
    row["_docID"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn request_doc_lookup_rejects_duplicate_logical_request_ids() {
    let db = test_db("ambiguous-request-doc-lookup").await;
    let input = wake_queue_input(hints(QueueSource::User, QueuePolicy::Append));
    insert_raw_queue_request(&db, "duplicate-logical-id", "session-a", &input).await;
    insert_raw_queue_request(&db, "duplicate-logical-id", "session-b", &input).await;

    let error = crate::request::binding::resolve_request_doc_id(&db.node, "duplicate-logical-id")
        .await
        .expect_err("duplicate logical ids must not resolve to an arbitrary document");
    assert!(error.to_string().contains("ambiguous across 2 documents"));
}
