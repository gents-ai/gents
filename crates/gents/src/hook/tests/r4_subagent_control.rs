//! Accepted native subagent control fixtures. These exercise the real hook
//! with an owned request and a StreamProcessor-published provider header.

use super::*;
use crate::support::fixtures::{
    configure_subagent_behavior, spawn_subagent_source, subagent_target,
};
use crate::support::test_db;
use crate::tool_call_lifecycle::ToolCallLifecycle;

pub(super) async fn accepted_subagent_hook(
    label: &str,
) -> (crate::support::TestDb, DefraSessionHook, String) {
    accepted_subagent_hook_with_child_spawning(label, false).await
}

async fn accepted_subagent_hook_with_child_spawning(
    label: &str,
    child_can_spawn: bool,
) -> (crate::support::TestDb, DefraSessionHook, String) {
    let db = test_db(label).await;
    let agent_did = db.node_identity.did().to_string();
    const CHILD: &str = "r4-accepted-control-child";
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        CHILD,
        "r4-accepted-control-child-tools",
        if child_can_spawn {
            vec![subagent_target(&agent_did, CHILD, &agent_did, CHILD)]
        } else {
            Vec::new()
        },
        child_can_spawn,
        child_can_spawn,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        "general",
        "r4-accepted-control-parent-tools",
        vec![subagent_target(&agent_did, CHILD, &agent_did, CHILD)],
        true,
        true,
        None,
    )
    .await;
    let session_id = format!("{label}-session");
    crate::session::ensure_session_with_behavior_id_and_requester_did(
        db.node.as_ref(),
        &session_id,
        "general",
        &agent_did,
        "general",
        Some(&agent_did),
    )
    .await
    .expect("ensure accepted parent session");
    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        "general",
        &agent_did,
        Some(&agent_did),
        FailurePolicy::default(),
    )
    .await
    .expect("resume configured subagent hook");
    super::r4c_private_support::bind_accepted_request(
        &db,
        &hook,
        "general",
        &format!("{label}-request"),
        &session_id,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    (db, hook, session_id)
}

pub(super) fn skip_json(action: ToolCallHookAction) -> serde_json::Value {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected accepted control result, got {action:?}");
    };
    serde_json::from_str(&reason)
        .unwrap_or_else(|error| panic!("control result JSON: {error}; raw={reason:?}"))
}

pub(super) async fn wait_for_child_materialized(
    node: &crate::defra_node::EmbeddedNode,
    child_request_id: &str,
) {
    let child_id_filter = crate::graphql::escape_graphql_string(child_request_id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_id_filter}" }} }}, limit: 1) {{ session_id }} }}"#
            ))
            .await;
        assert!(
            !response.has_errors(),
            "load materialized child: {:?}",
            response.errors
        );
        let child_session = response
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"].as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row["session_id"].as_str());
        if child_session.is_some_and(|value| !value.is_empty()) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "accepted bridge did not materialize its child"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

pub(super) async fn wait_for_bridge_child_id(
    node: &crate::defra_node::EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> String {
    let session = crate::graphql::escape_graphql_string(session_id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let response = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ tool_call_id child_request_id }} }}"#
            ))
            .await;
        assert!(
            !response.has_errors(),
            "load accepted bridge: {:?}",
            response.errors
        );
        let child = response
            .data
            .as_ref()
            .and_then(|data| data["AgentToolCall"].as_array())
            .and_then(|rows| rows.iter().find(|row| row["tool_call_id"] == tool_call_id))
            .and_then(|row| row["child_request_id"].as_str())
            .filter(|child| !child.is_empty());
        if let Some(child) = child {
            return child.to_owned();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "accepted bridge child id did not appear"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn seed_signed_child_queue_request(
    db: &crate::support::TestDb,
    request_id: &str,
    child_session_id: &str,
    source: gents_protocol::request_input::QueueSource,
    policy: gents_protocol::request_input::QueuePolicy,
    key: Option<&str>,
    queued_after_request_id: &str,
) {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
    use gents_protocol::request_input::{QueueSource, RequestQueue};

    let did = db.node_identity.did();
    let mut request = AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        did,
        did,
        "r4-accepted-control-child",
        child_session_id,
        "queued child work",
        if source == QueueSource::BackgroundCompletion {
            "scheduled"
        } else {
            "interactive"
        },
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(did),
    );
    request.subagent_depth = 1;
    request.input.queue = Some(RequestQueue {
        source,
        policy,
        key: key.map(str::to_owned),
        queued_after_request_id: Some(queued_after_request_id.to_owned()),
        interrupted_request_id: None,
        background_completion_wake_version: (source == QueueSource::BackgroundCompletion)
            .then_some(1),
    });
    crate::sign_agent_request_create(db.node_identity.as_ref(), &mut request)
        .await
        .expect("sign queued child request");
    let response = db.node.execute(&request.graphql_mutation().unwrap()).await;
    assert!(
        !response.has_errors(),
        "create signed queued child request: {:?}",
        response.errors
    );
}

async fn accepted_tool_row(
    node: &crate::defra_node::EmbeddedNode,
    session_id: &str,
    call_id: &str,
) -> serde_json::Value {
    let session = crate::graphql::escape_graphql_string(session_id);
    let call = crate::graphql::escape_graphql_string(call_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_call_id: {{ _eq: "{call}" }} }}, limit: 2) {{ lifecycle_state cancel_cause }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load accepted tool: {:?}",
        response.errors
    );
    let rows: Vec<serde_json::Value> =
        crate::graphql::rows(&response, "AgentToolCall").expect("decode accepted tool rows");
    assert_eq!(
        rows.len(),
        1,
        "one exact accepted tool {call_id} in session {session_id}"
    );
    rows.into_iter().next().unwrap()
}

async fn queued_request_row(
    node: &crate::defra_node::EmbeddedNode,
    request_id: &str,
) -> serde_json::Value {
    let request = crate::graphql::escape_graphql_string(request_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request}" }} }}, limit: 2) {{ lifecycle_state failure_reason }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load queued request: {:?}",
        response.errors
    );
    let rows: Vec<serde_json::Value> =
        crate::graphql::rows(&response, "AgentRequest").expect("decode queued request rows");
    assert_eq!(rows.len(), 1, "one exact queued request");
    rows.into_iter().next().unwrap()
}

#[tokio::test]
async fn accepted_wait_subagent_backgrounding_returns_receipt_on_original_bridge() {
    let (db, hook, session_id) = accepted_subagent_hook("r4-accepted-wait-backgrounded").await;
    const CHILD: &str = "r4-accepted-control-child";
    let _source = spawn_subagent_source(db.node.clone(), db.node_identity.did(), "general", CHILD);
    let spawn_args = json!({
        "name": CHILD,
        "prompt": "background child for wait backgrounding",
        "await_mode": "background"
    })
    .to_string();
    let spawn_receipt = skip_json(
        super::r4c_private_support::accepted_call(
            &hook,
            "spawn_subagent",
            None,
            "accepted-wait-spawn",
            &spawn_args,
        )
        .await,
    );
    assert_eq!(spawn_receipt["ok"], true);
    assert_eq!(spawn_receipt["await_mode"], "background");
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("reserved child id")
        .to_string();
    wait_for_child_materialized(db.node.as_ref(), &child_request_id).await;

    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    accept_hook_tool_call(
        &hook,
        "accepted-wait-control",
        "wait_subagent",
        &wait_args,
        None,
    )
    .await;
    let wait_hook = hook.clone();
    let wait_args_for_task = wait_args.clone();
    let wait = tokio::spawn(async move {
        wait_hook
            .on_tool_call(
                "wait_subagent",
                None,
                "accepted-wait-control",
                &wait_args_for_task,
            )
            .await
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let bridge = ToolCallLifecycle::load(db.node.clone(), &session_id, "accepted-wait-spawn")
            .await
            .expect("load accepted bridge")
            .expect("accepted bridge row");
        if bridge.await_mode() == crate::tool_call_lifecycle::AwaitMode::Foreground {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "wait did not foreground bridge"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let mut bridge = ToolCallLifecycle::load(db.node.clone(), &session_id, "accepted-wait-spawn")
        .await
        .expect("load foreground bridge")
        .expect("foreground bridge row");
    bridge
        .background()
        .await
        .expect("background original bridge");

    let receipt = skip_json(
        tokio::time::timeout(std::time::Duration::from_secs(5), wait)
            .await
            .expect("wait control completed")
            .expect("wait task did not panic"),
    );
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["await_mode"], "background");
    assert_eq!(receipt["status"], "running");
    assert_eq!(receipt["backgrounded"], true);
    assert_eq!(receipt["child_request_id"], child_request_id);
    let bridge = ToolCallLifecycle::load(db.node.clone(), &session_id, "accepted-wait-spawn")
        .await
        .expect("reload original bridge")
        .expect("original bridge row");
    assert_eq!(
        bridge.await_mode(),
        crate::tool_call_lifecycle::AwaitMode::Background
    );
    let durable = crate::tool_call_lifecycle::query::load_tool_call_result(
        &crate::config_client::ConfigAccess::Local(db.node.clone()),
        bridge.doc_id().expect("original accepted bridge document"),
        db.node_identity.did(),
        &session_id,
        Some(db.node_identity.did()),
    )
    .await
    .expect("original immutable background invocation receipt");
    let original_receipt: serde_json::Value = serde_json::from_str(
        &crate::tool_call_lifecycle::query::render_tool_result(&durable)
            .expect("render original immutable receipt"),
    )
    .expect("original receipt JSON");
    assert_eq!(original_receipt, spawn_receipt);
    let escaped_session = crate::graphql::escape_graphql_string(&session_id);
    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped_session}" }}, tool_name: {{ _eq: "spawn_subagent" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "count spawn bridges: {:?}",
        response.errors
    );
    let rows = response.data.as_ref().expect("count response")["AgentToolCall"]
        .as_array()
        .expect("tool rows");
    assert_eq!(
        rows.len(),
        1,
        "wait_subagent must reuse the original bridge"
    );
}

#[tokio::test]
async fn accepted_resumed_wait_cascades_current_parent_interrupt_to_child() {
    assert_resumed_wait_cascades_current_request_interrupt(false).await;
}

#[tokio::test]
async fn accepted_later_turn_wait_uses_current_caller_for_interrupt_cascade() {
    assert_resumed_wait_cascades_current_request_interrupt(true).await;
}

async fn assert_resumed_wait_cascades_current_request_interrupt(later_turn: bool) {
    let label = if later_turn {
        "r4-accepted-later-wait-interrupt"
    } else {
        "r4-accepted-resumed-wait-interrupt"
    };
    const CHILD: &str = "r4-accepted-control-child";
    let (db, hook, session_id) = accepted_subagent_hook(label).await;
    let _source = spawn_subagent_source(db.node.clone(), db.node_identity.did(), "general", CHILD);
    let spawn_args = json!({
        "name": CHILD,
        "prompt": "background child for resumed wait cancellation",
        "await_mode": "background"
    })
    .to_string();
    let spawn_receipt = skip_json(
        super::r4c_private_support::accepted_call(
            &hook,
            "spawn_subagent",
            None,
            "accepted-resumed-wait-spawn",
            &spawn_args,
        )
        .await,
    );
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("reserved child")
        .to_owned();
    assert_eq!(spawn_receipt["await_mode"], "background");
    wait_for_child_materialized(db.node.as_ref(), &child_request_id).await;
    let parent_request_id = format!("{label}-request");
    let waiting_request_id = if later_turn {
        let mut fixtures = hook_execution_fixtures().lock().await;
        let fixture = fixtures
            .get_mut(&hook_execution_fixture_key(&hook, &parent_request_id))
            .expect("original accepted request retains owned lifecycle");
        let turn = fixture.turn;
        fixture
            .writer
            .start_provider_attempt(
                &fixture.lifecycle.request().doc_id,
                turn,
                0,
                format!("inference.{}", turn + 1).parse().unwrap(),
            )
            .await;
        let message = gents_protocol::message::Message::Assistant {
            id: Some(format!("{label}-original-complete")),
            content: vec![gents_protocol::message::AssistantContent::Text(
                gents_protocol::message::Text {
                    text: "original request completed before later wait".to_owned(),
                },
            )],
        };
        let published = fixture
            .writer
            .publish_native_turn(&fixture.lifecycle, turn, 0, &message)
            .await
            .expect("publish original request completion");
        let terminal = fixture
            .lifecycle
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: published.message_doc_id,
                },
                None,
            )
            .await
            .expect("terminalize original accepted request");
        assert_eq!(terminal, crate::lifecycle::TerminalizeResult::Won);
        drop(fixtures);
        let later_id = format!("{label}-later-request");
        super::r4c_private_support::bind_accepted_request(
            &db,
            &hook,
            "general",
            &later_id,
            &session_id,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await;
        later_id
    } else {
        parent_request_id.clone()
    };
    let resumed = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        "general",
        db.node_identity.did(),
        Some(db.node_identity.did()),
        FailurePolicy::default(),
    )
    .await
    .expect("resume parent hook");
    resumed
        .set_active_request_lineage(
            Some(waiting_request_id.clone()),
            Some(db.node_identity.did().to_owned()),
        )
        .await
        .expect("bind current parent request");
    resumed
        .set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    accept_hook_tool_call(
        &resumed,
        "accepted-resumed-wait-control",
        "wait_subagent",
        &wait_args,
        None,
    )
    .await;
    let wait_hook = resumed.clone();
    let wait_args_for_task = wait_args.clone();
    let wait = tokio::spawn(async move {
        wait_hook
            .on_tool_call(
                "wait_subagent",
                None,
                "accepted-resumed-wait-control",
                &wait_args_for_task,
            )
            .await
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let bridge =
            ToolCallLifecycle::load(db.node.clone(), &session_id, "accepted-resumed-wait-spawn")
                .await
                .expect("load accepted bridge")
                .expect("accepted bridge row");
        if bridge.await_mode() == crate::tool_call_lifecycle::AwaitMode::Foreground {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "resumed wait did not foreground bridge"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    crate::interrupt_request(db.node.as_ref(), &waiting_request_id)
        .await
        .expect("interrupt current parent");
    let result = skip_json(
        tokio::time::timeout(std::time::Duration::from_secs(5), wait)
            .await
            .expect("resumed wait unblocked")
            .expect("wait task did not panic"),
    );
    assert_eq!(result["ok"], false);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "interrupted");
    let bridge =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "accepted-resumed-wait-spawn")
            .await
            .expect("reload bridge")
            .expect("bridge row");
    assert_eq!(
        bridge.state(),
        crate::tool_call_lifecycle::ToolCallState::Cancelled
    );
    assert!(
        crate::fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
            .await
            .expect("child interrupt observation")
            .is_some(),
        "current caller interruption must cascade to child"
    );
    if later_turn {
        assert!(
            crate::fetch_interrupt_requested_at(db.node.as_ref(), &parent_request_id)
                .await
                .expect("original parent interrupt observation")
                .is_none(),
            "the spawning parent was not the interrupted caller"
        );
    }
}

#[tokio::test]
async fn accepted_foreground_spawn_cancellation_cascades_and_unblocks_wait() {
    const LABEL: &str = "r4-accepted-foreground-cancel";
    const CHILD: &str = "r4-accepted-control-child";
    const CALL: &str = "accepted-foreground-cancel";
    let (db, hook, session_id) = accepted_subagent_hook(LABEL).await;
    let _source = spawn_subagent_source(db.node.clone(), db.node_identity.did(), "general", CHILD);
    let args = json!({
        "name": CHILD,
        "prompt": "foreground child that will be cancelled",
        "await_mode": "foreground"
    })
    .to_string();
    let call_hook = hook.clone();
    let call_args = args.clone();
    let running_call = tokio::spawn(async move {
        super::r4c_private_support::accepted_call(
            &call_hook,
            "spawn_subagent",
            None,
            CALL,
            &call_args,
        )
        .await
    });
    let child_request_id = wait_for_bridge_child_id(db.node.as_ref(), &session_id, CALL).await;
    wait_for_child_materialized(db.node.as_ref(), &child_request_id).await;
    let mut bridge = ToolCallLifecycle::load(db.node.clone(), &session_id, CALL)
        .await
        .expect("load foreground bridge")
        .expect("accepted foreground bridge");
    bridge
        .cancel_during_run(crate::tool_call_lifecycle::CancelCause::Interrupted)
        .await
        .expect("cancel accepted bridge");
    let intent = bridge
        .bridge_cancel_cascade()
        .await
        .expect("resolve cascade")
        .expect("foreground child cascade intent");
    assert_eq!(intent.child_request_id, child_request_id);
    crate::interrupt_request(db.node.as_ref(), &intent.child_request_id)
        .await
        .expect("interrupt materialized child");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), running_call)
        .await
        .expect("foreground wait unblocked")
        .expect("foreground task did not panic");
    let ToolCallHookAction::Skip { reason } = result else {
        panic!("cancelled accepted call must skip, got {result:?}");
    };
    assert_eq!(reason, "tool call cancelled");
    let bridge = ToolCallLifecycle::load(db.node.clone(), &session_id, CALL)
        .await
        .expect("reload cancelled bridge")
        .expect("cancelled bridge row");
    assert_eq!(
        bridge.state(),
        crate::tool_call_lifecycle::ToolCallState::Cancelled
    );
    let bridge_doc_id = crate::graphql::escape_graphql_string(
        bridge.doc_id().expect("cancelled accepted bridge document"),
    );
    let persisted = db
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{bridge_doc_id}" }} }}, limit: 1) {{ cancel_cause }} }}"#
        ))
        .await;
    assert!(
        !persisted.has_errors(),
        "load cancellation cause: {:?}",
        persisted.errors
    );
    let row: serde_json::Value = crate::graphql::first_row(&persisted, "AgentToolCall")
        .expect("decode cancellation cause")
        .expect("cancelled bridge remains persisted");
    assert_eq!(row["cancel_cause"], "interrupted");
    assert!(
        crate::fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
            .await
            .expect("child interrupt observation")
            .is_some(),
        "foreground cancellation must latch child interruption"
    );
}

#[tokio::test]
async fn accepted_cancel_subagent_cascades_descendants_and_drains_only_owned_queue() {
    use gents_protocol::request_input::{QueuePolicy, QueueSource};

    const LABEL: &str = "r4-accepted-cancel-cascade";
    const CHILD: &str = "r4-accepted-control-child";
    const SPAWN: &str = "accepted-cancel-root-spawn";
    const DESCENDANT: &str = "accepted-cancel-descendant";
    const CANCEL: &str = "accepted-cancel-control";
    const GRANDCHILD: &str = "r4-accepted-cancel-grandchild";
    let (db, hook, parent_session) = accepted_subagent_hook_with_child_spawning(LABEL, true).await;
    let source = spawn_subagent_source(db.node.clone(), db.node_identity.did(), "general", CHILD);
    let spawn_args = json!({
        "name": CHILD,
        "prompt": "background child with a descendant",
        "await_mode": "background"
    })
    .to_string();
    let root_receipt = skip_json(
        super::r4c_private_support::accepted_call(
            &hook,
            "spawn_subagent",
            Some("provider-cancel-root".into()),
            SPAWN,
            &spawn_args,
        )
        .await,
    );
    assert_eq!(root_receipt["ok"], true);
    let child_request_id = root_receipt["child_request_id"]
        .as_str()
        .expect("accepted child request id")
        .to_owned();
    wait_for_child_materialized(db.node.as_ref(), &child_request_id).await;
    drop(source);
    let child_row =
        crate::support::load_request_row_by_logical_id(db.node.as_ref(), &child_request_id).await;
    let child_session = child_row
        .session_id
        .clone()
        .expect("materialized child session");
    let child_request_doc = child_row.doc_id.clone().expect("physical child request");
    let mut child_owner = crate::lifecycle::RequestLifecycle::new_with_agent_did(
        db.node.clone(),
        CHILD,
        db.node_identity.did(),
        child_row.try_into().expect("canonical child request"),
        60,
    );
    assert_eq!(
        child_owner.claim().await.expect("claim child request"),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    let descendant =
        crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            db.node.clone(),
            &mut child_owner,
            db.node_identity.did(),
            0,
            "spawn_subagent",
            DESCENDANT,
            json!({"name": CHILD, "prompt": "grandchild work", "await_mode": "background"}),
            Some(crate::streaming::SpawnAdmissionPlan {
                tool_call_id: DESCENDANT.into(),
                child_request_id: GRANDCHILD.into(),
                spawn_target_did: db.node_identity.did().into(),
                spawn_behavior_id: CHILD.into(),
                delegated_workspace: None,
                await_mode: crate::tool_call_lifecycle::AwaitMode::Background,
            }),
            crate::tool_call_lifecycle::AwaitMode::Background,
            crate::tool_call_lifecycle::CancelPolicy::Cascade,
            true,
        )
        .await
        .expect("publish accepted descendant spawn");
    let _grandchild_session = crate::tool_call_lifecycle::create_subagent_request_with_request_id(
        db.node.as_ref(),
        GRANDCHILD.into(),
        child_request_id.clone(),
        child_request_doc,
        DESCENDANT.into(),
        descendant
            .doc_id()
            .expect("accepted descendant document")
            .into(),
        1,
        db.node_identity.did().into(),
        CHILD.into(),
        "grandchild work".into(),
        Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
    )
    .await
    .expect("materialize accepted grandchild lineage");

    let automated = "r4-accepted-cancel-auto-queue";
    let steering = "r4-accepted-cancel-steering-queue";
    let user = "r4-accepted-cancel-user-queue";
    seed_signed_child_queue_request(
        &db,
        automated,
        &child_session,
        QueueSource::BackgroundCompletion,
        QueuePolicy::Coalesce,
        Some("background_completion:r4-accepted-cancel"),
        &child_request_id,
    )
    .await;
    seed_signed_child_queue_request(
        &db,
        steering,
        &child_session,
        QueueSource::Steering,
        QueuePolicy::Append,
        None,
        &child_request_id,
    )
    .await;
    seed_signed_child_queue_request(
        &db,
        user,
        &child_session,
        QueueSource::User,
        QueuePolicy::Append,
        None,
        &child_request_id,
    )
    .await;

    let collision = super::r4c_private_support::accepted_call(
        &hook,
        "bash",
        None,
        DESCENDANT,
        r#"{"cmd":"still running"}"#,
    )
    .await;
    assert!(matches!(collision, ToolCallHookAction::Continue));
    let cancel_args = json!({
        "child_request_id": child_request_id,
        "reason": "parent no longer needs this work"
    })
    .to_string();
    let result = skip_json(
        super::r4c_private_support::accepted_call(
            &hook,
            "cancel_subagent",
            Some("provider-cancel-control".into()),
            CANCEL,
            &cancel_args,
        )
        .await,
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["status"], "cancelled");
    assert_eq!(result["child_request_id"], child_request_id);
    assert_eq!(result["child_session_id"], child_session);
    assert_eq!(result["active_interrupted"], true);
    assert_eq!(result["descendants_cancelled"], 1);
    assert_eq!(result["queued_drained"], 2);

    let root = accepted_tool_row(db.node.as_ref(), &parent_session, "provider-cancel-root").await;
    assert_eq!(root["lifecycle_state"], "cancelled");
    assert_eq!(root["cancel_cause"], "userCancelled");
    let descendant = accepted_tool_row(db.node.as_ref(), &child_session, DESCENDANT).await;
    assert_eq!(descendant["lifecycle_state"], "cancelled");
    assert_eq!(descendant["cancel_cause"], "userCancelled");
    let collision = accepted_tool_row(db.node.as_ref(), &parent_session, DESCENDANT).await;
    assert_eq!(collision["lifecycle_state"], "running");
    assert!(
        crate::fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        crate::fetch_interrupt_requested_at(db.node.as_ref(), GRANDCHILD)
            .await
            .unwrap()
            .is_some()
    );
    let automated_row = queued_request_row(db.node.as_ref(), automated).await;
    assert_eq!(automated_row["lifecycle_state"], "interrupted");
    assert!(automated_row["failure_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("parent no longer needs this work")));
    assert_eq!(
        queued_request_row(db.node.as_ref(), steering).await["lifecycle_state"],
        "interrupted"
    );
    assert_eq!(
        queued_request_row(db.node.as_ref(), user).await["lifecycle_state"],
        "pending"
    );
    let cancel_control =
        accepted_tool_row(db.node.as_ref(), &parent_session, "provider-cancel-control").await;
    assert_eq!(cancel_control["lifecycle_state"], "completed");
}
