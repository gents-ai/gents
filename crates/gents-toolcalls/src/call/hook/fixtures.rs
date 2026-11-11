use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;
use std::sync::OnceLock;

use gents_inference::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, UserContent,
};
use gents_loop::provider_input::ProviderInputProfile;
use gents_store::identity::{KeyIdentity, NodeIdentity};

use super::*;

pub struct HookExecutionFixture {
    pub lifecycle: gents_session::request::lifecycle::RequestLifecycle,
    pub writer: gents_session::request::streaming::DefraStreamWriter,
    pub turn: usize,
}

pub fn hook_execution_fixtures(
) -> &'static tokio::sync::Mutex<RapidHashMap<String, HookExecutionFixture>> {
    static FIXTURES: OnceLock<tokio::sync::Mutex<RapidHashMap<String, HookExecutionFixture>>> =
        OnceLock::new();
    FIXTURES.get_or_init(|| tokio::sync::Mutex::new(RapidHashMap::new()))
}

pub fn hook_execution_fixture_key(hook: &DefraSessionHook, request_id: &str) -> String {
    format!("{:p}:{request_id}", Arc::as_ptr(&hook.node))
}

pub async fn hook_managed_identity(hook: &DefraSessionHook, request_id: &str) -> Arc<dyn NodeIdentity> {
    let fixtures = hook_execution_fixtures().lock().await;
    let fixture = fixtures
        .get(&hook_execution_fixture_key(hook, request_id))
        .expect("owned hook fixture");
    Arc::new(
        fixture
            .lifecycle
            .managed_agent_signer()
            .expect("fixture retains managed Agent signer"),
    )
}

pub async fn bind_managed_hook_request(
    hook: &mut DefraSessionHook,
    request_id: &str,
    root: &dyn NodeIdentity,
    deadline: chrono::DateTime<chrono::Utc>,
) {
    let session = hook.session_id().await.expect("hook session");
    let lifecycle = crate::call::lifecycle::admission_fixture::claimed_signed_request(
        &hook.node, request_id, &session, root, None,
    )
    .await;
    let signer = lifecycle
        .managed_agent_signer()
        .expect("fixture retains managed Agent signer");
    hook.actor = Some(identity::Did::new(signer.did()).unwrap());
    hook.agent_signer = Some(signer);
    bind_claimed_hook_request(hook, lifecycle, deadline).await;
}

pub fn user_text_message(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

/// Reloads a tool-call row as the hook's selected Agent, bound to the same
/// managed signer so the reloaded lifecycle writes as the row's owner.
pub async fn reload_tool_call_as_hook(
    hook: &DefraSessionHook,
    doc_id: &str,
    session_id: &str,
) -> crate::call::lifecycle::ToolCallLifecycle {
    let mut lifecycle = crate::call::lifecycle::ToolCallLifecycle::load_by_doc_id_as(
        hook.node.clone(),
        hook.actor.clone().expect("a signed hook selects an Agent"),
        doc_id,
        &hook.node_did,
        session_id,
        hook.active_requester_did().await.as_deref(),
    )
    .await
    .unwrap()
    .expect("the hook's Agent sees its tool call");
    lifecycle
        .bind_agent_signer(hook.agent_signer.clone())
        .unwrap();
    lifecycle
}

/// Runs a read as the hook's selected Agent and returns its `data` object.
pub async fn hook_query(hook: &DefraSessionHook, query: &str) -> serde_json::Value {
    let response = hook
        .actor_access()
        .expect("a signed hook reads as its selected Agent")
        .execute(query)
        .await
        .expect("query as the hook's Agent");
    response["data"].clone()
}

/// Loads the hook's transcript as its selected Agent, which owns the session.
pub async fn hook_history(hook: &DefraSessionHook, session_id: &str) -> anyhow::Result<Vec<Message>> {
    gents_session::session::load_history_with_access(
        &hook.actor_access()?,
        session_id,
        &hook.node_did,
        hook.actor.as_ref().map(identity::Did::as_str),
    )
    .await
}

/// A hook whose session belongs to an Agent derived from `node`'s held
/// runtime root, bound to that Agent's managed signer the way the daemon binds
/// one before it claims work.
pub async fn signed_hook(node: &Arc<EmbeddedNode>, failure_policy: FailurePolicy) -> DefraSessionHook {
    let node_did = node
        .node_identity_did()
        .expect("fixture node has a runtime identity")
        .to_owned();
    signed_hook_as(node, &node_did, failure_policy).await
}

pub async fn signed_hook_as(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    failure_policy: FailurePolicy,
) -> DefraSessionHook {
    let root = gents_store::test_support::held_runtime_identity(node_did);
    let mut hook = DefraSessionHook::with_identity(node.clone(), root.did(), failure_policy);
    let session_id = hook.session_id().await.expect("hook session");
    let agent = crate::call::lifecycle::admission_fixture::ensure_managed_fixture_session(
        node,
        &session_id,
        &root,
        Some(root.did()),
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
    .await;
    hook.actor = Some(identity::Did::new(agent.did()).unwrap());
    hook.with_managed_identity(agent.as_ref())
        .expect("bind the hook to its Agent's managed signer")
}

/// Creates and claims `request_id` as the hook's selected Agent. The request
/// is a local-self request: the Agent is its own requester.
pub async fn claimed_hook_request(
    hook: &DefraSessionHook,
    request_id: &str,
    session_id: &str,
) -> gents_session::request::lifecycle::RequestLifecycle {
    let root = gents_store::test_support::held_runtime_identity(&hook.node_did);
    let create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        root.did(),
        root.did(),
        root.did(),
        session_id,
        "child request",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(root.did()),
    );
    let signer = hook
        .agent_signer
        .clone()
        .expect("a signed hook holds its Agent's managed signer");
    crate::call::lifecycle::admission_fixture::claimed_signed_request_create_as_agent(
        &hook.node, &root, &signer, create,
    )
    .await
}

/// Writes a child request the hook's Agent caused through `tool_call_doc_id`,
/// signed and owned by that Agent and left unclaimed in the parent's session.
pub async fn create_corroborated_child_request(
    hook: &DefraSessionHook,
    child_request_id: &str,
    session_id: &str,
    parent_request_id: &str,
    parent_request_doc_id: &str,
    tool_call_id: &str,
    tool_call_doc_id: &str,
) {
    let signer = hook
        .agent_signer
        .clone()
        .expect("a signed hook holds its Agent's managed signer");
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        child_request_id,
        &hook.node_did,
        signer.did(),
        signer.did(),
        session_id,
        "child request",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(signer.did()),
    );
    create.caused_by_parent_request_id = Some(parent_request_id.to_owned());
    create.caused_by_parent_request_doc_id = Some(parent_request_doc_id.to_owned());
    create.caused_by_parent_tool_call_id = Some(tool_call_id.to_owned());
    create.caused_by_parent_tool_call_doc_id = Some(tool_call_doc_id.to_owned());
    create.request_hop = 1;
    gents_session::request::admission::sign_agent_request_create(&signer, &mut create)
        .await
        .expect("sign the child request as the hook's Agent");
    hook.actor_access()
        .unwrap()
        .write(
            "test.hook.corroborated_child",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .expect("create the child request as the hook's Agent");
}

pub async fn bind_interruptible_request(
    hook: &DefraSessionHook,
    request_id: &str,
    session_id: &str,
    deadline_at: chrono::DateTime<chrono::Utc>,
) {
    let lifecycle = claimed_hook_request(hook, request_id, session_id).await;
    bind_claimed_hook_request(hook, lifecycle, deadline_at).await;
}

pub async fn bind_claimed_hook_request(
    hook: &DefraSessionHook,
    mut lifecycle: gents_session::request::lifecycle::RequestLifecycle,
    deadline_at: chrono::DateTime<chrono::Utc>,
) {
    let request_id = lifecycle.request().request_id.clone();
    let request_doc_id = lifecycle.request().doc_id.clone();
    let requester_did = lifecycle.request().requester_did.clone();
    assert!(
        !request_doc_id.is_empty(),
        "claimed request has a physical identity"
    );
    if let Some(actor) = &hook.actor {
        assert_eq!(actor.as_str(), lifecycle.request().agent_did);
        let access = hook.actor_access().expect("hook's selected Agent access");
        let observed = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
            &access,
            &request_doc_id,
        )
        .await
        .expect("read claimed request as selected Agent")
        .expect("selected Agent must see its claimed request");
        assert_eq!(observed.request_id, request_id);
        assert_eq!(observed.session_id, lifecycle.request().session_id);
        assert_eq!(observed.agent_did, lifecycle.request().agent_did);
        assert_eq!(observed.requester_did, requester_did);
    }
    let writer = gents_session::request::streaming::DefraStreamWriter::new_for_lifecycle(
        hook.node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    hook.set_active_request_binding(
        Some(request_id.clone()),
        Some(request_doc_id),
        requester_did,
    )
    .await;
    hook.set_request_deadline_at(Some(deadline_at)).await;
    hook_execution_fixtures().lock().await.insert(
        hook_execution_fixture_key(hook, &request_id),
        HookExecutionFixture {
            lifecycle,
            writer,
            turn: 0,
        },
    );
}

pub async fn publish_claimed_authored_input(
    hook: &DefraSessionHook,
    request_id: &str,
    context: Option<Message>,
    prompt: Message,
) {
    use gents_loop::loop_stream::LoopStreamItem;
    use gents_loop::stream_processor::StreamProcessor;

    let mut fixtures = hook_execution_fixtures().lock().await;
    let fixture = fixtures
        .get_mut(&hook_execution_fixture_key(hook, request_id))
        .expect("claimed request requires owned execution fixture");
    let request_doc_id = fixture.lifecycle.request().doc_id.clone();
    let HookExecutionFixture {
        lifecycle, writer, ..
    } = fixture;
    let mut processor = StreamProcessor::new(
        hook,
        writer,
        lifecycle,
        &request_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );
    processor
        .process_item(Ok(LoopStreamItem::AuthoredInputReady {
            context,
            prompt,
            folded: Vec::new(),
            reply: tokio::sync::oneshot::channel().0,
        }))
        .await
        .expect("publish claimed authored input");
}

pub async fn accept_hook_tool_call(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    tool_name: &str,
    arguments: &str,
    provider_call_id: Option<&str>,
) {
    let native_id = provider_call_id.unwrap_or(internal_call_id).to_string();
    let message = Message::Assistant {
        request_id: None,
        id: Some(format!("message-{internal_call_id}")),
        content: vec![AssistantContent::ToolCall(ToolCall {
            raw_arguments: None,
            id: native_id,
            call_id: provider_call_id.map(str::to_string),
            function: ToolFunction {
                name: tool_name.to_string(),
                arguments: serde_json::from_str(arguments).expect("test tool arguments JSON"),
            },
            signature: None,
            additional_params: None,
        })],
    };
    publish_and_adopt_tool_turn(hook, internal_call_id, provider_call_id, message).await;
}

pub async fn accepted_hook_tool_lifecycle(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    tool_name: &str,
    arguments: &str,
    deadline_at: chrono::DateTime<chrono::Utc>,
    await_mode: gents_protocol::tool_call::AwaitMode,
) -> crate::call::lifecycle::ToolCallLifecycle {
    accept_hook_tool_call(hook, internal_call_id, tool_name, arguments, None).await;
    let state = hook.state.lock().await;
    let request_id = state
        .current_request_id
        .clone()
        .expect("accepted test tool requires active request");
    let session_id = state
        .session_id
        .clone()
        .expect("accepted test tool requires active session");
    drop(state);
    hook.adopt_accepted_tool_dispatch(
        internal_call_id,
        None,
        &request_id,
        &session_id,
        tool_name,
        arguments,
        deadline_at,
        await_mode,
    )
    .await
    .expect("adopt published test tool")
}

pub async fn accepted_session_message_lifecycle(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    deadline_at: chrono::DateTime<chrono::Utc>,
) -> crate::call::lifecycle::ToolCallLifecycle {
    let arguments = serde_json::json!({ "agent": "general", "prompt": "work" }).to_string();
    accepted_hook_tool_lifecycle(
        hook,
        internal_call_id,
        gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
        &arguments,
        deadline_at,
        gents_protocol::tool_call::AwaitMode::Background,
    )
    .await
}

pub async fn publish_and_adopt_tool_turn(
    hook: &DefraSessionHook,
    internal_call_id: &str,
    provider_call_id: Option<&str>,
    message: Message,
) {
    publish_and_adopt_tool_turns(hook, &[(internal_call_id, provider_call_id)], message).await;
}

pub async fn publish_and_adopt_tool_turns(
    hook: &DefraSessionHook,
    calls: &[(&str, Option<&str>)],
    message: Message,
) -> u32 {
    let request_id = hook
        .state
        .lock()
        .await
        .current_request_id
        .clone()
        .expect("accepted test tool requires active request");
    let mut fixtures = hook_execution_fixtures().lock().await;
    let fixture = fixtures
        .get_mut(&hook_execution_fixture_key(hook, &request_id))
        .expect("active request requires owned execution fixture");
    let turn = fixture.turn;
    fixture.turn += 1;
    fixture
        .writer
        .start_provider_attempt(
            &fixture.lifecycle.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse().unwrap(),
        )
        .await;
    let published = fixture
        .writer
        .publish_native_turn(&fixture.lifecycle, turn, 0, &message)
        .await
        .unwrap();
    assert_eq!(published.accepted_tools.len(), calls.len());
    let sequence = published.sequence;
    drop(fixtures);
    let mut adopted = Vec::with_capacity(calls.len());
    for ((internal_call_id, provider_call_id), accepted) in
        calls.iter().copied().zip(published.accepted_tools)
    {
        assert_eq!(accepted.call_id.as_deref(), provider_call_id);
        if let Some(provider_call_id) = provider_call_id {
            assert_eq!(accepted.id, provider_call_id);
        }
        hook.register_stream_tool_call_identity(internal_call_id, &accepted.id, provider_call_id)
            .await;
        adopted.push((internal_call_id.to_string(), accepted));
    }
    hook.adopt_accepted_tool_calls(adopted).await.unwrap();
    sequence
}

pub async fn publish_claimed_provider_turn(
    hook: &DefraSessionHook,
    request_id: &str,
    message: Message,
) {
    let mut fixtures = hook_execution_fixtures().lock().await;
    let fixture = fixtures
        .get_mut(&hook_execution_fixture_key(hook, request_id))
        .expect("claimed request requires owned execution fixture");
    let turn = fixture.turn;
    fixture.turn += 1;
    fixture
        .writer
        .start_provider_attempt(
            &fixture.lifecycle.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse().unwrap(),
        )
        .await;
    fixture
        .writer
        .publish_native_turn(&fixture.lifecycle, turn, 0, &message)
        .await
        .expect("publish claimed provider turn");
}

/// Reads one tool-call row as the hook's selected Agent, which owns it.
pub async fn fetch_tool_call_row(
    hook: &DefraSessionHook,
    session_id: &str,
    tool_call_id: &str,
) -> serde_json::Value {
    let access = hook
        .actor_access()
        .expect("a signed hook reads as its selected Agent");
    fetch_tool_call_row_as(&access, session_id, tool_call_id).await
}

pub async fn fetch_tool_call_row_as(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
    tool_call_id: &str,
) -> serde_json::Value {
    let session_id = gents_store::graphql::escape_graphql_string(session_id);
    let tool_call_id = gents_store::graphql::escape_graphql_string(tool_call_id);
    let resp = access
        .execute(&format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        tool_call_id: {{ _eq: "{tool_call_id}" }}
                    }},
                    limit: 1
                ) {{
                    _docID
                    request_id
                    request_doc_id
                    node_did
                    requester_did
                    session_id
                    message_sequence
                    deadline_at
                    lifecycle_state
                    status
                    tool_failure_class
                    denial_reason
                    selected_service_id
                    selected_tool_name
                    cancel_cause
                    await_mode
                }}
            }}"#
        ))
        .await
        .expect("query tool call as its Agent");
    let mut row = resp
        .pointer("/data/AgentToolCall/0")
        .cloned()
        .expect("tool call row");
    let output =
        gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
            access,
            row["_docID"].as_str().unwrap(),
            row["request_doc_id"].as_str().unwrap(),
            row["session_id"].as_str().unwrap(),
            row["node_did"].as_str().unwrap(),
            row["requester_did"].as_str(),
        )
        .await
        .ok()
        .map(gents_session::session::tool_output_rows::CanonicalToolOutputObservation::into_text);
    row["result"] = output.map_or(serde_json::Value::Null, serde_json::Value::String);
    row
}

/// Publishes an unsigned hook's session as the runtime it runs as. That hook
/// never writes its session, and an unsigned reader cannot resolve one.
pub async fn ensure_runtime_session(
    node: &Arc<EmbeddedNode>,
    owner: &KeyIdentity,
    session_id: &str,
) -> anyhow::Result<gents_store::config_client::ConfigAccess> {
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(owner.did())?,
    );
    let session = gents_store::graphql::escape_graphql_string(session_id);
    let did = gents_store::graphql::escape_graphql_string(owner.did());
    let existing = access
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{did}" }}, session_id: {{ _eq: "{session}" }} }}) {{ _docID }} }}"#
        ))
        .await?;
    if existing["data"]["AgentSession"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        access
            .write(
                "test.hook.runtime_session",
                &format!(
                    r#"mutation {{ create_AgentSession(input: {{ session_id: "{session}", node_did: "{did}", requester_did: "{did}", agent_did: "{did}", created_at: "{}" }}) {{ _docID }} }}"#,
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                ),
            )
            .await?;
    }
    Ok(access)
}


pub async fn claim_request_fixture(
    node: &Arc<EmbeddedNode>,
    hook: &DefraSessionHook,
    request_id: &str,
    session: &str,
    signature_valid: bool,
) {
    let access = gents_store::config_client::ConfigAccess::local(node.clone());
    let response = access.execute(&format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{{}}}}}"#,
        gents_store::graphql::escape_graphql_string(request_id), gents_store::graphql::escape_graphql_string(session), gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    assert_eq!(
        gents_store::identity::request_receipt::verify_request_receipt_signature(&row).is_ok(),
        signature_valid
    );
    let mut lifecycle = gents_session::request::lifecycle::RequestLifecycle::new_with_node_did(
        node.clone(),
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        gents_session::request::lifecycle::ClaimOutcome::Claimed
    );
    let writer = gents_session::request::streaming::DefraStreamWriter::new(
        node.clone(),
        &hook.node_did,
        std::time::Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    hook.set_active_request_lineage(Some(request_id.into()), Some(hook.node_did.clone()))
        .await
        .unwrap();
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    hook_execution_fixtures().lock().await.insert(
        hook_execution_fixture_key(hook, request_id),
        HookExecutionFixture {
            lifecycle,
            writer,
            turn: 0,
        },
    );
}
