use gents::{
    session::canonical_rows::{
        decode_output_segment_row, decode_transcript_message_row, output_segment_create_variables,
        transcript_message_create_variables, OutputSegmentRow, AGENT_MESSAGE_FIELDS,
        AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    },
    tool_call_lifecycle::ToolCallLifecycle,
    RequestLifecycle,
};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use std::sync::Arc;

use crate::support::fixtures::{
    configure_behavior_tools, configure_subagent_behavior, subagent_target,
};
use crate::support::interrupt::create_runtime_request_with_valid_until;
use crate::support::snapshots::{
    fetch_message_snapshots_for_session, fetch_tool_call_snapshots_for_session,
};
use crate::support::{
    accepted_turn::{
        boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnRuntime, AcceptedTurnSpec,
    },
    create_agent_session, create_request, create_request_for_agent_with_signed_fields, first_row,
    streaming_backend::{StreamChunk, StreamPlan, StreamResponse},
    test_db, AGENT_DID, AGENT_NAME, BACKEND_ID,
};

type StatusRow = AgentRequestRow;

#[derive(Debug, Clone, Deserialize)]
struct NotificationDeliveryRow {
    completion_notification_delivered_at: Option<String>,
}

async fn boot_running_recovery_bash(
    db: &crate::support::TestDb,
    fixture: &str,
    call_id: &str,
) -> (AcceptedTurnRuntime, String, String, String) {
    let session_id = format!("{fixture}-session");
    let request_id = format!("{fixture}-request");
    let prompt = format!("{fixture}-prompt");
    let prepared = prepare_accepted_turn(
        db,
        AcceptedTurnSpec {
            backend_id: "lifecycle-recovery-backend",
            model: "lifecycle-recovery-model",
            parent_behavior_id: AGENT_NAME,
            configured_behavior_ids: &[AGENT_NAME],
            request_id: &request_id,
            session_id: &session_id,
            prompt: &prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                call_id,
                "bash",
                r#"{"command":"sleep","args":["60"]}"#,
            )],
            child_plans: Vec::new(),
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    configure_behavior_tools(
        db.node.as_ref(),
        db.node_identity.did(),
        AGENT_NAME,
        None,
        gents::document_config::Tools {
            tools_id: format!("{AGENT_NAME}:recovery-tools"),
            agent_did: db.node_identity.did().to_string(),
            host: Some(gents::document_config::HostTools {
                bash: Some(gents::document_config::BashTools {
                    mode: gents::BashMode::ReadOnly,
                    read_only_commands: Some(vec!["sleep".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let identity: Arc<dyn gents::AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build lifecycle recovery runtime");
    let runtime = boot_prepared_accepted_turn(db, prepared, agent).await;
    let tool_call_doc_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(tool_call_doc_id) =
                accepted_tool_call_doc_id(db.node.as_ref(), &session_id, call_id).await
            {
                let rows = fetch_tool_call_snapshots_for_session(&db.node, &session_id).await;
                if rows.iter().any(|row| {
                    row.doc_id == tool_call_doc_id
                        && row.lifecycle_state.as_deref() == Some("running")
                }) {
                    break tool_call_doc_id;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("canonical bash call did not reach running");
    (runtime, request_id, session_id, tool_call_doc_id)
}

async fn boot_two_running_recovery_bashes(
    db: &crate::support::TestDb,
) -> ([AcceptedTurnRuntime; 2], [(String, String, String); 2]) {
    let first = boot_running_recovery_bash(db, "tool-cancel", "tool-cancel-call").await;
    let second = boot_running_recovery_bash(db, "tool-other", "tool-other-call").await;
    let (first_runtime, first_request, first_session, first_doc) = first;
    let (second_runtime, second_request, second_session, second_doc) = second;
    (
        [first_runtime, second_runtime],
        [
            (first_request, first_session, first_doc),
            (second_request, second_session, second_doc),
        ],
    )
}

async fn boot_running_recovery_subagent(
    db: &crate::support::TestDb,
    fixture: &str,
    await_mode: &str,
    cancel_policy: &str,
) -> (AcceptedTurnRuntime, String, String, String, String) {
    let agent_did = db.node_identity.did().to_string();
    let child_behavior = format!("{fixture}-child-behavior");
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        &child_behavior,
        &format!("{fixture}-child-tools"),
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        AGENT_NAME,
        &format!("{fixture}-parent-tools"),
        vec![subagent_target(
            &agent_did,
            &child_behavior,
            &agent_did,
            &child_behavior,
        )],
        true,
        true,
        None,
    )
    .await;
    let request_id = format!("{fixture}-parent");
    let session_id = format!("{fixture}-parent-session");
    let prompt = format!("{fixture}-parent-prompt");
    let child_prompt = format!("{fixture}-child-prompt");
    let provider_call_id = format!("{fixture}-spawn");
    let args = serde_json::json!({
        "name": child_behavior,
        "prompt": child_prompt,
        "await_mode": await_mode,
    })
    .to_string();
    let prepared = prepare_accepted_turn(
        db,
        AcceptedTurnSpec {
            backend_id: "lifecycle-recovery-subagent-backend",
            model: "lifecycle-recovery-subagent-model",
            parent_behavior_id: AGENT_NAME,
            configured_behavior_ids: &[AGENT_NAME, &child_behavior],
            request_id: &request_id,
            session_id: &session_id,
            prompt: &prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                &provider_call_id,
                "spawn_subagent",
                args,
            )],
            child_plans: vec![StreamPlan::new(
                child_prompt.clone(),
                vec![StreamResponse::Stream(
                    crate::support::streaming_backend::StreamScript::paused(
                        child_prompt,
                        ["child held for recovery"],
                    ),
                )],
            )],
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    prepared.backend.enable_dynamic_followups(&prompt);
    let identity: Arc<dyn gents::AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build recovery subagent runtime");
    let runtime = boot_prepared_accepted_turn(db, prepared, agent).await;
    let (parent_doc_id, bridge_doc_id, child_request_id, child_request_doc_id) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        async {
            loop {
                let parent = db.node.execute(&format!(
                    r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
                    gents::graphql::escape_graphql_string(&request_id),
                )).await;
                let parent_doc_id = parent.data.as_ref().and_then(|data| data["AgentRequest"].as_array()).and_then(|rows| rows.first()).and_then(|row| row["_docID"].as_str());
                let rows = fetch_tool_call_snapshots_for_session(&db.node, &session_id).await;
                if let (Some(parent_doc_id), Some(bridge)) = (parent_doc_id, rows.iter().find(|row| row.tool_name == "spawn_subagent" && row.lifecycle_state.as_deref() == Some("running") && row.child_request_id.is_some())) {
                    let children = db.node.execute(&format!(
                        r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{}" }} }}) {{ _docID request_id }} }}"#,
                        gents::graphql::escape_graphql_string(&bridge.doc_id),
                    )).await;
                    let child_rows = children.data.as_ref().and_then(|data| data["AgentRequest"].as_array());
                    if let Some(child_doc_id) = child_rows.and_then(|rows| rows.first()).and_then(|row| row["_docID"].as_str()) {
                        break (parent_doc_id.to_string(), bridge.doc_id.clone(), bridge.child_request_id.clone().unwrap(), child_doc_id.to_string());
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        },
    ).await.expect("canonical subagent bridge did not reach running");
    // Spawn defaults to cascade; detachment is a separate lifecycle action,
    // not a spawn argument. Exercise that owner before the crash boundary.
    if cancel_policy == "detach" {
        let mut bridge = ToolCallLifecycle::load_by_doc_id(
            db.node.clone(),
            &bridge_doc_id,
            &agent_did,
            &session_id,
            Some(&agent_did),
        )
        .await
        .expect("load physical bridge")
        .expect("accepted bridge exists");
        bridge.detach().await.expect("detach accepted bridge");
    } else {
        assert_eq!(cancel_policy, "cascade");
    }
    (
        runtime,
        parent_doc_id,
        bridge_doc_id,
        child_request_id,
        child_request_doc_id,
    )
}

async fn fetch_interrupt_requested_at_by_doc(
    node: &gents::defra_node::EmbeddedNode,
    request_doc_id: &str,
) -> Option<String> {
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ request_id interrupt_requested_at }} }}"#,
            gents::graphql::escape_graphql_string(request_doc_id),
        ))
        .await;
    first_row::<AgentRequestRow>(&response, "AgentRequest").interrupt_requested_at
}

async fn assert_unique_request_identity(
    node: &gents::defra_node::EmbeddedNode,
    request_id: &str,
    expected_doc_id: &str,
    expected_parent_tool_doc_id: &str,
) {
    let bridge_response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ agent_did request_id request_doc_id tool_call_id }} }}"#,
            gents::graphql::escape_graphql_string(expected_parent_tool_doc_id),
        ))
        .await;
    assert!(
        !bridge_response.has_errors(),
        "load accepted bridge identity: {:?}",
        bridge_response.errors
    );
    let bridge = bridge_response
        .data
        .as_ref()
        .and_then(|data| data["AgentToolCall"].as_array())
        .and_then(|rows| rows.first())
        .expect("accepted bridge identity row");
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID request_id agent_did caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id }} }}"#,
            gents::graphql::escape_graphql_string(request_id),
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load child identity: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data["AgentRequest"].as_array())
        .expect("child identity rows");
    assert_eq!(
        rows.len(),
        1,
        "one accepted spawn must create exactly one physical child request: {rows:?}"
    );
    assert_eq!(rows[0]["_docID"].as_str(), Some(expected_doc_id));
    assert_eq!(rows[0]["agent_did"], bridge["agent_did"]);
    assert_eq!(rows[0]["caused_by_parent_request_id"], bridge["request_id"]);
    assert_eq!(
        rows[0]["caused_by_parent_request_doc_id"],
        bridge["request_doc_id"]
    );
    assert_eq!(
        rows[0]["caused_by_parent_tool_call_id"],
        bridge["tool_call_id"]
    );
    assert_eq!(
        rows[0]["caused_by_parent_tool_call_doc_id"].as_str(),
        Some(expected_parent_tool_doc_id)
    );
}

/// Resolve a provider call id through the accepted canonical header. The
/// lifecycle row's `tool_call_id` is an internal invocation id and is not the
/// provider id carried by the transcript block.
async fn accepted_tool_call_doc_id(
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
    provider_tool_call_id: &str,
) -> Option<String> {
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}, order: {{ sequence: ASC }}) {{ _docID agent_did requester_did }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load lifecycle recovery accepted headers: {:?}",
        response.errors
    );
    for row in response
        .data
        .as_ref()
        .and_then(|data| data["AgentMessage"].as_array())
        .expect("AgentMessage header rows")
    {
        let header_doc_id = row["_docID"].as_str().expect("header _docID");
        let agent_did = row["agent_did"].as_str().expect("header agent_did");
        let requester_did = row["requester_did"].as_str();
        let (header, _) = gents::session::load_canonical_message_from_node(
            node,
            header_doc_id,
            agent_did,
            requester_did,
        )
        .await
        .expect("reconstruct lifecycle recovery accepted header");
        for block in header.blocks {
            if let gents_protocol::output::MessageBlock::ToolCall {
                tool_call_doc_id,
                id,
                call_id,
                ..
            } = block
            {
                if id == provider_tool_call_id || call_id.as_deref() == Some(provider_tool_call_id)
                {
                    return Some(tool_call_doc_id);
                }
            }
        }
    }
    None
}

fn background_wake_input(session_id: &str) -> gents_protocol::request_input::RequestInput {
    use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
    RequestInput {
        queue: Some(RequestQueue {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(format!("background_completion:{session_id}")),
            queued_after_request_id: Some("foreground-parent".into()),
            interrupted_request_id: None,
            background_completion_wake_version: Some(1),
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn failed_background_wake_redrive_is_bounded_and_idempotent() {
    let _trace = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::WARN)
            .finish(),
    );
    let db = test_db("lifecycle-background-wake-redrive").await;
    let agent_did = db.node_identity.did().to_string();
    let input = background_wake_input("wake-redrive-session");
    let input_literal =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(&input).unwrap())
            .unwrap();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "failed-wake",
                purpose: "normal",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                behavior_id: "{AGENT_NAME}",
                session_id: "wake-redrive-session",
                retry_parent_request: "",
                retry_root_request: "failed-wake",
                superseded_by_request: "",
                content: "continue after background completion",
                input: {input_literal},
                lifecycle_state: "failed",
                backend_id: "{BACKEND_ID}",
                max_total_tokens: 4096,
                execution_origin: "scheduled",
                failure_reason: "backend admission failed",
                terminalized_at: "2026-08-12T00:00:00Z",
                terminal_redrive_attempts: 0,
                created_at: "2026-08-12T00:00:00Z",
                deadline: "2026-08-12T00:00:01Z",
                retry_count: 1,
                max_retries: 3,
                valid_until: "2026-08-12T00:00:01Z",
                subagent_depth: 0
            }}) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create failed wake: {:?}",
        response.errors
    );
    let mut session = crate::support::session_document(
        "wake-redrive-session",
        AGENT_NAME,
        "2026-08-12T00:00:00Z",
    );
    session.agent_did = agent_did.clone();
    session.requester_did = Some(agent_did.clone());
    crate::support::create_session_document(&db.node, &session).await;
    crate::support::seed_session_observation_from_request(
        &db.node,
        "wake-redrive-session",
        "failed-wake",
        "continue after background completion",
    )
    .await;

    // Background wake recovery deliberately checks the head across requesters.
    // A newer interactive request blocks the old wake even in another scope.
    let foreign = serde_json::json!({
        "request_id":"foreign-interactive", "agent_did":agent_did,
        "purpose":"normal",
        "requester_did":"did:test:foreign-requester", "behavior_id":AGENT_NAME,
        "session_id":"wake-redrive-session", "content":"foreign interactive",
        "lifecycle_state":"pending", "execution_origin":"interactive",
        "created_at":"2099-01-01T00:00:00Z", "retry_count":0, "max_retries":3,
        "subagent_depth":0
    });
    let foreign_input = gents_protocol::graphql::graphql_input_literal(&foreign).unwrap();
    let response = db
        .node
        .execute(&format!(
            "mutation {{ create_AgentRequest(input: {foreign_input}) {{ _docID }} }}"
        ))
        .await;
    assert!(
        !response.has_errors(),
        "foreign request fixture: {:?}",
        response.errors
    );
    let blocked = RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did)
        .await
        .expect("newer interactive head gate");
    assert_eq!(blocked.ineligible, 1);
    assert_eq!(blocked.redriven, 0);
    assert_eq!(blocked.failed, 0);
    // Remove only the competing fixture to exercise successful recovery below.
    let response = db.node.execute(&format!(
        r#"mutation {{ delete_AgentRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, request_id: {{ _eq: "foreign-interactive" }} }}) {{ _docID }} }}"#
    )).await;
    assert!(
        !response.has_errors(),
        "remove competing fixture: {:?}",
        response.errors
    );

    let (first, concurrent) = tokio::join!(
        RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did),
        RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did),
    );
    let first = first.expect("first concurrent redrive");
    let concurrent = concurrent.expect("second concurrent redrive");
    assert_eq!(first.scanned, 1);
    assert_eq!(concurrent.scanned, 1);
    assert_eq!(
        first.redriven + concurrent.redriven,
        1,
        "redrive reports: {first:?}, {concurrent:?}"
    );
    assert_eq!(first.already_redriven + concurrent.already_redriven, 1);
    assert_eq!(first.failed + concurrent.failed, 0);

    let rows = background_wake_retry_rows(&db.node, "wake-redrive-session").await;
    assert_eq!(rows.len(), 2);
    let successor = rows
        .iter()
        .find(|row| {
            row.request_id != "failed-wake"
                && row.requester_did.as_deref() == Some(agent_did.as_str())
        })
        .expect("retry successor");
    assert_eq!(
        successor.lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    assert_eq!(successor.execution_origin.as_deref(), Some("scheduled"));
    let source = rows
        .iter()
        .find(|row| row.request_id == "failed-wake")
        .unwrap();
    assert!(source.doc_id.is_some());
    assert_eq!(successor.retry_parent_request_doc_id, source.doc_id);
    assert_eq!(
        successor.admission_kind.as_deref(),
        Some("runtime-internal")
    );
    assert_eq!(
        successor.admission_signer_did.as_deref(),
        Some(agent_did.as_str())
    );
    assert!(successor
        .admission_signature
        .as_deref()
        .is_some_and(|signature| !signature.is_empty()));
    // Claim owns inference selection and budget pinning for the new request.
    assert_eq!(successor.backend_id, None);
    assert_eq!(successor.max_total_tokens, None);
    assert_eq!(
        successor.retry_parent_request.as_deref(),
        Some("failed-wake")
    );
    assert_eq!(successor.retry_root_request.as_deref(), Some("failed-wake"));
    assert_eq!(
        successor.content.as_deref(),
        Some(gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT)
    );
    assert_eq!(successor.retry_count, Some(2));
    assert_eq!(successor.max_retries, Some(3));
    assert_eq!(successor.input.as_ref(), Some(&input));
    assert_eq!(successor.deadline, None);
    assert_eq!(successor.valid_until, None);

    let second = RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did)
        .await
        .expect("repeat redrive");
    assert_eq!(second.redriven, 0);
    assert_eq!(second.already_redriven, 1);
    assert_eq!(
        background_wake_retry_rows(&db.node, "wake-redrive-session")
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn failed_background_wake_without_pending_sibling_obeys_persisted_backoff() {
    let db = test_db("lifecycle-background-wake-isolated-backoff").await;
    let agent_did = db.node_identity.did().to_string();
    let session_id = "wake-isolated-backoff-session";
    let request_id = "failed-isolated-backoff-wake";
    let input = background_wake_input(session_id);
    let terminalized_at = chrono::Utc::now();
    let created_at = (terminalized_at - chrono::Duration::seconds(1)).to_rfc3339();
    let terminalized_at = terminalized_at.to_rfc3339();
    let request = serde_json::json!({
        "request_id": request_id,
        "purpose": "normal",
        "agent_did": agent_did,
        "requester_did": agent_did,
        "behavior_id": AGENT_NAME,
        "session_id": session_id,
        "retry_root_request": request_id,
        "content": "continue after background completion",
        "input": input,
        "lifecycle_state": "failed",
        "execution_origin": "scheduled",
        "failure_reason": "provider failed",
        "created_at": created_at,
        "terminalized_at": terminalized_at,
        "retry_count": 4,
        "max_retries": 6,
        "subagent_depth": 0
    });
    let request_input = gents_protocol::graphql::graphql_input_literal(&request).unwrap();
    let created = db
        .node
        .execute(&format!(
            "mutation {{ create_AgentRequest(input: {request_input}) {{ _docID }} }}"
        ))
        .await;
    assert!(
        !created.has_errors(),
        "seed failed wake: {:?}",
        created.errors
    );
    let mut session = crate::support::session_document(session_id, AGENT_NAME, &created_at);
    session.agent_did = agent_did.clone();
    session.requester_did = Some(agent_did.clone());
    crate::support::create_session_document(&db.node, &session).await;
    crate::support::seed_session_observation_from_request(
        &db.node,
        session_id,
        request_id,
        "continue after background completion",
    )
    .await;

    let before = background_wake_retry_rows(&db.node, session_id).await;
    assert_eq!(before.len(), 1, "no same-key pending wake may mask backoff");
    assert_eq!(
        before[0].lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    let next_retry_at = gents::lifecycle::background_wake_next_retry_at(
        before[0].terminalized_at.as_deref(),
        before[0].retry_count.expect("persisted retry count"),
    )
    .expect("persisted terminal timestamp determines backoff");
    assert!(next_retry_at > chrono::Utc::now());

    let report = RequestLifecycle::redrive_failed_background_wakeups(&db.node, &agent_did)
        .await
        .expect("isolated backoff sweep");
    assert_eq!(report.scanned, 1);
    assert_eq!(report.deferred, 1);
    assert_eq!(report.redriven, 0);
    assert_eq!(report.coalesced, 0);
    assert_eq!(report.already_redriven, 0);
    assert_eq!(report.ineligible, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(
        background_wake_retry_rows(&db.node, session_id).await,
        before
    );
}

#[tokio::test]
async fn failed_background_wake_waits_for_persisted_backoff() {
    let db = test_db("lifecycle-background-wake-backoff").await;
    let session_id = "wake-backoff-session";
    let input = background_wake_input(session_id);
    // The queue wake is runtime-issued, not a local-self user request. Give
    // its authenticated local-control receipt one exact, signed parent row.
    // The source is expired so it cannot consume the scripted provider turn.
    let source_request_id = "foreground-parent";
    let source_doc_id = create_runtime_request_with_valid_until(
        db.node.as_ref(),
        db.node_identity.did(),
        AGENT_NAME,
        source_request_id,
        session_id,
        "2020-01-01T00:00:00Z",
        "expired background wake source",
    )
    .await;
    let issuer_did = db.node_identity.did().to_string();
    let process_gate_dir = tempfile::tempdir_in(
        std::env::current_dir().expect("resolve lifecycle fixture working directory"),
    )
    .expect("create held-process gate directory");
    let process_gate = process_gate_dir.path().join("release");
    let held_process_args = serde_json::json!({
        "tool_name": "bash",
        "args": {
            "command": "sh",
            "args": ["-c", r#"while [ ! -f "$1" ]; do sleep 0.05; done"#,
                "held-process", process_gate.to_string_lossy()],
        },
    })
    .to_string();
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "wake-backoff-backend",
            model: "wake-backoff-model",
            parent_behavior_id: AGENT_NAME,
            configured_behavior_ids: &[AGENT_NAME],
            request_id: "failed-wake-backoff",
            session_id,
            prompt: "continue",
            accepted_chunks: vec![StreamChunk::tool_call(
                "wake-backoff-spawn",
                "spawn_process",
                held_process_args,
            )],
            child_plans: Vec::new(),
            valid_until: None,
            subagent_depth: None,
            request_setup: Some(Box::new(move |request| {
                request.input = input;
                request.execution_origin = "scheduled".into();
                request.admission = AgentRequestAdmissionRecord::runtime_local_control(
                    &issuer_did,
                    source_request_id,
                );
                request.caused_by_parent_request_id = Some(source_request_id.into());
                request.caused_by_parent_request_doc_id = Some(source_doc_id);
                // Exercise the owner's capped 60-second persisted backoff.
                request.retry_count = 4;
                request.max_retries = 6;
            })),
        },
    )
    .await;
    prepared.backend.enable_dynamic_followups("continue");
    configure_behavior_tools(
        db.node.as_ref(),
        db.node_identity.did(),
        AGENT_NAME,
        None,
        gents::document_config::Tools {
            tools_id: format!("{AGENT_NAME}:wake-backoff-tools"),
            agent_did: db.node_identity.did().to_string(),
            host: Some(gents::document_config::HostTools {
                bash: Some(gents::document_config::BashTools {
                    mode: gents::BashMode::ReadOnly,
                    read_only_commands: Some(vec!["sh".into()]),
                    background_enabled: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let identity: Arc<dyn gents::AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build failed-wake runtime");
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let running = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if fetch_tool_call_snapshots_for_session(&db.node, session_id)
                .await
                .iter()
                .any(|row| {
                    row.tool_name == "bash" && row.lifecycle_state.as_deref() == Some("running")
                })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    if running.is_err() {
        let state = db.node.execute(
            r#"{ AgentRequest(filter: { request_id: { _eq: "failed-wake-backoff" } }, limit: 1) { request_id lifecycle_state failure_reason retry_count max_retries } }"#,
        ).await;
        panic!(
            "background process did not reach running; request={:?}; provider_calls={}",
            state.data,
            runtime.backend.observed_requests("continue"),
        );
    }
    runtime
        .backend
        .enqueue_response("continue", StreamResponse::bad_request("provider failed"));
    std::fs::write(&process_gate, b"release\n").expect("release held background process");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let request = db.node.execute(
                r#"{ AgentRequest(filter: { request_id: { _eq: "failed-wake-backoff" } }, limit: 1) { request_id lifecycle_state } }"#,
            ).await;
            if first_row::<StatusRow>(&request, "AgentRequest").lifecycle_state
                == Some(RequestLifecycleState::Failed)
                && fetch_message_snapshots_for_session(&db.node, session_id)
                    .await
                    .iter()
                    .any(|message| message.message_key.starts_with("background-completion-notification:"))
                // The notification materializes a successor wake. Shut down
                // only once that wake is inside inference (the third scripted
                // provider call), so shutdown deterministically fails it into
                // its own persisted backoff; shutting down before its claim
                // would leave it pending instead.
                && runtime.backend.observed_requests("continue") >= 3
                && background_wake_retry_rows(&db.node, session_id)
                    .await
                    .iter()
                    .any(|row| {
                        row.request_id.starts_with("background-completion-")
                            && row.lifecycle_state == Some(RequestLifecycleState::Processing)
                    })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("failed wake did not retain canonical background completion notification with its successor wake in inference");
    runtime.shutdown().await;

    // Freeze the exact physical candidate set before the sweep. The sweep may
    // materialize a successor, so a post-sweep session query is not a stable
    // measure of what this invocation scanned.
    let wake_rows_before = background_wake_retry_rows(&db.node, session_id).await;
    let failed_wake = wake_rows_before
        .iter()
        .find(|row| row.request_id == "failed-wake-backoff")
        .expect("physical failed wake must remain persisted");
    assert_eq!(
        failed_wake.lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    let next_retry_at = gents::lifecycle::background_wake_next_retry_at(
        failed_wake.terminalized_at.as_deref(),
        failed_wake.retry_count.expect("persisted retry count"),
    )
    .expect("failed wake must persist a parseable terminal timestamp");
    assert!(
        next_retry_at > chrono::Utc::now(),
        "the explicit sweep must run inside the persisted backoff window: {next_retry_at}"
    );
    assert!(
        wake_rows_before.iter().all(|row| {
            row.retry_parent_request_doc_id.as_deref() != failed_wake.doc_id.as_deref()
        }),
        "the live runtime must not have redriven the failed wake before the bounded sweep"
    );
    let sweep_candidates = wake_rows_before
        .iter()
        .filter(|row| {
            row.agent_did.as_deref() == Some(db.node_identity.did())
                && row.lifecycle_state == Some(RequestLifecycleState::Failed)
                && row.execution_origin.as_deref() == Some("scheduled")
        })
        .map(|row| {
            row.doc_id
                .as_deref()
                .expect("wake candidate physical identity")
        })
        .collect::<Vec<_>>();
    let report =
        RequestLifecycle::redrive_failed_background_wakeups(&db.node, db.node_identity.did())
            .await
            .expect("deferred redrive sweep");
    let wake_rows = background_wake_retry_rows(&db.node, session_id).await;
    let coalesced_pending = wake_rows_before
        .iter()
        .filter(|row| {
            row.lifecycle_state == Some(RequestLifecycleState::Pending)
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .is_some_and(|queue| {
                        queue.source
                            == gents_protocol::request_input::QueueSource::BackgroundCompletion
                            && queue.policy == gents_protocol::request_input::QueuePolicy::Coalesce
                            && queue.key
                                == failed_wake
                                    .input
                                    .as_ref()
                                    .and_then(|input| input.queue.as_ref())
                                    .and_then(|queue| queue.key.clone())
                    })
        })
        .count();
    assert!(
        coalesced_pending <= 1,
        "one canonical pending wake per queue key"
    );
    for candidate in wake_rows_before.iter().filter(|row| {
        row.agent_did.as_deref() == Some(db.node_identity.did())
            && row.lifecycle_state == Some(RequestLifecycleState::Failed)
            && row.execution_origin.as_deref() == Some("scheduled")
    }) {
        assert_eq!(
            candidate
                .input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .and_then(|queue| queue.key.as_ref()),
            failed_wake
                .input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .and_then(|queue| queue.key.as_ref()),
            "all scanned wakes share the physical coalescing key"
        );
    }
    assert_eq!(
        report.scanned,
        sweep_candidates.len(),
        "the sweep must account for the exact failed scheduled candidates present at its boundary: {sweep_candidates:?}"
    );
    assert_eq!(
        report.coalesced,
        if coalesced_pending == 1 {
            report.scanned
        } else {
            0
        }
    );
    assert_eq!(
        report.deferred,
        if coalesced_pending == 0 {
            report.scanned
        } else {
            0
        }
    );
    assert_eq!(
        report.redriven
            + report.deferred
            + report.already_redriven
            + report.coalesced
            + report.ineligible
            + report.failed,
        report.scanned,
        "every scanned canonical wake must have one exact sweep disposition"
    );
    assert_eq!(report.redriven, 0);
    assert_eq!(
        wake_rows, wake_rows_before,
        "neither disposition writes a retry successor"
    );
    assert!(!wake_rows.is_empty());
    let diagnostics = gents::load_background_completion_diagnostics(
        &gents::config_client::ConfigAccess::Local(db.node.clone()),
        db.node_identity.did(),
    )
    .await
    .expect("load persisted completion diagnostics");
    assert_eq!(diagnostics.pending_notifications, 1);
    assert_eq!(diagnostics.stranded_notifications, 0);
    let notification_key = fetch_message_snapshots_for_session(&db.node, session_id)
        .await
        .into_iter()
        .filter(|message| {
            message
                .message_key
                .starts_with("background-completion-notification:")
        })
        .map(|message| message.message_key)
        .collect::<Vec<_>>();
    assert_eq!(
        notification_key.len(),
        1,
        "the failed tool must publish one exact canonical completion notification"
    );
    let notification_key = &notification_key[0];
    let active_epoch = diagnostics
        .epochs
        .iter()
        .filter(|epoch| {
            epoch
                .pending_notification_keys
                .iter()
                .any(|key| key == notification_key)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        active_epoch.len(),
        1,
        "the pending notification must select one physical active wake epoch: {diagnostics:?}"
    );
    let active_epoch = active_epoch[0];
    assert_eq!(active_epoch.state, "retry_backoff");
    assert_eq!(active_epoch.notification_count, 1);
    assert_eq!(active_epoch.acknowledged_notification_count, 0);
    assert!(active_epoch.next_retry_at.is_some());
    assert!(wake_rows
        .iter()
        .any(|row| row.request_id == active_epoch.active_request_id));
    for epoch in diagnostics
        .epochs
        .iter()
        .filter(|epoch| epoch.active_request_id != active_epoch.active_request_id)
    {
        assert_eq!(
            epoch.state, "retry_ineligible_not_latest",
            "a displaced physical wake must be diagnosed as ineligible, not as the active retry epoch"
        );
        assert_eq!(epoch.next_retry_at, None);
    }

    create_request_for_agent_with_signed_fields(
        &db.node,
        db.node_identity.did(),
        "later-interactive-request",
        session_id,
        "pending",
        "2099-01-01T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    crate::support::seed_session_observation_from_request(
        &db.node,
        session_id,
        "later-interactive-request",
        "new user turn",
    )
    .await;
    let displaced = gents::load_background_completion_diagnostics(
        &gents::config_client::ConfigAccess::Local(db.node.clone()),
        db.node_identity.did(),
    )
    .await
    .expect("load displaced completion diagnostics");
    assert_eq!(displaced.pending_notifications, 1);
    assert_eq!(displaced.stranded_notifications, 1);
    assert_eq!(displaced.epochs[0].state, "retry_ineligible_not_latest");
    assert_eq!(displaced.epochs[0].next_retry_at, None);
}

async fn background_wake_retry_rows(
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
) -> Vec<AgentRequestRow> {
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{
                AgentRequest(
                    filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                    order: {{ created_at: ASC }}
                ) {{
                    _docID request_id agent_did requester_did content lifecycle_state execution_origin
                    retry_parent_request retry_parent_request_doc_id retry_root_request retry_count max_retries
                    admission_kind admission_signer_did admission_signature backend_id max_total_tokens
                    input deadline terminalized_at valid_until
                }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "fetch background wake retries: {:?}",
        response.errors
    );
    serde_json::from_value(response.data.expect("wake retry data")["AgentRequest"].clone())
        .expect("decode wake retry rows")
}

async fn recover_interrupted_request_after_crash(
    node: &gents::defra_node::EmbeddedNode,
    doc_id: &str,
    expected_active_child_doc_id: Option<&str>,
) {
    let physical = gents::graphql::escape_graphql_string(doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 2) {{ _docID request_id agent_did requester_did execution_generation }} }}"#,
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load exact request interrupt scope: {:?}",
        response.errors
    );
    let row = first_row::<AgentRequestRow>(&response, "AgentRequest");
    assert_eq!(row.doc_id.as_deref(), Some(doc_id));
    let agent_did = row
        .agent_did
        .as_deref()
        .expect("accepted request principal identity");
    gents::interrupt::interrupt_request_by_doc_id(
        node,
        doc_id,
        agent_did,
        row.requester_did.as_deref(),
    )
    .await
    .expect("latch exact request interrupt through canonical owner");
    let generation = row
        .execution_generation
        .as_deref()
        .expect("crashed request execution generation");
    seed_expired_execution_lease(
        node,
        doc_id,
        Some(generation),
        Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
    )
    .await;
    // Request recovery is principal-wide. In subagent fixtures the crashed
    // runtime also stops renewing the one known child. Preserve only that
    // exact linked child at the active boundary so tool recovery remains the
    // owner of the subsequent cascade/detach decision. Never revive arbitrary
    // same-principal requests.
    let mut protected_child = None;
    if let Some(child_doc_id) = expected_active_child_doc_id {
        let child_physical = gents::graphql::escape_graphql_string(child_doc_id);
        let child = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{child_physical}" }} }}, limit: 2) {{ _docID request_id lifecycle_state execution_generation execution_lease_expires_at caused_by_parent_request_doc_id }} }}"#,
            ))
            .await;
        assert!(
            !child.has_errors(),
            "load exact active recovery child: {:?}",
            child.errors
        );
        let child = first_row::<AgentRequestRow>(&child, "AgentRequest");
        assert_eq!(child.doc_id.as_deref(), Some(child_doc_id));
        assert_eq!(
            child.caused_by_parent_request_doc_id.as_deref(),
            Some(doc_id),
            "protected child must retain exact physical parent lineage"
        );
        match child.lifecycle_state {
            Some(RequestLifecycleState::Pending) => {
                assert!(
                    child.execution_generation.is_none()
                        && child.execution_lease_expires_at.is_none(),
                    "pending child must not carry execution ownership: {child:?}"
                );
            }
            Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing) => {
                let child_generation = child
                    .execution_generation
                    .as_deref()
                    .expect("active recovery child generation");
                seed_expired_execution_lease(
                    node,
                    child_doc_id,
                    Some(child_generation),
                    Some(chrono::Utc::now() + chrono::Duration::minutes(5)),
                )
                .await;
            }
            _ => panic!("protected child must remain non-terminal: {child:?}"),
        }
        protected_child = Some((child_doc_id.to_owned(), child.request_id));
    }
    let report = RequestLifecycle::recover_all(node, agent_did)
        .await
        .expect("recover exact interrupted request after crash");
    assert_eq!(
        report.requests_recovered, 1,
        "only the selected expired interrupted owner may terminalize before tool recovery; protected child={protected_child:?}"
    );
    let recovered = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 1) {{ request_id lifecycle_state }} }}"#,
        ))
        .await;
    assert_eq!(
        first_row::<StatusRow>(&recovered, "AgentRequest").lifecycle_state,
        Some(RequestLifecycleState::Interrupted),
        "canonical request recovery must establish the terminal-parent boundary"
    );
}

async fn seed_accepted_request_projection(
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
    request_id: &str,
) {
    create_agent_session(node, session_id, AGENT_NAME, "2026-03-23T00:00:00Z").await;
    crate::support::seed_session_observation_from_request(
        node,
        session_id,
        request_id,
        "stuck request",
    )
    .await;
}

/// Fixture fault injection only: force the exact stored lease fields the
/// production recovery predicate compares. No live path may write these
/// directly; the claim owner and the bounded renewal are the only writers.
/// The `generation` argument is the exact `execution_generation` string
/// recovery must match; `expires_at` in the past makes recovery eligible.
/// `expires_at: None` clears the lease entirely — recovery then treats the
/// active request as missing an execution lease.
async fn seed_expired_execution_lease(
    node: &gents::defra_node::EmbeddedNode,
    request_doc_id: &str,
    generation: Option<&str>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let request_doc_id = gents::graphql::escape_graphql_string(request_doc_id);
    let generation_field = match generation {
        Some(value) => format!(
            r#"execution_generation: "{}","#,
            gents::graphql::escape_graphql_string(value)
        ),
        None => "execution_generation: null,".into(),
    };
    let expiry_field = match expires_at {
        Some(value) => format!(
            r#"execution_lease_expires_at: "{}","#,
            gents::graphql::escape_graphql_string(&value.to_rfc3339())
        ),
        None => "execution_lease_expires_at: null,".into(),
    };
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }},
                input: {{
                    {generation_field}
                    {expiry_field}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set execution lease failed: {:?}",
        response.errors
    );
}

/// The exact provider-turn output coordinate the production writer allocates
/// (`streaming.rs`): one inference capture at the first per-request allocation,
/// first turn, first attempt. Recovery enumerates sources from committed
/// segments, so the fixture and recovery must name the identical coordinate.
fn fixture_provider_turn_source() -> gents_protocol::output::OutputSource {
    gents_protocol::output::OutputSource::ProviderTurn {
        scope: gents_protocol::rendered_request::CaptureScope {
            kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
            seq: 1,
        },
        turn_index: 0,
        attempt: 0,
    }
}

/// The production writer's provider-turn header identity (`streaming.rs`):
/// `provider:{request_doc_id}:{serde_json(OutputSource)}`. Recovery keys its
/// own partial publications `request-recovery:{request_doc_id}:{...}` — the
/// fixture mirrors the writer, never the retired `provider-turn:` literal.
fn fixture_provider_message_key(
    request_doc_id: &str,
    source: &gents_protocol::output::OutputSource,
) -> String {
    format!(
        "provider:{request_doc_id}:{}",
        serde_json::to_string(source).expect("serializing fixture output source")
    )
}

/// Reuse the shared pinned-driver create/add mutation normalization.
fn canonical_created_doc_id(
    response: &gents::defra_node::QueryResponse,
    collection: &str,
) -> String {
    gents::graphql::single_mutation_document(response, &format!("create_{collection}"))
        .expect("decode canonical mutation response")
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!(
                "pinned create_{collection} response shape: {:?}",
                response.errors
            )
        })
}

/// Seed one canonical, complete provider turn for an expired generation,
/// exactly as the production `DefraStreamWriter` publication path writes it:
/// a fully-closed `OutputSegment` whose ordinal run carries the streamed text,
/// followed by a `RequestExecution`-published assistant transcript header
/// whose text block references that closing record. This is the canonical
/// replacement of the retired `AgentResponse`/`progress_seq` fixture model —
/// content lives in the canonical transcript, not in a status row.
/// Returns `(closing_segment_doc_id, header_message_doc_id)`.
#[allow(clippy::too_many_arguments)]
async fn seed_completed_canonical_output(
    node: &gents::defra_node::EmbeddedNode,
    request_doc_id: &str,
    session_id: &str,
    agent_did: &str,
    generation: &str,
    content: &str,
    created_at: &str,
) -> (String, String) {
    let source = fixture_provider_turn_source();
    let segment = gents_protocol::output::OutputSegment {
        agent_did: agent_did.to_string(),
        requester_did: None,
        session_id: session_id.to_string(),
        request_doc_id: request_doc_id.to_string(),
        source: source.clone(),
        writer: gents_protocol::output::OutputWriter::RequestExecution {
            execution_generation: generation.to_string(),
        },
        ordinal: Some(0),
        runs: vec![gents_protocol::output::SegmentRun {
            stream: 0,
            bytes: content.len() as u32,
            declaration: Some(gents_protocol::output::StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: gents_protocol::output::StreamPayload::Text,
            }),
        }],
        payload: content.to_string(),
        close: Some(gents_protocol::output::SourceClose::Closed {
            outcome: gents_protocol::output::OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![content.len() as u64],
        }),
        created_at: created_at.to_string(),
    };
    let segment_response = node
        .execute_request_with_retry(
            gents::defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            gents::defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !segment_response.has_errors(),
        "canonical segment fixture create failed: {:?}",
        segment_response.errors
    );
    let segment_doc_id = canonical_created_doc_id(&segment_response, "AgentOutputSegment");
    let close_doc_id = segment_doc_id.clone();
    let message = gents_protocol::output::TranscriptMessage {
        message_key: fixture_provider_message_key(request_doc_id, &source),
        agent_did: agent_did.to_string(),
        session_id: session_id.to_string(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.to_string()),
        publication: gents_protocol::output::MessagePublication::RequestExecution {
            execution_generation: generation.to_string(),
        },
        outcome: gents_protocol::output::OutputOutcome::Complete,
        sequence: 1,
        role: gents_protocol::output::MessageRole::Assistant,
        native_id: None,
        blocks: vec![gents_protocol::output::MessageBlock::Text {
            text: gents_protocol::output::PresentedPayload {
                output: gents_protocol::output::PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: 0,
                },
                presentation: gents_protocol::output::PayloadPresentation::Full,
            },
        }],
        created_at: created_at.to_string(),
    };
    let message_response = node
        .execute_request_with_retry(
            gents::defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
            gents::defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !message_response.has_errors(),
        "canonical message fixture create failed: {:?}",
        message_response.errors
    );
    let message_doc_id = canonical_created_doc_id(&message_response, "AgentMessage");
    (segment_doc_id, message_doc_id)
}

/// Seed one canonical, open (unclosed) provider-turn flush under the expired
/// generation — the interrupted-mid-stream state the production writer leaves
/// behind. Recovery enumerates exactly this shape, closes it Partial, and
/// publishes the retained text under a fresh `RequestRecovery` publication.
#[allow(clippy::too_many_arguments)]
async fn seed_open_canonical_flush(
    node: &gents::defra_node::EmbeddedNode,
    request_doc_id: &str,
    session_id: &str,
    agent_did: &str,
    generation: &str,
    content: &str,
    created_at: &str,
) -> String {
    let segment = gents_protocol::output::OutputSegment {
        agent_did: agent_did.to_string(),
        requester_did: None,
        session_id: session_id.to_string(),
        request_doc_id: request_doc_id.to_string(),
        source: fixture_provider_turn_source(),
        writer: gents_protocol::output::OutputWriter::RequestExecution {
            execution_generation: generation.to_string(),
        },
        ordinal: Some(0),
        runs: vec![gents_protocol::output::SegmentRun {
            stream: 0,
            bytes: content.len() as u32,
            declaration: Some(gents_protocol::output::StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: gents_protocol::output::StreamPayload::Text,
            }),
        }],
        payload: content.to_string(),
        close: None,
        created_at: created_at.to_string(),
    };
    let segment_response = node
        .execute_request_with_retry(
            gents::defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            gents::defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !segment_response.has_errors(),
        "open canonical segment fixture create failed: {:?}",
        segment_response.errors
    );
    canonical_created_doc_id(&segment_response, "AgentOutputSegment")
}

#[tokio::test]
async fn recover_all_marks_requests_as_error() {
    let db = test_db("lifecycle-recover-error").await;
    let request_doc_id = create_request(
        &db.node,
        "stuck-1",
        "session-1",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    seed_expired_execution_lease(
        &db.node,
        &request_doc_id,
        Some("expired-generation"),
        Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
    )
    .await;
    seed_accepted_request_projection(&db.node, "session-1", "stuck-1").await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 1);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "stuck-1" } },
                    limit: 1
                ) { request_id lifecycle_state execution_generation }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(request.lifecycle_state, Some(RequestLifecycleState::Failed));
    assert_ne!(
        request.execution_generation.as_deref(),
        Some("expired-generation"),
        "recovery must take ownership with a fresh generation"
    );
}

#[tokio::test]
async fn recover_all_preserves_completed_canonical_output_after_lease_expiry() {
    let db = test_db("lifecycle-recover-complete").await;
    let request_doc_id = create_request(
        &db.node,
        "stuck-complete",
        "session-complete",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    seed_expired_execution_lease(
        &db.node,
        &request_doc_id,
        Some("expired-completed-generation"),
        Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
    )
    .await;
    seed_accepted_request_projection(&db.node, "session-complete", "stuck-complete").await;
    let (segment_doc_id, header_doc_id) = seed_completed_canonical_output(
        &db.node,
        &request_doc_id,
        "session-complete",
        AGENT_DID,
        "expired-completed-generation",
        "complete",
        "2026-03-23T00:00:05Z",
    )
    .await;
    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 1);

    let escaped_request_doc_id = gents::graphql::escape_graphql_string(&request_doc_id);
    let request_resp = db
        .node
        .execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_request_doc_id}" }} }}, limit: 1)
            {{ request_id lifecycle_state failure_reason terminal_output }} }}"#))
        .await;
    let request = first_row::<StatusRow>(&request_resp, "AgentRequest");
    assert_eq!(
        request.request_id, "stuck-complete",
        "the exact physical request document must be addressed by doc id"
    );
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Failed),
        "an expired lease terminalizes as failed even when its output completed"
    );
    assert_eq!(
        request.failure_reason.as_deref(),
        Some("execution lease expired"),
        "production recovery records the exact lease-expiry failure reason"
    );
    assert_eq!(
        request.terminal_output,
        Some(gents_protocol::output::TerminalOutput::Message {
            message_doc_id: header_doc_id.clone(),
        }),
        "the completed source is closed Complete and headed, so recovery \
         selects the exact existing header rather than republishing"
    );

    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentMessage(
                    filter: {{ request_doc_id: {{ _eq: "{escaped_request_doc_id}" }} }},
                    limit: 1
                ) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        ))
        .await;
    let message =
        decode_transcript_message_row(&first_row::<serde_json::Value>(&response, "AgentMessage"))
            .unwrap();
    assert_eq!(
        message.message.message_key,
        fixture_provider_message_key(&request_doc_id, &fixture_provider_turn_source()),
        "completed canonical output must be preserved"
    );
    assert_eq!(
        message.message.outcome,
        gents_protocol::output::OutputOutcome::Complete
    );
    let [gents_protocol::output::MessageBlock::Text { text }] = message.message.blocks.as_slice()
    else {
        panic!("a completed fixture message carries exactly one text block")
    };
    assert_eq!(
        text.presentation,
        gents_protocol::output::PayloadPresentation::Full,
        "full presentation payload must be preserved"
    );
    assert_eq!(
        text.output.close_doc_id, segment_doc_id,
        "the retained text block must reference the exact closing record"
    );
    assert!(
        !message.message.blocks.iter().any(|block| matches!(
            block,
            gents_protocol::output::MessageBlock::Text { text }
                if matches!(text.presentation, gents_protocol::output::PayloadPresentation::Composed { .. })
        )),
        "recovery must not downgrade a complete publication to partial"
    );
}
#[tokio::test]
async fn recover_all_leaves_live_execution_lease_untouched() {
    let db = test_db("lifecycle-recover-live-lease").await;
    let request_doc_id = create_request(
        &db.node,
        "live-request",
        "live-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    seed_expired_execution_lease(
        &db.node,
        &request_doc_id,
        Some("live-generation"),
        Some(chrono::Utc::now() + chrono::Duration::minutes(5)),
    )
    .await;
    seed_accepted_request_projection(&db.node, "live-session", "live-request").await;
    let (_segment_doc_id, _header_doc_id) = seed_completed_canonical_output(
        &db.node,
        &request_doc_id,
        "live-session",
        AGENT_DID,
        "live-generation",
        "still running",
        "2026-03-23T00:00:05Z",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.responses_recovered, 0);
    assert_eq!(report.requests_recovered, 0);

    let request_response = db
        .node
        .execute(
            r#"{
                AgentRequest(filter: { request_id: { _eq: "live-request" } }, limit: 1) {
                    request_id lifecycle_state execution_generation execution_lease_expires_at
                }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&request_response, "AgentRequest");
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );
    assert_eq!(
        request.execution_generation.as_deref(),
        Some("live-generation"),
        "a live lease must keep its exact stored generation"
    );

    let escaped_request_doc_id = gents::graphql::escape_graphql_string(&request_doc_id);
    let message = db
        .node
        .execute(&format!(
            r#"{{ AgentMessage(
                    filter: {{ request_doc_id: {{ _eq: "{escaped_request_doc_id}" }} }},
                    limit: 1
                ) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        ))
        .await;
    let message =
        decode_transcript_message_row(&first_row::<serde_json::Value>(&message, "AgentMessage"))
            .unwrap();
    let [gents_protocol::output::MessageBlock::Text { text }] = message.message.blocks.as_slice()
    else {
        panic!("the live fixture message carries exactly one text block")
    };
    assert_eq!(
        text.presentation,
        gents_protocol::output::PayloadPresentation::Full
    );
    assert_eq!(
        message.message.message_key,
        fixture_provider_message_key(&request_doc_id, &fixture_provider_turn_source())
    );
    assert_eq!(
        message.message.publication,
        gents_protocol::output::MessagePublication::RequestExecution {
            execution_generation: "live-generation".to_string()
        },
        "live canonical output must be untouched"
    );
}

#[tokio::test]
async fn recover_all_interrupts_an_expired_lease_with_a_durable_interrupt() {
    let db = test_db("lifecycle-recover-expired-interrupt").await;
    let request_doc_id = create_request(
        &db.node,
        "interrupted-request",
        "interrupted-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    seed_expired_execution_lease(
        &db.node,
        &request_doc_id,
        Some("expired-interrupt-generation"),
        Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
    )
    .await;
    seed_accepted_request_projection(&db.node, "interrupted-session", "interrupted-request").await;
    // The interrupted-mid-stream state: one open provider-turn flush under the
    // expired generation. Recovery enumerates exactly this shape, closes it
    // Partial, and publishes the retained text under a fresh generation.
    seed_open_canonical_flush(
        &db.node,
        &request_doc_id,
        "interrupted-session",
        AGENT_DID,
        "expired-interrupt-generation",
        "partial turn",
        "2026-03-23T00:00:05Z",
    )
    .await;
    let interrupt_requested_at = chrono::Utc::now().to_rfc3339();
    let escaped_request_doc_id = gents::graphql::escape_graphql_string(&request_doc_id);
    let escaped_interrupt_requested_at =
        gents::graphql::escape_graphql_string(&interrupt_requested_at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_request_doc_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_interrupt_requested_at}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set durable interrupt failed: {:?}",
        response.errors
    );

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.requests_recovered, 1);
    assert_eq!(report.responses_recovered, 1);

    let request_response = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "interrupted-request" } },
                    limit: 1
                ) { request_id lifecycle_state failure_reason execution_generation }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&request_response, "AgentRequest");
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Interrupted)
    );
    assert_eq!(
        request.failure_reason.as_deref(),
        Some("execution lease expired"),
        "recovery records the exact lease-expiry failure reason"
    );
    assert_ne!(
        request.execution_generation.as_deref(),
        Some("expired-interrupt-generation"),
        "recovery must take ownership with a fresh generation"
    );

    // The closing record: recovery committed the exact Partial closure.
    let segment_response = db
        .node
        .execute(&format!(
            r#"{{ AgentOutputSegment(
                    filter: {{ request_doc_id: {{ _eq: "{escaped_request_doc_id}" }} }},
                    limit: 2
                ) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
        ))
        .await;
    assert!(
        !segment_response.has_errors(),
        "fetch recovered segments failed: {:?}",
        segment_response.errors
    );
    let segments: Vec<OutputSegmentRow> = segment_response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(|rows| rows.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| decode_output_segment_row(row).expect("decode recovered segment"))
                .collect()
        })
        .expect("segment data present");
    assert_eq!(
        segments.len(),
        2,
        "recovery adds a terminal-only closing record; the open flush is retained"
    );
    let close = segments
        .iter()
        .find(|row| row.segment.close.is_some())
        .expect("recovery commits exactly one closing record");
    assert_eq!(
        close.segment.close,
        Some(gents_protocol::output::SourceClose::Closed {
            outcome: gents_protocol::output::OutputOutcome::Partial,
            segments: 1,
            stream_bytes: vec!["partial turn".len() as u64],
        }),
        "the exact retained extent is closed Partial"
    );

    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentMessage(
                    filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }},
                    limit: 1
                ) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        ))
        .await;
    let message =
        decode_transcript_message_row(&first_row::<serde_json::Value>(&response, "AgentMessage"))
            .unwrap();
    assert_eq!(
        message.message.outcome,
        gents_protocol::output::OutputOutcome::Partial,
        "interruption recovery publishes a partial canonical transcript message"
    );
    assert!(
        matches!(
            &message.message.publication,
            gents_protocol::output::MessagePublication::RequestRecovery { execution_generation }
                if !execution_generation.is_empty()
                    && execution_generation != "expired-interrupt-generation"
        ),
        "recovery publishes under its fresh recovery generation, never the \
         expired one; got {:?}",
        message.message.publication
    );
    assert!(
        message
            .message
            .message_key
            .starts_with(&format!("request-recovery:{request_doc_id}:")),
        "recovery keys its publication request-recovery:{{doc_id}}:{{source}}; got {:?}",
        message.message.message_key
    );
    let [gents_protocol::output::MessageBlock::Text { text }] = message.message.blocks.as_slice()
    else {
        panic!("recovery retains exactly the one text stream of the open flush")
    };
    assert_eq!(
        text.presentation,
        gents_protocol::output::PayloadPresentation::Full,
        "recovered text retains its full presentation"
    );
    assert_eq!(
        text.output.close_doc_id, close.doc_id,
        "the recovered block references the exact closing record recovery committed"
    );
}

#[tokio::test]
async fn recover_all_times_out_expired_running_tool_calls() {
    let db = test_db("tool-call-recover-timeout").await;
    let (runtime, _request_id, session_id, tool_doc_id) =
        boot_running_recovery_bash(&db, "tool-timeout", "tool-timeout-call").await;
    let response = db.node.execute(&format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ deadline_at: "2020-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
        gents::graphql::escape_graphql_string(&tool_doc_id),
    )).await;
    assert!(
        !response.has_errors(),
        "expire accepted tool: {:?}",
        response.errors
    );

    let report = ToolCallLifecycle::recover_all(&db.node, db.node_identity.did())
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, &session_id).await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("timedOut"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("deadline"));
    assert!(snapshots[0]
        .load_result(db.node.clone())
        .await
        .contains("deadline exceeded"));
    runtime.shutdown().await;
}

#[tokio::test]
async fn recover_all_repairs_terminal_background_tool_notification_once() {
    let db = test_db("tool-call-repair-notification").await;
    let agent_did = db.node_identity.did().to_string();
    let session_id = "tool-notification-session";
    let behavior_id = AGENT_NAME;
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "tool-notification-backend",
            model: "tool-notification-model",
            parent_behavior_id: behavior_id,
            configured_behavior_ids: &[behavior_id],
            request_id: "tool-notification-req",
            session_id,
            prompt: "run background notification fixture",
            accepted_chunks: vec![StreamChunk::tool_call(
                "tool-notification-spawn",
                "spawn_process",
                r#"{"tool_name":"bash","args":{"command":"printf","args":["durable result"]}}"#,
            )],
            child_plans: Vec::new(),
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    configure_behavior_tools(
        db.node.as_ref(),
        &agent_did,
        behavior_id,
        None,
        gents::document_config::Tools {
            tools_id: format!("{behavior_id}:notification-tools"),
            agent_did: agent_did.clone(),
            host: Some(gents::document_config::HostTools {
                bash: Some(gents::document_config::BashTools {
                    mode: gents::BashMode::ReadOnly,
                    read_only_commands: Some(vec!["printf".into()]),
                    background_enabled: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let identity: Arc<dyn gents::AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build notification runtime");
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let terminal_tool = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(row) = fetch_tool_call_snapshots_for_session(&db.node, session_id)
                .await
                .into_iter()
                .find(|row| {
                    row.tool_name == "bash" && row.lifecycle_state.as_deref() == Some("completed")
                })
            {
                break row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("background bash did not complete");
    let tool_doc_id = terminal_tool.doc_id.clone();
    let notification_key = format!("background-completion-notification:{tool_doc_id}:tool");
    // Stop the live observer before establishing the lost-ack crash shape;
    // otherwise it can legitimately reconcile the marker ahead of the
    // explicit startup recovery call under test.
    runtime.shutdown().await;
    // Simulate the durable pre-ack cursor, not corrupt transcript storage. The
    // immutable canonical notification remains present while the lifecycle
    // row says its native side effects still need convergence.
    let response = db.node.execute(&format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ status: "completionPending", completion_notification_delivered_at: null }}) {{ _docID }} }}"#,
        gents::graphql::escape_graphql_string(&tool_doc_id),
    )).await;
    assert!(
        !response.has_errors(),
        "remove delivered notification fixture: {:?}",
        response.errors
    );

    let first = ToolCallLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(first.notifications_repaired, 1);
    let second = ToolCallLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(second.notifications_repaired, 0);

    let messages = fetch_message_snapshots_for_session(&db.node, session_id).await;
    let matching = messages
        .iter()
        .filter(|message| message.message_key == notification_key)
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "repair must be durably idempotent by exact key"
    );
    assert!(matching[0].content.contains("durable result"));

    let response = db
        .node
        .execute(&format!(
            r#"{{
                AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{ completion_notification_delivered_at }}
            }}"#,
            gents::graphql::escape_graphql_string(&tool_doc_id),
        ))
        .await;
    let row = first_row::<NotificationDeliveryRow>(&response, "AgentToolCall");
    assert!(
        row.completion_notification_delivered_at.is_some(),
        "successful notification append must advance the delivery marker"
    );
}

#[tokio::test]
async fn recover_all_cancels_running_tool_call_for_interrupted_parent_only() {
    let db = test_db("tool-call-recover-cancel").await;
    let ([interrupted_runtime, unrelated_runtime], rows) =
        boot_two_running_recovery_bashes(&db).await;
    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID request_id }} }}"#,
            gents::graphql::escape_graphql_string(&rows[0].0),
        ))
        .await;
    let interrupted_doc = first_row::<AgentRequestRow>(&response, "AgentRequest")
        .doc_id
        .expect("accepted interrupted request physical identity");
    interrupted_runtime.crash().await;
    recover_interrupted_request_after_crash(&db.node, &interrupted_doc, None).await;

    let report = ToolCallLifecycle::recover_all(&db.node, db.node_identity.did())
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let cancelled_snapshots = fetch_tool_call_snapshots_for_session(&db.node, &rows[0].1).await;
    assert_eq!(
        cancelled_snapshots
            .iter()
            .find(|row| row.doc_id == rows[0].2)
            .and_then(|row| row.lifecycle_state.as_deref()),
        Some("cancelled")
    );
    assert_eq!(
        cancelled_snapshots
            .iter()
            .find(|row| row.doc_id == rows[0].2)
            .and_then(|row| row.cancel_cause.as_deref()),
        Some("interrupted")
    );

    let unrelated_snapshots = fetch_tool_call_snapshots_for_session(&db.node, &rows[1].1).await;
    assert_eq!(
        unrelated_snapshots
            .iter()
            .find(|row| row.doc_id == rows[1].2)
            .and_then(|row| row.lifecycle_state.as_deref()),
        Some("running"),
        "unrelated running tool call should not be swept"
    );
    unrelated_runtime.shutdown().await;
}

#[tokio::test]
/// A crash between the parent's interrupt latch and the live hook's
/// retention leaves the awaited bridge foreground. Request recovery's terminal
/// accounting backgrounds it with its receipt, so the child's later terminal
/// is still delivered to the session.
async fn crash_between_interrupt_latch_and_retain_still_delivers_child_completion() {
    let contract: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("load generated recovery contract");
    let modeled = contract["restart_disposition_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "restart_awaited_bridge_interrupted_parent_backgrounded")
        .expect("Lean awaited-bridge restart witness");
    assert_eq!(modeled["disposition"], "retain_in_background");
    assert_eq!(modeled["post_await_mode"], "background");
    let db = test_db("tool-call-recover-cascade").await;
    let (runtime, interrupted_doc, bridge_doc_id, child_request_id, child_request_doc_id) =
        boot_running_recovery_subagent(&db, "tool-cascade", "foreground", "cascade").await;
    assert_unique_request_identity(
        &db.node,
        &child_request_id,
        &child_request_doc_id,
        &bridge_doc_id,
    )
    .await;
    // Crash and join the live runtime so only startup recovery owns the transition.
    runtime.crash().await;
    // An awaited same-principal spawn carries the foreground unclaimed bound
    // (#1830); the terminal accounting's background flip must drop it.
    let armed = db
        .node
        .execute(&format!(
            r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ unclaimed_deadline_at: "2099-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
            gents::graphql::escape_graphql_string(&bridge_doc_id),
        ))
        .await;
    assert!(!armed.has_errors(), "{:?}", armed.errors);
    recover_interrupted_request_after_crash(
        &db.node,
        &interrupted_doc,
        Some(&child_request_doc_id),
    )
    .await;

    let report = ToolCallLifecycle::recover_all(&db.node, db.node_identity.did())
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "tool-cascade-parent-session").await;
    let bridge = snapshots
        .iter()
        .find(|row| row.doc_id == bridge_doc_id)
        .expect("accepted cascade bridge remains queryable");
    assert_eq!(bridge.lifecycle_state.as_deref(), Some("running"));
    let bridge_row = db
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ await_mode unclaimed_deadline_at }} }}"#,
            gents::graphql::escape_graphql_string(&bridge_doc_id),
        ))
        .await;
    assert!(!bridge_row.has_errors(), "{:?}", bridge_row.errors);
    let bridge_row = bridge_row.data.unwrap()["AgentToolCall"][0].clone();
    assert_eq!(
        bridge_row["await_mode"], "background",
        "the interrupted parent's terminal accounting backgrounds its awaited bridge"
    );
    assert!(
        bridge_row["unclaimed_deadline_at"].is_null(),
        "a same-principal spawn retained in background carries no unclaimed bound"
    );
    assert!(
        !bridge.load_result(db.node.clone()).await.is_empty(),
        "the backgrounded bridge owns its invocation receipt"
    );

    let child_interrupt =
        fetch_interrupt_requested_at_by_doc(&db.node, &child_request_doc_id).await;
    assert!(
        child_interrupt.is_none(),
        "recovery of an interrupted parent must not reach its child"
    );

    // The child's own terminal is delivered to the parent session.
    let failed = db
        .node
        .execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "failed", failure_reason: "child failed after parent interrupt" }}) {{ _docID }} }}"#,
            gents::graphql::escape_graphql_string(&child_request_doc_id),
        ))
        .await;
    assert!(!failed.has_errors(), "{:?}", failed.errors);
    ToolCallLifecycle::recover_all(&db.node, db.node_identity.did())
        .await
        .unwrap();
    let projected =
        fetch_tool_call_snapshots_for_session(&db.node, "tool-cascade-parent-session").await;
    assert_eq!(
        projected
            .iter()
            .find(|row| row.doc_id == bridge_doc_id)
            .and_then(|row| row.lifecycle_state.as_deref()),
        Some("failed")
    );
    assert!(
        fetch_message_snapshots_for_session(&db.node, "tool-cascade-parent-session")
            .await
            .iter()
            .any(|message| message.content.contains("<subagent-notification")),
        "the child's terminal is delivered as a completion notification"
    );
    assert_unique_request_identity(
        &db.node,
        &child_request_id,
        &child_request_doc_id,
        &bridge_doc_id,
    )
    .await;
}

#[tokio::test]
async fn recover_all_preserves_detached_bridge_without_interrupting_child() {
    let contract: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("load generated recovery contract");
    let modeled = contract["restart_disposition_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "restart_detached_bridge_interrupted_parent_retained")
        .expect("Lean detached restart witness");
    assert_eq!(modeled["disposition"], "retain_in_background");
    assert!(modeled["terminal_state"].is_null());
    let db = test_db("tool-call-recover-detach").await;
    let (runtime, interrupted_doc, bridge_doc_id, child_request_id, child_request_doc_id) =
        boot_running_recovery_subagent(&db, "tool-detach", "background", "detach").await;
    assert_unique_request_identity(
        &db.node,
        &child_request_id,
        &child_request_doc_id,
        &bridge_doc_id,
    )
    .await;
    // Crash and join the live runtime so its interrupt observer cannot race recovery.
    runtime.crash().await;
    recover_interrupted_request_after_crash(
        &db.node,
        &interrupted_doc,
        Some(&child_request_doc_id),
    )
    .await;

    let report = ToolCallLifecycle::recover_all(&db.node, db.node_identity.did())
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "tool-detach-parent-session").await;
    let bridge = snapshots
        .iter()
        .find(|row| row.doc_id == bridge_doc_id)
        .expect("accepted detached bridge remains queryable");
    let child = db.node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_id lifecycle_state failure_reason interrupt_requested_at terminalized_at }} }}"#,
        gents::graphql::escape_graphql_string(&child_request_doc_id),
    )).await;
    assert_eq!(
        bridge.lifecycle_state.as_deref(),
        Some("running"),
        "the modeled restart leaves an interrupted parent's detached bridge running; child data={:?}, errors={:?}",
        child.data, child.errors,
    );

    let child_interrupt =
        fetch_interrupt_requested_at_by_doc(&db.node, &child_request_doc_id).await;
    assert!(
        child_interrupt.is_none(),
        "detached recovery should not interrupt the child request"
    );
    assert_unique_request_identity(
        &db.node,
        &child_request_id,
        &child_request_doc_id,
        &bridge_doc_id,
    )
    .await;
}
