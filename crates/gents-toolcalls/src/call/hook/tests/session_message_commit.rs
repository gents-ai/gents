use super::*;
use crate::test_support::enable_agents_tools_with_access;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::identity::NodeIdentity;

async fn rows(
    access: &gents_store::config_client::ConfigAccess,
    query: &str,
    collection: &str,
) -> Vec<serde_json::Value> {
    let response = access.execute(query).await.unwrap();
    response["data"][collection]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Forces a request's lifecycle state as its owning Agent, staging the
/// queue positions the steering cases need.
async fn force_request_state(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
    state: &str,
) {
    access
        .write(
            "test.session_message.force_request_state",
            &format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "{}" }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(doc_id),
                gents_store::graphql::escape_graphql_string(state),
            ),
        )
        .await
        .unwrap();
}

async fn dispatch_with_lost_receipt(
    hook: &DefraSessionHook,
    call_id: &str,
    tool_name: &str,
    args: serde_json::Value,
    operation: &'static str,
) -> (String, serde_json::Value) {
    let args = args.to_string();
    accept_hook_tool_call(hook, call_id, tool_name, &args, None).await;
    let (action, fired) = ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
        Some(operation),
        hook.on_tool_call(
            tool_name,
            None,
            call_id,
            &args,
            &gents_loop::tool::ToolCallEnvelope::default(),
        ),
    )
    .await;
    assert!(
        fired,
        "{call_id} ({operation}): the receipt loss fired; action={action:?}"
    );
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("{operation}: the call is answered by its receipt");
    };
    let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
    assert_eq!(receipt["ok"], true, "{operation}: {reason}");
    assert!(receipt.get("retryable").is_none(), "{operation}: {reason}");

    let access = hook.actor_access().unwrap();
    let tool = rows(
        &access,
        &format!(
            r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{call_id}" }} }}) {{ _docID lifecycle_state }} }}"#
        ),
        "AgentToolCall",
    )
    .await;
    assert_eq!(tool.len(), 1, "{operation}");
    assert_eq!(tool[0]["lifecycle_state"], "running", "{operation}");
    let tool_doc_id = tool[0]["_docID"].as_str().unwrap().to_owned();
    let caused = rows(
        &access,
        &format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{tool_doc_id}" }} }}) {{ _docID input }} }}"#
        ),
        "AgentRequest",
    )
    .await;
    assert_eq!(caused.len(), 1, "{operation}: one caused request");
    assert_eq!(
        receipt["request_doc_id"], caused[0]["_docID"],
        "{operation}"
    );
    let segments = rows(
        &access,
        "{ AgentOutputSegment { source close } }",
        "AgentOutputSegment",
    )
    .await;
    let closures = segments
        .iter()
        .filter(|segment| {
            segment["source"].to_string().contains(&tool_doc_id) && !segment["close"].is_null()
        })
        .count();
    assert_eq!(
        closures, 1,
        "{operation}: the receipt's closure only, no failed-dispatch closure"
    );
    (tool_doc_id, caused[0].clone())
}

#[tokio::test]
async fn agent_new_inherits_the_signed_caller_directory() {
    let home = gents_session::test_support::AgentHome::start("agent-new-cwd").await;
    let node = home.node.clone();
    let did = home.node_did().to_owned();
    let session = format!("session-{}", uuid::Uuid::new_v4());
    let claimed = gents_session::test_support::ClaimedRequest::client_signed(&home, &home.binding)
        .ids("cwd-parent", &session)
        .input(gents_protocol::request_input::RequestInput {
            cwd: Some(home.data_path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .drive(gents_session::test_support::Stage::Claimed)
        .await;
    let hook = DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        &session,
        &did,
        Some(home.user.did()),
        home.agent_did().parse().unwrap(),
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_managed_identity(home.agent_identity.as_ref())
    .unwrap();
    enable_agents_tools_with_access(&home.user_access, &did, home.agent_did()).await;
    bind_claimed_hook_request(&hook, claimed.lifecycle).await;
    hook.on_completion_call(&user_text_message("start agents"), &[])
        .await;
    let (_, caused) = dispatch_with_lost_receipt(
        &hook,
        "cwd-normal",
        gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
        json!({"agent": "general", "prompt": "work"}),
        "session_message.commit_request",
    )
    .await;
    assert_eq!(
        caused["input"]["cwd"],
        home.data_path().to_string_lossy().as_ref()
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn a_lost_commit_receipt_is_decided_by_the_durable_row() {
    let home = gents_session::test_support::AgentHome::start("lost-commit-receipt").await;
    let node = home.node.clone();
    let did = home.node_did().to_owned();
    let session = format!("session-{}", uuid::Uuid::new_v4());
    let claimed = gents_session::test_support::ClaimedRequest::client_signed(&home, &home.binding)
        .ids("request-commit-receipt-loss", &session)
        .drive(gents_session::test_support::Stage::Claimed)
        .await;
    let agent_signer = claimed.lifecycle.managed_agent_signer().unwrap();
    let hook = DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        &session,
        &did,
        Some(home.user.did()),
        home.agent_did().parse().unwrap(),
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_managed_identity(home.agent_identity.as_ref())
    .unwrap();
    enable_agents_tools_with_access(&home.user_access, &did, home.agent_did()).await;
    bind_claimed_hook_request(&hook, claimed.lifecycle).await;
    hook.on_completion_call(&user_text_message("start agents"), &[])
        .await;

    let (tool_doc_id, caused) = dispatch_with_lost_receipt(
        &hook,
        "new-call",
        gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
        json!({ "agent": "general", "prompt": "work" }),
        "session_message.commit_request",
    )
    .await;
    let caused_row = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &hook.actor_access().unwrap(),
        caused["_docID"].as_str().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    crate::call::lifecycle::admission_fixture::complete_child_managed_verified(
        &node,
        &caused_row.request_id,
        "done",
        &agent_signer,
        home.node_identity.clone(),
    )
    .await;
    for expected in [1, 0] {
        assert_eq!(
            crate::background::completion::settle_running_session_message_rows_with_access(
                &node,
                &hook.actor_access().unwrap(),
                &gents_session::session::message::NoSessionMessageGoals,
                &did
            )
            .await
            .unwrap(),
            expected
        );
    }
    let access = hook.actor_access().unwrap();
    let notifications = rows(
        &access,
        &format!(
            r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            gents_protocol::background_completion::background_completion_notification_message_key(
                &tool_doc_id,
                "tool"
            )
        ),
        "AgentMessage",
    )
    .await;
    assert_eq!(notifications.len(), 1, "one completion notification");

    force_request_state(&access, &caused_row.doc_id, "processing").await;
    let (_, steering) = dispatch_with_lost_receipt(
        &hook,
        "steer-call",
        gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
        json!({ "session_id": caused_row.session_id, "message": "change course" }),
        "session_message.commit_request",
    )
    .await;
    assert_eq!(steering["input"]["queue"]["source"], "steering");
    assert_eq!(
        steering["input"]["queue"]["queued_after_request_id"],
        caused_row.request_id.as_str()
    );
    let (_, explicit) = dispatch_with_lost_receipt(
        &hook,
        "explicit-steer-call",
        gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
        json!({ "session_id": caused_row.session_id, "message": "interrupt the tool boundary", "delivery": "steer" }),
        "session_message.commit_request",
    ).await;
    assert_eq!(explicit["input"]["queue"]["delivery"], "steer");
    assert_eq!(
        explicit["input"]["queue"]["queued_after_request_id"],
        caused_row.request_id.as_str()
    );
    let (_, second) = dispatch_with_lost_receipt(
        &hook,
        "second-explicit-steer-call",
        gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
        json!({ "session_id": caused_row.session_id, "message": "and another correction", "delivery": "steer" }),
        "session_message.commit_request",
    ).await;
    assert_eq!(second["input"]["queue"]["delivery"], "steer");
    let ordered = access
        .transact_readonly("test.steer_order", |txn| {
            let did = &did;
            let session_id = &caused_row.session_id;
            Box::pin(async move {
                gents_session::request::lifecycle::queue::pending_in_txn(txn, did, session_id).await
            })
        })
        .await
        .unwrap();
    let ids = ordered
        .iter()
        .filter_map(|row| row.doc_id.as_deref())
        .collect::<Vec<_>>();
    assert!(
        ids.iter()
            .position(|id| *id == explicit["_docID"].as_str().unwrap())
            .unwrap()
            < ids
                .iter()
                .position(|id| *id == second["_docID"].as_str().unwrap())
                .unwrap()
    );
    force_request_state(&access, &caused_row.doc_id, "completed").await;
    let mut incompatible = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "incompatible-head",
        gents_protocol::request_admission::RequestScope::new(
            &did,
            agent_signer.did(),
            agent_signer.did(),
            &caused_row.session_id,
        ),
        "work in another directory",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            agent_signer.did(),
        ),
    );
    incompatible.input.cwd = Some("/different-cwd".to_owned());
    gents_session::request::admission::sign_agent_request_create(&agent_signer, &mut incompatible)
        .await
        .unwrap();
    let created = access
        .write(
            "test.session_message.incompatible_head",
            &incompatible.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let incompatible_doc = gents_store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    force_request_state(&access, &incompatible_doc, "processing").await;
    let args =
        json!({ "session_id": caused_row.session_id, "message": "bad scope", "delivery": "steer" })
            .to_string();
    accept_hook_tool_call(
        &hook,
        "incompatible-steer",
        gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
        &args,
        None,
    )
    .await;
    let action = hook
        .on_tool_call(
            gents_protocol::tool_names::AGENT_MESSAGE_TOOL_NAME,
            None,
            "incompatible-steer",
            &args,
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await;
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("incompatible steer must be refused")
    };
    let refusal: serde_json::Value = serde_json::from_str(&reason).unwrap();
    assert_eq!(
        refusal["failure_class"], "invalid_tool_arguments",
        "{reason}"
    );
    assert_eq!(refusal["path"], "/delivery", "{reason}");
    assert_eq!(refusal["retryable"], false, "{reason}");
    assert!(
        refusal["message"]
            .as_str()
            .unwrap()
            .contains("delivery=queue"),
        "{reason}"
    );
    let previous = rows(
        &access,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            steering["_docID"].as_str().unwrap()
        ),
        "AgentRequest",
    )
    .await;
    assert_eq!(previous[0]["lifecycle_state"], "pending");
    node.shutdown().await.expect("shut down test node");
}
