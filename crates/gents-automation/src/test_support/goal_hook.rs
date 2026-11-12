use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_toolcalls::call::hook::fixtures::{claim_request_fixture, ensure_runtime_session};
use gents_toolcalls::call::hook::DefraSessionHook;

pub async fn bind_signed_request(
    node: &Arc<EmbeddedNode>,
    hook: &DefraSessionHook,
    identity: &KeyIdentity,
    request_id: &str,
    session: &str,
    reference: Option<GoalAssignmentInput>,
) {
    let access = ConfigAccess::local(node.clone());
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        identity.did(),
        identity.did(),
        identity.did(),
        session,
        "Goal control request",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.input.goal_assignment = reference;
    gents_session::request::admission::sign_agent_request_create(identity, &mut request)
        .await
        .unwrap();
    let response = access
        .write(
            "test.goal_tool_bound_request",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    access
        .transact_write("test.goal_tool_bind_existing_goal", |txn| {
            Box::pin(async {
                if let Some(goal) =
                    crate::goal::load_canonical_goal_in_txn(txn, identity.did(), session).await?
                {
                    if goal.assignment_root_request_doc_id.is_none() {
                        crate::goal::bind_goal_first_request_in_txn(
                            txn,
                            &goal,
                            &doc,
                            request.input.goal_assignment.as_ref(),
                        )
                        .await?;
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    access.write("test.goal_tool_finish_previous_requests", &format!(r#"mutation {{update_AgentRequest(filter: {{_docID: {{_neq: "{}"}}}}, input: {{lifecycle_state: "completed"}}) {{_docID}}}}"#,
        gents_store::graphql::escape_graphql_string(&doc))).await.unwrap();
    claim_request_fixture(node, hook, request_id, session, true).await;
}

/// Writes the session's Goal as the runtime that signs the session.
pub async fn set_goal_as_session_owner(
    node: &Arc<EmbeddedNode>,
    owner: &KeyIdentity,
    session_id: &str,
    objective: Option<&str>,
    status: Option<gents_protocol::goal_status::GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> anyhow::Result<crate::goal::GoalDocument> {
    let access = ensure_runtime_session(node, owner, session_id).await?;
    crate::goal::seed_goal_from_access(
        &access,
        owner.did(),
        session_id,
        objective,
        status,
        token_budget,
        None,
    )
    .await
}
