//! C2: the workflow return instruction, end to end through the real renderer.
//!
//! The unit tests prove the wording and that an ordinary body is unchanged.
//! These prove the wiring: a **genuinely verified** plugin-job child's request
//! context carries the sentence, and an `agent_new` child's does not. The
//! receipt is signed by the fixture key and verified by
//! `verified_creation_origin`, so a session merely labelled `PluginJob` would
//! not pass.

use std::sync::Arc;

use gents_protocol::session::{SessionCreationCause, SessionReference};
use gents_session::session::origin::{verified_creation_origin, verified_plugin_job_child};
use gents_session::test_support::lineage_fixtures::{create_started_session_as, fixture_session};
use gents_store::config_client::ConfigAccess;

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
    let agent_did = node_did.clone();
    let session = fixture_session(&node_did, &session_id);
    let source = SessionReference {
        node_did: node_did.clone(),
        session_id: format!("caller-{name}"),
        requester_did: Some(agent_did.clone()),
    };
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

/// A plugin-job child's session is verified as one, and its request carries the
/// sentence a workflow child needs.
#[tokio::test]
async fn a_verified_workflow_child_is_told_where_its_answer_returns() {
    let (node, node_did, session_id, agent_did, _db) =
        seed_session("c2-plugin-job-child", SessionCreationCause::PluginJob).await;
    let access = ConfigAccess::local(node.clone());
    let scope = gents_protocol::session::SessionScope {
        node_did: node_did.clone(),
        session_id: session_id.clone(),
        requester_did: Some(agent_did.clone()),
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

    // And the body a workflow child receives carries the sentence.
    let body =
        gents_session::workspace::instruction_body_for_request_with_origin(None, None, None, true)
            .expect("a workflow child always has a body");
    assert!(
        body.contains(gents_session::workspace::WORKFLOW_RESULT_RETURN_INSTRUCTION),
        "{body}"
    );
}

/// The same node, an `agent_new` session: verified, but not a workflow child,
/// and its body is unchanged.
#[tokio::test]
async fn an_agent_new_child_is_not_a_workflow_child() {
    let (node, node_did, session_id, agent_did, _db) =
        seed_session("c2-agent-new-child", SessionCreationCause::AgentNew).await;
    let access = ConfigAccess::local(node.clone());
    let scope = gents_protocol::session::SessionScope {
        node_did: node_did.clone(),
        session_id: session_id.clone(),
        requester_did: Some(agent_did.clone()),
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
    // Its body is the older function's, byte for byte.
    assert_eq!(
        gents_session::workspace::instruction_body_for_request_with_origin(None, None, None, false),
        gents_session::workspace::instruction_body_for_request(None, None, None),
        "an ordinary session is unchanged"
    );
}

/// A session with no receipt at all -- one a client opened -- is not a child.
#[tokio::test]
async fn a_session_without_a_receipt_is_not_a_workflow_child() {
    let db = gents_inference::test_support::test_db(
        "c2-no-receipt",
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
