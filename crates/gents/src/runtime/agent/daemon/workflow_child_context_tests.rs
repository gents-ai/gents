//! The workflow return instruction, through the real renderer.
//!
//! The unit tests in `gents-session` prove the wording and that an ordinary body
//! is unchanged. These drive `render_request_context_message` -- the function the
//! daemon actually calls for every request -- with an `AgentRequest` built for a
//! seeded session, so the wiring from stored, verified provenance to the message
//! a child receives is what is under test. The receipt is signed by the node's
//! own key and checked by `verified_creation_origin`, so a session merely
//! labelled `PluginJob` would not pass.

use std::sync::Arc;

use gents_protocol::row::AgentRequest;
use gents_protocol::session::{SessionCreationCause, SessionReference};
use gents_session::session::origin::{verified_creation_origin, verified_plugin_job_child};
use gents_session::test_support::lineage_fixtures::{create_started_session_as, fixture_session};
use gents_store::config_client::ConfigAccess;
use identity::Did;

/// One session, created with a receipt of `cause`, in a node whose own key
/// signs it.
async fn seed_session(
    name: &str,
    cause: SessionCreationCause,
) -> (
    Arc<defra_node::Node>,
    String,
    String,
    String,
    gents_inference::test_support::TestDb,
) {
    let db = gents_inference::test_support::test_db(
        name,
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let node_did = gents_store::identity::NodeIdentity::did(db.node_identity.as_ref()).to_owned();
    let node = db.node.clone();
    let session_id = format!("session-{name}");
    // The node's own did:key as the selected agent: the request's commit is
    // signed as that agent, so it must be a real DID this process holds.
    let agent_did = node_did.clone();
    let requester_did = node_did.clone();
    let mut session = fixture_session(&node_did, &session_id);
    session.agent_did = agent_did.clone();
    let source = SessionReference {
        node_did: node_did.clone(),
        session_id: format!("caller-{name}"),
        requester_did: Some(requester_did.clone()),
    };
    // The selected agent must exist: request admission reads its Agent document.
    let access = ConfigAccess::local_as(
        node.clone(),
        Did::new(&node_did).expect("the node DID parses"),
    );
    access
        .write(
            "test.workflow.child.agent",
            &format!(
                r#"mutation {{ create_Agent(input: {{node_did: "{}", agent_did: "{}", creation_nonce: "nonce", context_id: "{}:context", inference_profile_id: "default:inference"}}) {{_docID}} }}"#,
                gents_store::graphql::escape_graphql_string(&node_did),
                gents_store::graphql::escape_graphql_string(&agent_did),
                gents_store::graphql::escape_graphql_string(&agent_did),
            ),
        )
        .await
        .expect("the agent is created");
    create_started_session_as(
        node.as_ref(),
        session,
        source,
        "parent-request",
        "opening-request",
        cause,
    )
    .await;
    (node, node_did, session_id, agent_did, db)
}

/// An `AgentRequest` for the seeded session: the request the daemon renders a
/// context message for.
///
/// Only the fields the renderer's provenance query reads are set, because those
/// are what decides the sentence: `node_did`, `session_id` and `requester_did`.
/// The rest is the request's own payload, which the context message does not
/// depend on.
fn request_for(session_id: &str, node_did: &str, agent_did: &str) -> AgentRequest {
    use gents_protocol::request_admission::RequestPurpose;
    let request = AgentRequest {
        purpose: RequestPurpose::Normal,
        doc_id: format!("doc-{session_id}"),
        request_id: format!("request-{session_id}"),
        node_did: node_did.to_owned(),
        requester_did: Some(node_did.to_owned()),
        agent_did: agent_did.to_owned(),
        session_id: session_id.to_owned(),
        content: "a child's task".to_owned(),
        input: Default::default(),
        created_at: "2026-11-17T00:00:00Z".to_owned(),
        retry_parent_request_doc_id: None,
        max_total_tokens: None,
        execution_origin: None,
        deadline: None,
        execution_generation: None,
        execution_lease_secs: None,
        execution_lease_expires_at: None,
        executed_model_name: None,
        executed_reasoning_effort: None,
        executed_service_tier: None,
        executed_inference_profile_id: None,
        executed_runtime_generation: None,
        request_hop: 0,
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
        workspace_owner_node_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    };
    request
}

/// The message the real renderer produces for `request`, or `None` when it
/// renders no context at all.
async fn rendered_context(node: &Arc<defra_node::Node>, request: &AgentRequest) -> Option<String> {
    let tool_config = gents_tools::surface::AgentToolSurfaceConfig::meta_only();
    let message =
        super::inference::render_request_context_message(node, &tool_config, request, None)
            .await
            .expect("the context renders");
    message.map(|message| format!("{message:?}"))
}

/// A verified plugin-job child's rendered request context carries the sentence.
#[tokio::test]
async fn a_verified_workflow_childs_rendered_context_carries_the_sentence() {
    let (node, node_did, session_id, agent_did, _db) =
        seed_session("plugin-job-child", SessionCreationCause::PluginJob).await;
    let access = ConfigAccess::local(node.clone());
    let scope = gents_protocol::session::SessionScope {
        node_did: node_did.clone(),
        session_id: session_id.clone(),
        requester_did: Some(node_did.clone()),
    };
    // The provenance is genuinely verified -- signed by the node's own key --
    // or the rest of this test would be asserting the wrong thing.
    assert!(
        verified_creation_origin(&access, &scope)
            .await
            .expect("the receipt reads")
            .is_some(),
        "the fixture receipt is verified, not merely present"
    );
    assert!(
        verified_plugin_job_child(&access, &scope)
            .await
            .expect("the receipt reads"),
        "a PluginJob receipt is a workflow child"
    );

    let request = request_for(&session_id, &node_did, &agent_did);
    let context = rendered_context(&node, &request)
        .await
        .expect("a request carries a context message");
    assert!(
        context.contains(gents_session::workspace::WORKFLOW_RESULT_RETURN_INSTRUCTION),
        "the rendered context carries the sentence: {context}"
    );
}

/// An `agent_new` child is verified and is not a workflow child, so its rendered
/// context omits the sentence.
#[tokio::test]
async fn an_agent_new_childs_rendered_context_omits_the_sentence() {
    let (node, node_did, session_id, agent_did, _db) =
        seed_session("agent-new-child", SessionCreationCause::AgentNew).await;
    let access = ConfigAccess::local(node.clone());
    let scope = gents_protocol::session::SessionScope {
        node_did: node_did.clone(),
        session_id: session_id.clone(),
        requester_did: Some(node_did.clone()),
    };
    assert!(
        verified_creation_origin(&access, &scope)
            .await
            .expect("the receipt reads")
            .is_some(),
        "an AgentNew receipt is verified too"
    );
    assert!(
        !verified_plugin_job_child(&access, &scope)
            .await
            .expect("the receipt reads"),
        "an AgentNew session is not a workflow child"
    );

    let request = request_for(&session_id, &node_did, &agent_did);
    if let Some(context) = rendered_context(&node, &request).await {
        assert!(
            !context.contains(gents_session::workspace::WORKFLOW_RESULT_RETURN_INSTRUCTION),
            "an ordinary session's context omits the sentence: {context}"
        );
    }
}

/// A session with no receipt at all -- one a client opened -- is not a child.
#[tokio::test]
async fn a_session_without_a_receipt_is_not_a_workflow_child() {
    let db = gents_inference::test_support::test_db(
        "no-receipt",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let node_did = db.node_identity.did().to_owned();
    let access = ConfigAccess::local(db.node.clone());
    let scope = gents_protocol::session::SessionScope {
        node_did: node_did.clone(),
        session_id: "never-created".to_owned(),
        requester_did: Some(node_did.clone()),
    };
    assert!(verified_creation_origin(&access, &scope)
        .await
        .expect("the read succeeds")
        .is_none());
    assert!(
        !verified_plugin_job_child(&access, &scope)
            .await
            .expect("the read succeeds"),
        "no receipt is not a child"
    );
}
