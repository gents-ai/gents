use crate::call::hook::background_executions::BackgroundExecutionRegistry;
use crate::call::hook::fixtures::*;
use crate::call::hook::*;
use gents_inference::llm::ToolCallHookAction;
use gents_store::identity::NodeIdentity;
use std::sync::Arc;

pub struct BudgetHookFixture {
    pub db: gents_store::test_support::TestStore,
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
    let signed_home = gents_documents::test_support::SignedHome::start(name, composition).await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let request_id = format!("request-{name}-{}", uuid::Uuid::new_v4());
    let session_id = format!("session-{name}-{}", uuid::Uuid::new_v4());
    let claimed =
        gents_session::test_support::ClaimedRequest::client_signed(&signed_home, &binding)
            .ids(&request_id, &session_id)
            .content("spawn")
            .drive(gents_session::test_support::Stage::Claimed)
            .await;
    let (db, user, owner) = signed_home.into_parts();
    let node = db.node.clone();
    let agent = claimed.agent;
    let access = claimed.agent_access;
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
    bind_claimed_hook_request(&hook, claimed.lifecycle).await;
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

pub async fn invoke(
    hook: &DefraSessionHook,
    id: &str,
    name: &str,
    args: &str,
) -> serde_json::Value {
    accept_hook_tool_call(hook, id, name, args, None).await;
    let ToolCallHookAction::Skip { reason } = hook
        .on_tool_call(
            name,
            None,
            id,
            args,
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
    else {
        panic!("accepted {name} must return a tool result")
    };
    serde_json::from_str(&reason).unwrap()
}
