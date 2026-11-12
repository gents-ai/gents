use crate::call::hook::fixtures::*;
use crate::call::hook::*;
use chrono::Utc;
use gents_store::identity::NodeIdentity;
use std::sync::Arc;

pub struct BudgetHookFixture {
    pub db: gents_inference::test_support::TestDb,
    pub user: gents_store::identity::KeyIdentity,
    pub owner: gents_store::config_client::ConfigAccess,
    pub agent: Arc<dyn NodeIdentity>,
    pub access: gents_store::config_client::ConfigAccess,
    pub hook: DefraSessionHook,
    pub request_id: String,
    pub session_id: String,
}

pub async fn budget_hook_fixture(
    name: &str,
    registry: BackgroundToolRegistry,
    executions: BackgroundExecutionRegistry,
    composition: gents_store::node_extension::Extensions,
) -> BudgetHookFixture {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let (db, user, owner, binding) =
        gents_inference::test_support::signed_test_node(name, composition).await;
    let node = db.node.clone();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let request_id = format!("request-{name}-{}", uuid::Uuid::new_v4());
    let session_id = format!("session-{name}-{}", uuid::Uuid::new_v4());
    let create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        &request_id,
        db.node_identity.did(),
        user.did(),
        agent.did(),
        &session_id,
        "spawn",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let mut lifecycle = crate::call::lifecycle::admission_fixture::pending_client_fixture_request(
        &node,
        &owner,
        db.node_identity.clone(),
        &user,
        agent.as_ref(),
        create,
    )
    .await
    .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    let hook = DefraSessionHook::resume_with_actor_policy(
        node,
        &session_id,
        db.node_identity.did(),
        Some(user.did()),
        agent.did().parse().unwrap(),
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_managed_identity(agent.as_ref())
    .unwrap()
    .with_background_tool_registry(registry)
    .with_background_execution_registry(executions);
    bind_claimed_hook_request(&hook, lifecycle, Utc::now() + chrono::Duration::minutes(5)).await;
    BudgetHookFixture {
        db,
        user,
        owner,
        agent,
        access,
        hook,
        request_id,
        session_id,
    }
}

pub async fn tool_row(
    access: &gents_store::config_client::ConfigAccess,
    session: &str,
    tool: &str,
) -> serde_json::Value {
    let session = gents_store::graphql::escape_graphql_string(session);
    let tool = gents_store::graphql::escape_graphql_string(tool);
    let response = access.execute(&format!(r#"{{ AgentToolCall(filter: {{session_id: {{_eq: "{session}"}}, tool_call_id: {{_eq: "{tool}"}}}}, limit: 2) {{ _docID request_doc_id node_did requester_did session_id lifecycle_state await_mode deadline_at selected_service_id selected_tool_name }} }}"#)).await.unwrap();
    let [row] = response["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("selected Agent must read exactly one tool call: {response}");
    };
    row.clone()
}

