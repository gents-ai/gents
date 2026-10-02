use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gents::goal::{
    claim_continuation, claim_retry_continuation, create_goal_for_session,
    delete_goals_for_session, load_canonical_goal, load_goals_for_session, session_token_usage,
    set_goal, set_goal_from_access, update_goal_fields_if_status, CreateGoalForSessionError,
    CreateGoalForSessionOutcome, GoalStatus,
};
use gents::{
    ActiveRuntimeSnapshot, ConfigAccess, GoalSource, TriggerSource, UpdateSubscriptionSource,
};
use serde::Deserialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::support::mock_subscription::MockUpdateSubscriptionSource;
use crate::support::{
    create_request_for_agent_with_signed_fields, set_request_lifecycle_state, test_db, TestDb,
};

const SESSION: &str = "goal-session";
const RESCAN: Duration = Duration::from_millis(20);

/// Seed an already-committed canonical terminal observation for GoalSource.
///
/// This fixture deliberately bypasses provider publication and therefore
/// proves neither acceptance nor transaction/lease behavior; those guarantees
/// belong to owned-loop tests. It supplies a physical generation explicitly so
/// canonical output never relies on an invented fallback during observation.
async fn publish_canonical_terminal_activity(db: &TestDb, request_doc_id: &str, content: &str) {
    use gents::defra_node::{ExecuteRetryPolicy, QueryRequest};
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TerminalOutput, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let escaped = gents::graphql::escape_graphql_string(request_doc_id);
    let response = db.node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ agent_did requester_did session_id execution_generation }} }}"#
    )).await;
    let row = response
        .data
        .as_ref()
        .and_then(|data| data["AgentRequest"].as_array())
        .and_then(|rows| rows.first())
        .expect("canonical request row");
    let agent_did = row["agent_did"].as_str().expect("request agent");
    let requester_did = row["requester_did"].as_str();
    let session_id = row["session_id"].as_str().expect("request session");
    let generation = match row["execution_generation"].as_str() {
        Some(generation) => generation.to_owned(),
        None => {
            let generation = format!("goal-observation-{request_doc_id}");
            let escaped_generation = gents::graphql::escape_graphql_string(&generation);
            let response = db.node.execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, input: {{ execution_generation: "{escaped_generation}" }}) {{ _docID }} }}"#)).await;
            assert!(
                !response.has_errors(),
                "seed goal observation generation: {:?}",
                response.errors
            );
            generation
        }
    };
    let sequence_response = db.node.execute(&format!(
        r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}, order: {{ sequence: DESC }}, limit: 1) {{ sequence }} }}"#,
        gents::graphql::escape_graphql_string(session_id), gents::graphql::escape_graphql_string(agent_did)
    )).await;
    let sequence = sequence_response
        .data
        .as_ref()
        .and_then(|data| data["AgentMessage"].as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row["sequence"].as_u64())
        .unwrap_or(0) as u32
        + 1;
    let source = OutputSource::ProviderTurn {
        scope: CaptureScope {
            kind: CaptureScopeKind::Inference,
            seq: u64::from(sequence),
        },
        turn_index: 0,
        attempt: 0,
    };
    let segment = OutputSegment {
        agent_did: agent_did.into(),
        requester_did: requester_did.map(Into::into),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source,
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: content.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: content.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![content.len() as u64],
        }),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let created = db
        .node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !created.has_errors(),
        "publish goal output: {:?}",
        created.errors
    );
    let close_doc_id =
        gents::graphql::single_mutation_document(&created, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
    let message = TranscriptMessage {
        message_key: gents::session::sequence_message_key(
            agent_did,
            session_id,
            requester_did,
            sequence,
        ),
        session_id: session_id.into(),
        agent_did: agent_did.into(),
        requester_did: requester_did.map(Into::into),
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::Assistant,
        native_id: Some(format!("goal-terminal-{sequence}")),
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id,
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let created = db
        .node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !created.has_errors(),
        "publish goal message: {:?}",
        created.errors
    );
    let message_doc_id = gents::graphql::single_mutation_document(&created, "create_AgentMessage")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let terminal = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(TerminalOutput::Message { message_doc_id }).unwrap(),
    )
    .unwrap();
    let response = db.node.execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, input: {{ terminal_output: {terminal} }}) {{ _docID }} }}"#)).await;
    assert!(
        !response.has_errors(),
        "select goal terminal output: {:?}",
        response.errors
    );
}

async fn select_no_terminal_message(db: &TestDb, request_doc_id: &str) {
    let escaped = gents::graphql::escape_graphql_string(request_doc_id);
    let terminal = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(gents_protocol::output::TerminalOutput::NoMessage).unwrap(),
    )
    .unwrap();
    let response = db.node.execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, input: {{ terminal_output: {terminal} }}) {{ _docID }} }}"#)).await;
    assert!(
        !response.has_errors(),
        "select no terminal output: {:?}",
        response.errors
    );
}

fn snapshot(local_did: &str) -> Arc<ActiveRuntimeSnapshot> {
    Arc::new(ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: local_did.to_string(),
        default_behavior_id: crate::support::AGENT_NAME.to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    })
}

/// Publish the runtime-authored readiness row GoalSource consumes, with the
/// router aligned on `generation`.
async fn publish_behavior_readiness(
    db: &TestDb,
    process_state: gents_protocol::row::BehaviorReadinessProcessState,
    behavior: Option<gents_protocol::row::BehaviorReadinessUnavailableReason>,
) {
    use gents_protocol::row::{
        BehaviorReadinessEntry, BehaviorReadinessSnapshot, BehaviorReadinessState,
        BEHAVIOR_READINESS_FORMAT_VERSION,
    };
    let snapshot = BehaviorReadinessSnapshot {
        format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
        process_state,
        active_generation: 1,
        router_generation: 1,
        default_behavior_id: crate::support::AGENT_NAME.to_string(),
        behaviors: vec![BehaviorReadinessEntry {
            behavior_id: crate::support::AGENT_NAME.to_string(),
            state: if behavior.is_some() {
                BehaviorReadinessState::Unavailable
            } else {
                BehaviorReadinessState::Ready
            },
            reason: behavior,
        }],
    };
    let did = gents::graphql::escape_graphql_string(db.node_identity.did());
    let snapshot_json =
        gents::graphql::escape_graphql_string(&serde_json::to_string(&snapshot).unwrap());
    let updated_at = chrono::Utc::now().to_rfc3339();
    let response = db
        .node
        .execute(&format!(
            r#"mutation {{ upsert_AgentBehaviorReadiness(
                filter: {{ agent_did: {{ _eq: "{did}" }} }},
                add: {{ agent_did: "{did}", snapshot_json: "{snapshot_json}", updated_at: "{updated_at}" }},
                update: {{ snapshot_json: "{snapshot_json}", updated_at: "{updated_at}" }}
            ) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "publish behavior readiness: {:?}",
        response.errors
    );
}

async fn publish_reconcile_phase(db: &TestDb, phase: &str) {
    let did = gents::graphql::escape_graphql_string(db.node_identity.did());
    let response = db
        .node
        .execute(&format!(
            r#"mutation {{ upsert_AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{did}" }} }},
                add: {{ agent_did: "{did}", reconcile_phase: "{phase}", last_reconcile_result: "applied" }},
                update: {{ reconcile_phase: "{phase}", last_reconcile_result: "applied" }}
            ) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "publish reconcile phase: {:?}",
        response.errors
    );
}

async fn source(db: &TestDb) -> (GoalSource, watch::Sender<Arc<ActiveRuntimeSnapshot>>) {
    publish_behavior_readiness(
        db,
        gents_protocol::row::BehaviorReadinessProcessState::Ready,
        None,
    )
    .await;
    unready_source(db)
}

fn unready_source(db: &TestDb) -> (GoalSource, watch::Sender<Arc<ActiveRuntimeSnapshot>>) {
    let (tx, rx) = watch::channel(snapshot(db.node_identity.did()));
    let subscriptions: Arc<dyn UpdateSubscriptionSource> =
        Arc::new(MockUpdateSubscriptionSource::new());
    (
        GoalSource::with_subscription_source(
            subscriptions,
            rx,
            db.node.clone(),
            CancellationToken::new(),
        )
        .with_rescan_interval(RESCAN),
        tx,
    )
}

async fn seed_completed_request(db: &TestDb, request_id: &str) -> String {
    let doc_id = create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        request_id,
        SESSION,
        "completed",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    publish_canonical_terminal_activity(db, &doc_id, "durable progress").await;
    doc_id
}

async fn seed_failed_request(db: &TestDb, request_id: &str) -> String {
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        request_id,
        SESSION,
        "failed",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await
}

async fn boot_goal_background_handoff(
    db: &TestDb,
    request_id: &str,
    entered_path: &std::path::Path,
    release_path: &std::path::Path,
) -> (crate::support::accepted_turn::AcceptedTurnRuntime, String) {
    boot_goal_background_handoff_with_plans(db, request_id, entered_path, release_path, Vec::new())
        .await
}

async fn boot_goal_background_handoff_with_plans(
    db: &TestDb,
    request_id: &str,
    entered_path: &std::path::Path,
    release_path: &std::path::Path,
    child_plans: Vec<crate::support::streaming_backend::StreamPlan>,
) -> (crate::support::accepted_turn::AcceptedTurnRuntime, String) {
    let did = db.node_identity.did();
    let behavior = "goal-background-handoff";
    let prompt = "start late background handoff";
    let prepared = crate::support::accepted_turn::prepare_accepted_turn(
        db,
        crate::support::accepted_turn::AcceptedTurnSpec {
            backend_id: "goal-background-backend",
            model: "test-model",
            parent_behavior_id: behavior,
            configured_behavior_ids: &[behavior],
            request_id,
            session_id: SESSION,
            prompt,
            accepted_chunks: vec![crate::support::streaming_backend::StreamChunk::tool_call(
                "goal-background-spawn",
                "spawn_process",
                serde_json::json!({
                    "tool_name": "bash",
                    "args": {
                        "command": "sh",
                        "args": [
                            "-c",
                            ": > \"$1\"; n=0; while [ ! -f \"$2\" ] && [ \"$n\" -lt 3000 ]; do sleep 0.02; n=$((n + 1)); done; [ -f \"$2\" ] || exit 124; printf 'late result required for final wrapup'",
                            "goal-background-handoff",
                            entered_path.to_string_lossy(),
                            release_path.to_string_lossy(),
                        ]
                    }
                })
                .to_string(),
            )],
            child_plans,
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    prepared.backend.enable_dynamic_followups(prompt);
    crate::support::fixtures::configure_behavior_tools(
        db.node.as_ref(),
        did,
        behavior,
        None,
        gents::document_config::Tools {
            tools_id: format!("{behavior}:tools"),
            agent_did: did.to_string(),
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
    .unwrap();
    let runtime =
        crate::support::accepted_turn::boot_prepared_accepted_turn(db, prepared, agent).await;
    let parent_doc = crate::support::exact_request_doc_id(db.node.as_ref(), request_id).await;
    for _ in 0..200 {
        let response = db.node.execute(&format!(r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "bash" }}, lifecycle_state: {{ _eq: "running" }} }}, limit: 1) {{ _docID }} }}"#, gents::graphql::escape_graphql_string(&parent_doc))).await;
        if response
            .data
            .as_ref()
            .and_then(|data| data["AgentToolCall"].as_array())
            .is_some_and(|rows| !rows.is_empty())
        {
            if entered_path.exists() {
                return (runtime, parent_doc);
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("accepted goal background handoff did not start")
}

#[tokio::test]
async fn real_waited_process_defers_goal_until_completion_wake_becomes_parent() {
    use crate::support::streaming_backend::{
        StreamChunk, StreamPlan, StreamResponse, StreamScript,
    };
    use gents::llm::message::{Message, Text, ToolResultContent, UserContent};

    let db = test_db("goal-real-process-wait-wake").await;
    let parent_request_id = "goal-real-process-parent";
    let objective = "goal-real-process-wake-1973";
    struct ReleaseOnDrop {
        directory: Option<tempfile::TempDir>,
        release_path: std::path::PathBuf,
        settled: bool,
    }
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            if !self.settled {
                let _ = std::fs::write(&self.release_path, b"release");
                if let Some(directory) = self.directory.take() {
                    let _ = directory.keep();
                }
            }
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let mut barrier = ReleaseOnDrop {
        release_path: directory.path().join("release"),
        directory: Some(directory),
        settled: false,
    };
    let entered_path = barrier.directory.as_ref().unwrap().path().join("entered");
    let release_path = barrier.release_path.clone();
    let hold_goal_child = StreamPlan::current_authored_user(
        objective,
        vec![StreamResponse::Stream(StreamScript::paused_before(
            objective,
            vec![StreamChunk::text("goal child reached provider")],
        ))],
    );
    let (runtime, parent_doc_id) = boot_goal_background_handoff_with_plans(
        &db,
        parent_request_id,
        &entered_path,
        &release_path,
        vec![hold_goal_child],
    )
    .await;
    assert!(
        entered_path.exists(),
        "real child did not enter its held process body"
    );
    let process = db.node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "bash" }} }}, limit: 1) {{ _docID tool_call_id lifecycle_state await_mode }} }}"#,
        gents::graphql::escape_graphql_string(&parent_doc_id)
    )).await;
    assert!(!process.has_errors(), "process query: {:?}", process.errors);
    let process = &process.data.as_ref().unwrap()["AgentToolCall"][0];
    let process_doc_id = process["_docID"].as_str().unwrap().to_owned();
    let process_handle = process["tool_call_id"].as_str().unwrap().to_owned();
    assert_eq!(process["lifecycle_state"], "running");
    assert_eq!(process["await_mode"], "background");

    let prompt = "start late background handoff";
    runtime.backend.enqueue_response(
        prompt,
        StreamResponse::streams(
            prompt,
            vec![StreamChunk::tool_call(
                "goal-background-wait",
                "wait_process",
                serde_json::json!({ "tool_call_id": process_handle, "timeout_secs": 1 })
                    .to_string(),
            )],
        ),
    );
    runtime.backend.enqueue_response(
        prompt,
        StreamResponse::completes(prompt, ["parent completed after bounded wait"]),
    );
    assert_eq!(
        crate::support::live_inference::wait_for_request_terminal(
            db.node.as_ref(),
            parent_request_id,
            Duration::from_secs(15),
        )
        .await,
        "completed"
    );
    let wait_result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let history = gents::load_history(
                db.node.as_ref(),
                SESSION,
                db.node_identity.did(),
                Some(db.node_identity.did()),
            )
            .await
            .unwrap();
            if let Some(result) = history.iter().find_map(|message| match message {
                Message::User { content } => content.iter().find_map(|part| match part {
                    UserContent::ToolResult(result)
                        if result.id == "goal-background-wait"
                            || result.call_id.as_deref() == Some("goal-background-wait") =>
                    {
                        result.content.iter().find_map(|part| match part {
                            ToolResultContent::Text(Text { text }) => {
                                serde_json::from_str::<serde_json::Value>(text).ok()
                            }
                            _ => None,
                        })
                    }
                    _ => None,
                }),
                _ => None,
            }) {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("accepted canonical wait reply");
    assert_eq!(wait_result["tool_call_id"], process_handle);
    assert_eq!(wait_result["status"], "running");
    assert_eq!(wait_result["error"]["reason"], "wait_timeout");
    assert!(!release_path.exists());

    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some(objective),
        Some(GoalStatus::Active),
        Some(Some(10_000)),
    )
    .await
    .unwrap();
    let claimed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
                .await
                .unwrap()
                .unwrap();
            if goal.last_continued_from_request_id.as_deref() == Some(parent_request_id) {
                break goal;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("GoalSource did not claim the terminal parent");
    assert_eq!(claimed.continuation_sequence(), 1);
    assert_eq!(claimed.infrastructure_retry_count.unwrap_or_default(), 0);
    let held_until = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        assert!(entered_path.exists() && !release_path.exists());
        let process = db.node.execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state }} }}"#,
            gents::graphql::escape_graphql_string(&process_doc_id)
        )).await;
        assert!(
            !process.has_errors(),
            "held process query: {:?}",
            process.errors
        );
        assert_eq!(
            process.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "running"
        );
        assert!(
            goal_children(&db).await.is_empty(),
            "held process must defer the claimed Goal child"
        );
        let still_claimed = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            still_claimed.continuation_sequence,
            claimed.continuation_sequence
        );
        assert_eq!(
            still_claimed.last_continued_from_request_id,
            claimed.last_continued_from_request_id
        );
        assert_eq!(
            still_claimed.infrastructure_retry_count,
            claimed.infrastructure_retry_count
        );
        if tokio::time::Instant::now() >= held_until {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    std::fs::write(&release_path, b"release").unwrap();
    let notification_key = format!("background-completion-notification:{process_doc_id}:tool");
    let notification = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = db.node.execute(&format!(
                r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}, limit: 2) {{ _docID message_key agent_did requester_did request_doc_id }} }}"#,
                gents::graphql::escape_graphql_string(&notification_key)
            )).await;
            assert!(!response.has_errors(), "notification query: {:?}", response.errors);
            let rows = response.data.as_ref().unwrap()["AgentMessage"].as_array().unwrap();
            assert!(rows.len() <= 1, "ambiguous process completion notification");
            if let Some(row) = rows.first() {
                break row.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("background worker did not publish its exact notification");
    let wake_doc_id = notification["request_doc_id"]
        .as_str()
        .expect("notification wake binding");
    let (header, reconstructed) = gents::session::load_canonical_message_from_node(
        db.node.as_ref(),
        notification["_docID"].as_str().unwrap(),
        notification["agent_did"].as_str().unwrap(),
        notification["requester_did"].as_str(),
    )
    .await
    .unwrap();
    assert_eq!(header.message_key, notification_key);
    assert_eq!(header.request_doc_id.as_deref(), Some(wake_doc_id));
    assert!(matches!(header.publication,
        gents_protocol::output::MessagePublication::ToolDelivery { ref tool_call_doc_id }
            if tool_call_doc_id == &process_doc_id));
    let Message::User { content } = reconstructed else {
        panic!("process completion must be canonical user input");
    };
    let notification_text = content
        .into_iter()
        .filter_map(|part| match part {
            UserContent::Text(Text { text }) => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        notification_text.contains("tool-completion")
            && notification_text.contains(&process_handle)
    );
    let wake_response = db.node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_id lifecycle_state execution_origin }} }}"#,
        gents::graphql::escape_graphql_string(wake_doc_id)
    )).await;
    assert!(
        !wake_response.has_errors(),
        "bound wake query: {:?}",
        wake_response.errors
    );
    let wakes = wake_response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(wakes.len(), 1, "the exact notification must bind one wake");
    let wake = &wakes[0];
    let wake_id = wake["request_id"].as_str().unwrap();
    assert!(wake_id.starts_with("background-completion-"));
    assert_eq!(wake["execution_origin"], "scheduled");
    assert_eq!(
        crate::support::live_inference::wait_for_request_terminal(
            db.node.as_ref(),
            wake_id,
            Duration::from_secs(15)
        )
        .await,
        "completed"
    );
    let child = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let children = goal_children(&db).await;
            if children.len() == 1 {
                break children.into_iter().next().unwrap();
            }
            assert!(
                children.is_empty(),
                "GoalSource published multiple children"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("GoalSource did not continue from the terminal wake");
    assert_eq!(child.caused_by_parent_request_id.as_deref(), Some(wake_id));
    assert_eq!(
        child.caused_by_parent_request_doc_id.as_deref(),
        Some(wake_doc_id)
    );
    assert_ne!(
        child.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        goal.last_continued_from_request_id.as_deref(),
        Some(wake_id)
    );
    assert_eq!(
        goal.continuation_sequence(),
        claimed.continuation_sequence() + 1
    );
    assert_eq!(goal.infrastructure_retry_count.unwrap_or_default(), 0);
    let process = db.node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state completion_notification_delivered_at }} }}"#,
        gents::graphql::escape_graphql_string(&process_handle)
    )).await;
    assert!(
        !process.has_errors(),
        "completed process query: {:?}",
        process.errors
    );
    let process = &process.data.as_ref().unwrap()["AgentToolCall"][0];
    assert_eq!(process["lifecycle_state"], "completed");
    assert!(process["completion_notification_delivered_at"].is_string());
    barrier.settled = true;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].doc_id, child.doc_id);
    runtime.shutdown().await;
}

#[derive(Debug, Deserialize)]
struct ChildRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    behavior_id: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    input: Option<serde_json::Value>,
    lifecycle_state: Option<String>,
    retry_key: Option<String>,
    subagent_depth: Option<i64>,
    workspace_id: Option<String>,
    workspace_authority: Option<String>,
    workspace_owner_agent_did: Option<String>,
    workspace_seal_hash: Option<String>,
}

async fn goal_children(db: &TestDb) -> Vec<ChildRow> {
    let response = db
        .node
        .execute(
            r#"{
                AgentRequest(filter: { caused_by_trigger_kind: { _eq: "goal" } }) {
                    _docID request_id agent_did requester_did session_id behavior_id caused_by_parent_request_id caused_by_parent_request_doc_id
                    caused_by_trigger_id caused_by_trigger_kind input lifecycle_state
                    retry_key subagent_depth workspace_id workspace_authority
                    workspace_owner_agent_did workspace_seal_hash
                }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "query children: {:?}",
        response.errors
    );
    serde_json::from_value(
        response
            .data
            .and_then(|data| data.get("AgentRequest").cloned())
            .unwrap_or_default(),
    )
    .expect("decode goal children")
}

#[tokio::test]
async fn goal_continuation_preserves_nested_workspace_lineage() {
    let db = test_db("goal-nested-workspace-lineage").await;
    let did = db.node_identity.did();
    let admission = gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did);
    let mut parent = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "parent-nested-workspace",
        did,
        did,
        crate::support::AGENT_NAME,
        SESSION,
        "nested durable work",
        "interactive",
        "2026-07-15T00:00:00Z",
        admission,
    );
    parent.subagent_depth = 2;
    parent.caused_by_parent_request_id = Some("grandparent-request".to_string());
    parent.caused_by_parent_request_doc_id = Some("grandparent-request-doc".to_string());
    parent.caused_by_parent_tool_call_id = Some("grandparent-tool-call".to_string());
    parent.caused_by_parent_tool_call_doc_id = Some("grandparent-tool-call-doc".to_string());
    parent.workspace_id = Some("workspace-goal".to_string());
    parent.workspace_authority = Some("readOnly".to_string());
    parent.workspace_owner_agent_did = Some(did.to_string());
    parent.workspace_seal_hash = Some("seal-hash".to_string());
    gents::sign_agent_request_create_as_registered_target(&mut parent)
        .await
        .expect("sign nested parent");
    let response = db
        .node
        .execute(&parent.graphql_mutation().expect("parent mutation"))
        .await;
    assert!(
        !response.has_errors(),
        "create nested parent: {:?}",
        response.errors
    );
    let lookup = db
        .node
        .execute(
            r#"{ AgentRequest(filter: { request_id: { _eq: "parent-nested-workspace" } }) { _docID } }"#,
        )
        .await;
    assert!(
        !lookup.has_errors(),
        "lookup nested parent: {:?}",
        lookup.errors
    );
    let parent_doc_id = lookup.data.as_ref().unwrap()["AgentRequest"][0]["_docID"]
        .as_str()
        .expect("parent doc id")
        .to_string();
    set_request_lifecycle_state(db.node.as_ref(), &parent_doc_id, "completed").await;
    publish_canonical_terminal_activity(&db, &parent_doc_id, "durable progress").await;
    set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Preserve nested workspace scope"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _tx) = source(&db).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("lineage continuation timed out")
        .expect("lineage continuation intent");
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    let child = &children[0];
    assert_eq!(child.subagent_depth, Some(2));
    assert_eq!(child.workspace_id.as_deref(), Some("workspace-goal"));
    assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
    assert_eq!(child.workspace_owner_agent_did.as_deref(), Some(did));
    assert_eq!(child.workspace_seal_hash.as_deref(), Some("seal-hash"));
    let child_identity = (
        child.doc_id.clone(),
        child.request_id.clone(),
        child.retry_key.clone(),
    );
    let continuation_sequence = load_canonical_goal(db.node.as_ref(), did, SESSION)
        .await
        .unwrap()
        .unwrap()
        .continuation_sequence;
    drop(source);
    for _ in 0..2 {
        let recovered = gents::RequestLifecycle::recover_all(db.node.as_ref(), did)
            .await
            .unwrap();
        assert_eq!(recovered.requests_recovered, 0);
        let (mut restarted, _restart_tx) = self::source(&db).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
                .await
                .is_err(),
            "repeated maintenance and source reconciliation must not emit another continuation"
        );
        let children = goal_children(&db).await;
        assert_eq!(children.len(), 1);
        let child = &children[0];
        assert_eq!(
            (
                child.doc_id.clone(),
                child.request_id.clone(),
                child.retry_key.clone()
            ),
            child_identity
        );
        assert_eq!(child.subagent_depth, Some(2));
        assert_eq!(child.workspace_id.as_deref(), Some("workspace-goal"));
        assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
        assert_eq!(child.workspace_owner_agent_did.as_deref(), Some(did));
        assert_eq!(child.workspace_seal_hash.as_deref(), Some("seal-hash"));
        assert_eq!(
            load_canonical_goal(db.node.as_ref(), did, SESSION)
                .await
                .unwrap()
                .unwrap()
                .continuation_sequence,
            continuation_sequence
        );
    }
}

#[tokio::test]
async fn delimiter_ambiguous_goal_parent_pairs_get_distinct_retry_keys() {
    let db = test_db("goal-delimiter-safe-key").await;
    let did = db.node_identity.did();
    for (session, parent) in [("b:c", "d"), ("b", "c:d")] {
        let doc_id = create_request_for_agent_with_signed_fields(
            db.node.as_ref(),
            did,
            parent,
            session,
            "completed",
            "2026-07-15T00:00:00Z",
            None,
            None,
            None,
            None,
        )
        .await;
        publish_canonical_terminal_activity(&db, &doc_id, "progress").await;
        assert!(!doc_id.is_empty());
        set_goal(
            db.node.as_ref(),
            did,
            session,
            Some(&format!("goal for {session}")),
            Some(GoalStatus::Active),
            None,
        )
        .await
        .expect("set delimiter goal");
    }

    let (mut source, _tx) = source(&db).await;
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .expect("delimiter continuation timed out")
            .expect("delimiter continuation intent");
    }
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 2);
    let keys = children
        .iter()
        .map(|child| child.retry_key.as_deref().expect("retry key"))
        .collect::<HashSet<_>>();
    assert_eq!(keys.len(), 2);
}

#[tokio::test]
async fn completed_request_materializes_exactly_one_same_session_goal_child() {
    let db = test_db("goal-exactly-once").await;
    seed_completed_request(&db, "parent-complete").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Finish the durable objective"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db).await;
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out")
        .expect("goal continuation intent");
    assert!(intent.pre_materialized_request_id.is_some());

    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    let child = &children[0];
    assert_eq!(child.session_id, SESSION);
    assert_eq!(
        child.behavior_id.as_deref(),
        Some(crate::support::AGENT_NAME)
    );
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some("parent-complete")
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(goal.goal_id.as_str())
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("goal"));

    assert!(
        tokio::time::timeout(Duration::from_millis(150), source.next_fire())
            .await
            .is_err(),
        "a pending child must suppress duplicate continuation"
    );
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn foreign_request_id_collision_cannot_preempt_owned_goal_continuation() {
    use sha2::{Digest, Sha256};

    let db = test_db("goal-foreign-request-id-collision").await;
    seed_completed_request(&db, "parent-owned-collision").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Ignore foreign request IDs"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    let digest =
        Sha256::digest(format!("{}\0{}", goal.goal_id, "parent-owned-collision").as_bytes());
    let collision_id = format!(
        "goal-cont-{:020}-{}",
        1,
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        "did:key:foreign",
        &collision_id,
        "foreign-session",
        "pending",
        "2026-07-15T00:00:01Z",
        None,
        None,
        None,
        None,
    )
    .await;

    let (mut source, _tx) = source(&db).await;
    let fired = tokio::time::timeout(Duration::from_secs(2), source.next_fire()).await;
    if !matches!(fired, Ok(Some(_))) {
        let state = db
            .node
            .execute("{ Goal { goal_id status last_continued_from_request_id continuation_sequence } AgentRequest { request_id agent_did session_id retry_key caused_by_trigger_kind } }")
            .await;
        let outcome = match fired {
            Err(_) => "timeout",
            Ok(None) => "source ended",
            Ok(Some(_)) => unreachable!(),
        };
        panic!("owned continuation did not fire ({outcome}); state={state:?}");
    }
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].request_id, collision_id);
    assert_eq!(children[0].session_id, SESSION);
}

#[tokio::test]
async fn foreign_retry_key_collision_is_rejected_instead_of_reused() {
    use sha2::{Digest, Sha256};

    let db = test_db("goal-foreign-retry-key-collision").await;
    seed_completed_request(&db, "parent-owned-retry-collision").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Reject foreign retry keys"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    let digest =
        Sha256::digest(format!("{}\0{}", goal.goal_id, "parent-owned-retry-collision").as_bytes());
    let retry_key = format!(
        "goal-continuation:{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{
            request_id: "foreign-retry-collision", purpose: "normal", agent_did: "did:key:foreign",
            behavior_id: "foreign", session_id: "foreign-session",
            retry_root_request: "foreign-retry-collision",
            retry_key: "{}",
            content: "foreign", lifecycle_state: "completed",
            execution_origin: "interactive", created_at: "2026-07-15T00:00:01Z",
            retry_count: 0, max_retries: 3
        }}) {{ _docID }} }}"#,
        retry_key
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed collision: {:?}",
        response.errors
    );

    let (mut source, _tx) = source(&db).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err(),
        "a conflicting retry key must not be reported as an owned continuation"
    );
    assert!(goal_children(&db).await.is_empty());
}

#[tokio::test]
async fn restart_reconcile_does_not_duplicate_a_goal_continuation() {
    let db = test_db("goal-restart-no-duplicate").await;
    seed_completed_request(&db, "parent-restart").await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Continue exactly once across restart"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut first, _first_tx) = source(&db).await;
    tokio::time::timeout(Duration::from_secs(2), first.next_fire())
        .await
        .expect("first source timed out")
        .expect("first continuation");
    drop(first);

    let (mut restarted, _restart_tx) = source(&db).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
            .await
            .is_err(),
        "restart must not emit a second child"
    );
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn restart_materializes_a_claimed_continuation_after_crash() {
    let db = test_db("goal-restart-claimed").await;
    seed_completed_request(&db, "parent-claimed").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Recover the claimed continuation"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    assert!(
        claim_continuation(db.node.as_ref(), &goal, "parent-claimed")
            .await
            .expect("claim continuation")
    );
    assert!(goal_children(&db).await.is_empty());

    let (mut restarted, _restart_tx) = source(&db).await;
    tokio::time::timeout(Duration::from_secs(2), restarted.next_fire())
        .await
        .expect("recovery source timed out")
        .expect("recovered continuation");
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn restart_materializes_claimed_retry_without_charging_it_twice() {
    let db = test_db("goal-restart-claimed-retry").await;
    seed_failed_request(&db, "parent-claimed-retry").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Recover one charged retry"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    assert!(
        claim_retry_continuation(db.node.as_ref(), &goal, "parent-claimed-retry", 1, "failed",)
            .await
            .expect("atomically charge and claim retry")
    );

    let (mut restarted, _tx) = source(&db).await;
    tokio::time::timeout(Duration::from_secs(2), restarted.next_fire())
        .await
        .expect("retry recovery timed out")
        .expect("retry recovery intent");
    let recovered = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load recovered goal")
        .expect("goal exists");
    assert_eq!(recovered.infrastructure_retry_count, Some(1));
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn restart_materializes_claimed_wrapup_retry_without_charging_it_twice() {
    let db = test_db("goal-restart-claimed-wrapup-retry").await;
    seed_failed_request(&db, "parent-claimed-wrapup-retry").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Recover one charged wrap-up retry"),
        Some(GoalStatus::BudgetLimited),
        None,
    )
    .await
    .expect("set budget-limited goal");
    assert!(claim_retry_continuation(
        db.node.as_ref(),
        &goal,
        "parent-claimed-wrapup-retry",
        1,
        "failed",
    )
    .await
    .expect("atomically charge and claim wrap-up retry"));

    let (mut restarted, _tx) = source(&db).await;
    tokio::time::timeout(Duration::from_secs(2), restarted.next_fire())
        .await
        .expect("wrap-up retry recovery timed out")
        .expect("wrap-up retry recovery intent");
    let recovered = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load recovered goal")
        .expect("goal exists");
    assert_eq!(recovered.infrastructure_retry_count, Some(1));
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    let input = children[0].input.as_ref().expect("goal continuation input");
    assert_eq!(input["goal_continuation"]["wrapup"], true);
}

#[tokio::test]
async fn continuation_claim_rejects_a_stale_active_status_snapshot() {
    let db = test_db("goal-claim-status-cas").await;
    let stale = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Do not race an operator pause"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    seed_goal_fields(db.node.as_ref(), &stale, r#"status: "paused""#)
        .await
        .expect("pause goal");
    assert!(
        !claim_continuation(db.node.as_ref(), &stale, "parent-stale")
            .await
            .expect("stale claim returns cleanly")
    );
}

#[tokio::test]
async fn controller_status_cas_never_overwrites_a_newer_completion() {
    let db = test_db("goal-controller-status-cas").await;
    let stale = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Completion wins controller races"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    seed_goal_fields(db.node.as_ref(), &stale, r#"status: "complete""#)
        .await
        .expect("complete goal");
    assert!(!update_goal_fields_if_status(
        db.node.as_ref(),
        &stale,
        GoalStatus::Active,
        r#"status: "paused", last_failure: "stale controller""#,
    )
    .await
    .expect("conditional controller update"));
    let current = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(current.parsed_status(), Some(GoalStatus::Complete));
}

async fn assert_stale_controller_cannot_mutate_new_continuation(status: GoalStatus) {
    let db = test_db("goal-controller-continuation-cas").await;
    let stale = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Preserve the newer continuation from stale controller decisions"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set active goal");
    assert_eq!(stale.continuation_sequence(), 0);
    assert!(
        claim_continuation(db.node.as_ref(), &stale, "newly-claimed-parent")
            .await
            .expect("claim newer continuation")
    );
    let claimed = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load claimed goal")
        .expect("goal exists");
    assert_eq!(claimed.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(claimed.continuation_sequence(), 1);
    assert_eq!(
        claimed.last_continued_from_request_id.as_deref(),
        Some("newly-claimed-parent")
    );
    let fields = format!(
        r#"status: "{}", last_failure: "stale controller observation""#,
        status.as_str()
    );
    assert!(
        !update_goal_fields_if_status(db.node.as_ref(), &stale, GoalStatus::Active, &fields,)
            .await
            .expect("stale controller mutation returns cleanly"),
        "same status must not authorize an older continuation snapshot"
    );
    let current = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("reload goal after rejected stale write")
        .expect("goal exists");
    assert_eq!(current.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(
        current.continuation_sequence(),
        claimed.continuation_sequence()
    );
    assert_eq!(
        current.last_continued_from_request_id,
        claimed.last_continued_from_request_id
    );
    assert_eq!(current.last_failure, claimed.last_failure);
    // A current observation must still authorize the legitimate transition.
    assert!(
        update_goal_fields_if_status(db.node.as_ref(), &current, GoalStatus::Active, &fields,)
            .await
            .expect("current controller mutation")
    );
    let updated = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("reload current update")
        .expect("goal exists");
    assert_eq!(updated.parsed_status(), Some(status));
    assert_eq!(updated.continuation_sequence(), 1);
}

#[tokio::test]
async fn controller_pause_rejects_same_status_from_an_older_continuation() {
    assert_stale_controller_cannot_mutate_new_continuation(GoalStatus::Paused).await;
}

#[tokio::test]
async fn controller_block_rejects_same_status_from_an_older_continuation() {
    assert_stale_controller_cannot_mutate_new_continuation(GoalStatus::Blocked).await;
}

#[tokio::test]
async fn model_goal_create_is_idempotent_and_never_mutates_a_conflict() {
    let db = test_db("goal-model-create").await;
    let created = create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        "Ship the feature",
        Some(1_000),
    )
    .await
    .expect("create model goal");
    assert!(matches!(created, CreateGoalForSessionOutcome::Created(_)));

    let duplicate = create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        " Ship the feature ",
        Some(1_000),
    )
    .await
    .expect("idempotent model goal retry");
    assert!(matches!(
        duplicate,
        CreateGoalForSessionOutcome::Idempotent(_)
    ));
    assert_eq!(
        load_goals_for_session(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("load model goals")
            .len(),
        1
    );

    let error = create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        "Different objective",
        Some(1_000),
    )
    .await
    .expect_err("conflicting create must fail");
    assert!(matches!(error, CreateGoalForSessionError::Conflict));
    let unchanged = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load unchanged goal")
        .expect("goal remains");
    assert_eq!(unchanged.objective, "Ship the feature");
    assert_eq!(unchanged.token_budget, Some(1_000));
}

#[tokio::test]
async fn clearing_a_goal_also_clears_its_creation_claim() {
    let db = test_db("goal-model-create-clear").await;
    create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        "First objective",
        None,
    )
    .await
    .expect("create first goal");
    assert_eq!(
        delete_goals_for_session(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("clear goal and claim"),
        1
    );
    let replacement = create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        "Replacement objective",
        None,
    )
    .await
    .expect("create replacement goal");
    assert!(matches!(
        replacement,
        CreateGoalForSessionOutcome::Created(_)
    ));
}

async fn signed_goal_backed_request(
    db: &TestDb,
    request_id: &str,
) -> gents_protocol::request_admission::AgentRequestCreate {
    let did = db.node_identity.did();
    let admission = gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        did,
        did,
        crate::support::AGENT_NAME,
        SESSION,
        "Begin durable work",
        "interactive",
        "2026-07-15T00:00:00Z",
        admission,
    );
    create.retry_key = Some(format!("goal-submit:{SESSION}"));
    gents::sign_agent_request_create_as_registered_target(&mut create)
        .await
        .expect("sign goal-backed request");
    create
}

#[tokio::test]
async fn goal_backed_submission_commits_goal_before_request_visibility_and_retries_idempotently() {
    let db = test_db("goal-backed-submit").await;
    let access = ConfigAccess::Local(db.node.clone());
    let create = signed_goal_backed_request(&db, "goal-backed-first").await;
    let first = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("atomic goal submission");
    assert_eq!(first, gents::goal::GoalBackedRequestDisposition::Created);

    let response = db
        .node
        .execute(r#"{ Goal { goal_id } AgentRequest(filter: { retry_key: { _eq: "goal-submit:goal-session" } }) { request_id lifecycle_state } }"#)
        .await;
    assert!(
        !response.has_errors(),
        "load atomic pair: {:?}",
        response.errors
    );
    assert_eq!(
        response.data.as_ref().unwrap()["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let requests = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["lifecycle_state"], "pending");

    let retry = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("idempotent atomic retry");
    assert_eq!(retry, gents::goal::GoalBackedRequestDisposition::Idempotent);

    let committed_goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load committed goal")
        .expect("goal exists");
    seed_goal_fields(db.node.as_ref(), &committed_goal, r#"status: "complete""#)
        .await
        .expect("simulate progress after an ambiguous commit acknowledgement");
    let late_retry = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("exact retry remains idempotent after goal progress");
    assert_eq!(
        late_retry,
        gents::goal::GoalBackedRequestDisposition::Idempotent
    );
}

#[tokio::test]
async fn goal_backed_retry_rejects_changed_logical_request_fields() {
    let db = test_db("goal-backed-submit-fingerprint-conflict").await;
    let access = ConfigAccess::Local(db.node.clone());
    let create = signed_goal_backed_request(&db, "goal-backed-fingerprint").await;
    gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship one exact request",
        None,
        &create,
    )
    .await
    .expect("initial goal submission");

    let mut changed = create.clone();
    changed.input = serde_json::from_value(serde_json::json!({
        "queue": {"source": "goal", "policy": "coalesce", "key": "changed"}
    }))
    .unwrap();
    gents::sign_agent_request_create_as_registered_target(&mut changed)
        .await
        .expect("re-sign changed request");
    let error = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship one exact request",
        None,
        &changed,
    )
    .await
    .expect_err("changed retry semantics must conflict");
    assert!(error
        .to_string()
        .contains("different immutable request fields"));
}

#[tokio::test]
async fn goal_backed_submission_rejects_a_matching_terminal_goal_without_publishing() {
    let db = test_db("goal-backed-submit-terminal-goal").await;
    let access = ConfigAccess::Local(db.node.clone());
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Already terminal"),
        Some(GoalStatus::Complete),
        None,
    )
    .await
    .expect("set terminal goal");
    let create = signed_goal_backed_request(&db, "goal-backed-terminal").await;
    let error = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Already terminal",
        None,
        &create,
    )
    .await
    .expect_err("terminal goal must reject a new first request");
    assert!(error.to_string().contains("non-active goal"));
    let response = db
        .node
        .execute("{ AgentRequest { request_id } GoalCreationClaim { creation_key } }")
        .await;
    assert!(
        !response.has_errors(),
        "query terminal rollback: {:?}",
        response.errors
    );
    assert!(response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(response.data.as_ref().unwrap()["GoalCreationClaim"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn goal_backed_submission_discards_goal_when_request_staging_fails() {
    let db = test_db("goal-backed-submit-rollback").await;
    let access = ConfigAccess::Local(db.node.clone());
    let mut invalid = signed_goal_backed_request(&db, "goal-backed-invalid").await;
    invalid.initial_lifecycle_state =
        gents_protocol::client_protocol::RequestLifecycleState::Processing;
    let error = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Never publish partially",
        None,
        &invalid,
    )
    .await
    .expect_err("invalid request must abort transaction");
    assert!(error.to_string().contains("pending state"));
    assert!(
        load_goals_for_session(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("load rolled-back goals")
            .is_empty()
    );
    let response = db
        .node
        .execute("{ GoalCreationClaim { creation_key } AgentRequest { request_id } }")
        .await;
    assert!(
        !response.has_errors(),
        "load rollback state: {:?}",
        response.errors
    );
    assert!(response.data.as_ref().unwrap()["GoalCreationClaim"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn concurrent_model_goal_create_converges_to_one_physical_goal() {
    let db = test_db("goal-model-create-concurrent").await;
    let left_node = db.node.clone();
    let right_node = db.node.clone();
    let did = db.node_identity.did().to_string();
    let left_did = did.clone();
    let left = tokio::spawn(async move {
        create_goal_for_session(
            left_node.as_ref(),
            &left_did,
            SESSION,
            "Concurrent objective",
            None,
        )
        .await
    });
    let right = tokio::spawn(async move {
        create_goal_for_session(
            right_node.as_ref(),
            &did,
            SESSION,
            "Concurrent objective",
            None,
        )
        .await
    });
    left.await.expect("left task").expect("left create");
    right.await.expect("right task").expect("right create");
    assert_eq!(
        load_goals_for_session(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("load converged goals")
            .len(),
        1
    );
}

#[tokio::test]
async fn any_newer_active_request_blocks_goal_continuation_for_the_whole_session() {
    let db = test_db("goal-session-idle").await;
    seed_completed_request(&db, "older-complete").await;
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        "newer-manual",
        SESSION,
        "pending",
        "2026-07-15T00:01:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Wait for session idleness"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), source.next_fire())
            .await
            .is_err()
    );
    assert!(goal_children(&db).await.is_empty());
}

#[tokio::test]
async fn interrupted_terminal_pauses_instead_of_self_continuing() {
    let db = test_db("goal-interrupted").await;
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        "parent-interrupted",
        SESSION,
        "interrupted",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Respect human interruption"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Paused));
    assert!(goal_children(&db).await.is_empty());
}

#[tokio::test]
async fn exhausted_budget_after_failed_or_dead_request_materializes_wrapup_not_retry() {
    for terminal in ["failed", "dead"] {
        let db = test_db(&format!("goal-budget-after-{terminal}")).await;
        let parent = "parent-over-budget";
        let barrier = tempfile::tempdir().unwrap();
        let entered_path = barrier.path().join("entered");
        let release_path = barrier.path().join("release");
        let (runtime, parent_doc) =
            boot_goal_background_handoff(&db, parent, &entered_path, &release_path).await;
        set_request_lifecycle_state(db.node.as_ref(), &parent_doc, terminal).await;
        // The accepted spawn has real provider usage before the failed call
        // below. Retain that usage instead of assuming an empty call history.
        let prior_usage = session_token_usage(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap();
        let expected_usage = prior_usage + 105;
        let usage = format!(
            r#"mutation {{ add_InferenceCall(input: {{
                call_id: "over-budget-failed-call",
                request_id: "{parent}",
                agent_did: "{}",
                call_seq: 1,
                attempt: 1,
                call_state: "failed",
                failure_reason: "provider stream interrupted after reporting usage",
                prompt_tokens: 100,
                completion_tokens: 5
            }}) {{ _docID }} }}"#,
            gents::graphql::escape_graphql_string(db.node_identity.did()),
        );
        let response = db.node.execute(&usage).await;
        assert!(!response.has_errors(), "seed usage: {:?}", response.errors);
        assert_eq!(
            session_token_usage(db.node.as_ref(), db.node_identity.did(), SESSION)
                .await
                .unwrap(),
            expected_usage,
        );
        set_goal(
            db.node.as_ref(),
            db.node_identity.did(),
            SESSION,
            Some("Respect the budget even when execution fails"),
            Some(GoalStatus::Active),
            Some(Some(10)),
        )
        .await
        .unwrap();

        // Order the two writers: the Goal owner publishes its wrapup into the
        // idle session first, and only then does the background process end
        // and publish its completion wake.
        for _ in 0..200 {
            if goal_children(&db).await.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            goal_children(&db).await.len(),
            1,
            "the Goal wrapup is published"
        );
        std::fs::write(&release_path, b"release").unwrap();
        for _ in 0..200 {
            let delivery = db.node.execute(
                "{ AgentToolCall(filter: { tool_name: { _eq: \"bash\" } }, limit: 1) { completion_notification_delivered_at } }",
            ).await;
            if goal_children(&db).await.len() == 1
                && delivery
                    .data
                    .as_ref()
                    .and_then(|data| data["AgentToolCall"].as_array())
                    .and_then(|rows| rows.first())
                    .is_some_and(|row| row["completion_notification_delivered_at"].is_string())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        gents::tool_call_lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(&db.node, db.node_identity.did()).await.unwrap();
        let observed = db.node.execute(
            "{ AgentRequest { _docID request_id created_at lifecycle_state execution_origin caused_by_trigger_kind caused_by_parent_request_id } AgentMessage { _docID agent_did requester_did request_doc_id } AgentToolCall { _docID tool_name lifecycle_state completion_notification_delivered_at } }",
        ).await;
        assert!(!observed.has_errors(), "{:?}", observed.errors);
        let data = observed.data.unwrap();
        // A Goal never suppresses a background completion: the wrapup and the
        // completion's coalesced wake are both published.
        assert_eq!(
            data["AgentRequest"].as_array().unwrap().len(),
            3,
            "expected the parent, the Goal wrapup and one completion wake; observed={data}"
        );
        let requests = data["AgentRequest"].as_array().unwrap();
        let goal_requests = requests
            .iter()
            .filter(|row| row["caused_by_trigger_kind"].as_str() == Some("goal"))
            .collect::<Vec<_>>();
        assert_eq!(
            goal_requests.len(),
            1,
            "expected exactly one Goal wrapup: {requests:?}"
        );
        let wakes = requests
            .iter()
            .filter(|row| {
                row["request_id"] != parent && row["_docID"] != goal_requests[0]["_docID"]
            })
            .collect::<Vec<_>>();
        assert_eq!(
            wakes.len(),
            1,
            "besides the parent and the Goal wrapup, only the completion wake exists: {requests:?}"
        );
        // The Goal owner continues the session's latest request, which at
        // wrapup time is the failed parent: the completion wake was written
        // only after the wrapup.
        let wrapup = goal_requests[0];
        assert_eq!(
            wrapup["caused_by_parent_request_id"], parent,
            "the wrapup must continue the session's latest request: {requests:?}"
        );
        // Goal continuations are themselves scheduled work, like the wake.
        assert_eq!(goal_requests[0]["execution_origin"], "scheduled");
        let messages = data["AgentMessage"].as_array().unwrap();
        let mut completions = Vec::new();
        for header in messages {
            let (_, message) = gents::session::load_canonical_message_from_node(
                db.node.as_ref(),
                header["_docID"].as_str().unwrap(),
                header["agent_did"].as_str().unwrap(),
                header["requester_did"].as_str(),
            )
            .await
            .unwrap();
            let rendered = format!("{message:?}");
            if rendered.contains("tool-completion") {
                completions.push((header, rendered));
            }
        }
        assert_eq!(completions.len(), 1);
        // The completion is published as the input of its wake.
        assert_eq!(completions[0].0["request_doc_id"], wakes[0]["_docID"]);
        let background_rows = data["AgentToolCall"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["tool_name"] == "bash")
            .collect::<Vec<_>>();
        assert_eq!(
            background_rows.len(),
            1,
            "the real background execution must have one physical lifecycle row: {}",
            data["AgentToolCall"]
        );
        assert!(
            background_rows[0]["completion_notification_delivered_at"].is_string(),
            "the physical background row must durably account its notification delivery: {}",
            data["AgentToolCall"]
        );
        let goal = load_canonical_goal(&db.node, db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.parsed_status(),
            Some(GoalStatus::BudgetLimited),
            "the runtime's sole GoalSource owns the budget decision after {terminal}"
        );
        runtime.shutdown().await;
        assert_eq!(goal.tokens_used, Some(expected_usage), "{terminal}");
        assert_eq!(goal.wrapup_requested, Some(true), "{terminal}");
        assert!(!goal.wrapup_completed.unwrap_or(false), "{terminal}");
        assert_eq!(
            goal.infrastructure_retry_count.unwrap_or_default(),
            0,
            "{terminal}"
        );
        let children = goal_children(&db).await;
        assert_eq!(children.len(), 1, "{terminal}");
        assert_eq!(
            children[0].caused_by_parent_request_id.as_deref(),
            Some(parent)
        );
        let input = children[0].input.as_ref().unwrap();
        assert_eq!(input["goal_continuation"]["wrapup"], true, "{terminal}");
        let history = gents::load_history(
            &db.node,
            &children[0].session_id,
            &children[0].agent_did,
            children[0].requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert!(
            serde_json::to_string(&history)
                .unwrap()
                .contains("late result required for final wrapup"),
            "the Goal child must be able to load the parent's durable background input"
        );

        // A restart while this wrapup is pending must not publish another child
        // or charge an ordinary infrastructure retry for the failed parent.
        let (mut restarted, _restart_tx) = self::source(&db).await;
        let restarted_fire =
            tokio::time::timeout(Duration::from_millis(200), restarted.next_fire()).await;
        assert!(
            restarted_fire.is_err(),
            "{terminal}: unexpected restart result; existing children={children:?}, emitted={:?}",
            restarted_fire.as_ref().ok().map(|result| result
                .as_ref()
                .map(|intent| &intent.pre_materialized_request_id))
        );
        let current = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.parsed_status(), Some(GoalStatus::BudgetLimited));
        assert_eq!(current.tokens_used, Some(expected_usage));
        assert_eq!(current.continuation_sequence, goal.continuation_sequence);
        assert_eq!(current.infrastructure_retry_count.unwrap_or_default(), 0);
        let persisted = goal_children(&db).await;
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].doc_id, children[0].doc_id);
        drop(restarted);
    }
}

#[tokio::test]
async fn token_budget_materializes_one_wrapup_and_never_repeats_it() {
    let db = test_db("goal-budget-wrapup").await;
    seed_completed_request(&db, "parent-budget").await;
    let usage = format!(
        r#"mutation {{
        add_InferenceCall(input: {{
            call_id: "goal-budget-call",
            runtime_instance_id: "goal-test",
            request_id: "parent-budget",
            call_seq: 1,
            backend_id: "backend-test",
            behavior_id: "test",
            agent_did: "{}",
            call_kind: "inference",
            attempt: 1,
            call_state: "completed",
            queued_at: "2026-07-15T00:00:00Z",
            started_at: "2026-07-15T00:00:00Z",
            ended_at: "2026-07-15T00:00:01Z",
            priority: 0,
            queue_depth_at_enqueue: 0,
            controller_generation: 1,
            backend_config_fingerprint: "goal-test",
            prompt_tokens: 100,
            completion_tokens: 5,
            cached_input_tokens: 90
        }}) {{ _docID }}
        add_InferenceCall(input: {{
            call_id: "goal-budget-call-foreign",
            runtime_instance_id: "goal-test-foreign",
            request_id: "parent-budget",
            call_seq: 1,
            backend_id: "backend-test",
            behavior_id: "test",
            agent_did: "did:key:foreign-goal-owner",
            call_kind: "inference",
            attempt: 1,
            call_state: "completed",
            queued_at: "2026-07-15T00:00:00Z",
            started_at: "2026-07-15T00:00:00Z",
            ended_at: "2026-07-15T00:00:01Z",
            priority: 0,
            queue_depth_at_enqueue: 0,
            controller_generation: 1,
            backend_config_fingerprint: "goal-test",
            prompt_tokens: 10000,
            completion_tokens: 10000,
            cached_input_tokens: 0
        }}) {{ _docID }}
    }}"#,
        db.node_identity.did()
    );
    let response = db.node.execute(&usage).await;
    assert!(!response.has_errors(), "seed usage: {:?}", response.errors);
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Stop at the durable budget"),
        Some(GoalStatus::Active),
        Some(Some(10)),
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out")
        .expect("wrapup intent");
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    let input = children[0].input.as_ref().expect("goal continuation input");
    assert_eq!(
        input.pointer("/goal_continuation/wrapup"),
        Some(&serde_json::json!(true))
    );

    set_request_lifecycle_state(db.node.as_ref(), &children[0].doc_id, "completed").await;
    publish_canonical_terminal_activity(&db, &children[0].doc_id, "final durable wrapup").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::BudgetLimited));
    // Charged total: prompt_tokens (100, cached input included by design) +
    // completion_tokens (5) == 105, matching the request ledger's formula.
    assert_eq!(goal.tokens_used, Some(105));
    assert_eq!(goal.wrapup_requested, Some(true));
    assert_eq!(goal.wrapup_completed, Some(true));
    assert_eq!(goal_children(&db).await.len(), 1);
    let child_identity = (
        children[0].doc_id.clone(),
        children[0].request_id.clone(),
        children[0].retry_key.clone(),
    );
    let continuation_sequence = goal.continuation_sequence;
    assert_eq!(
        session_token_usage(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("session usage before repeated recovery"),
        105,
    );
    drop(source);
    for _ in 0..2 {
        let recovered =
            gents::RequestLifecycle::recover_all(db.node.as_ref(), db.node_identity.did())
                .await
                .expect("repeat lease recovery");
        assert_eq!(recovered.requests_recovered, 0);
        assert_eq!(recovered.responses_recovered, 0);
        let (mut restarted, _restart_tx) = self::source(&db).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
                .await
                .is_err(),
            "recovery and goal reconciliation must not emit a second wrapup",
        );
        let current = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.parsed_status(), Some(GoalStatus::BudgetLimited));
        assert_eq!(
            current.tokens_used,
            Some(105),
            "the same persisted inference usage must not be charged again"
        );
        assert_eq!(current.continuation_sequence, continuation_sequence);
        assert_eq!(current.wrapup_completed, Some(true));
        assert_eq!(
            session_token_usage(db.node.as_ref(), db.node_identity.did(), SESSION)
                .await
                .unwrap(),
            105,
            "recovery must preserve the session charge, including cached input exactly once",
        );
        let after = goal_children(&db).await;
        assert_eq!(after.len(), 1);
        assert_eq!(
            (
                after[0].doc_id.clone(),
                after[0].request_id.clone(),
                after[0].retry_key.clone()
            ),
            child_identity
        );
    }
}

#[tokio::test]
async fn session_token_usage_charges_cached_input_as_part_of_the_total() {
    let db = test_db("goal-cached-input").await;
    seed_completed_request(&db, "parent-cached").await;
    let usage = format!(
        r#"mutation {{
        add_InferenceCall(input: {{
            call_id: "goal-cached-call",
            runtime_instance_id: "goal-test",
            request_id: "parent-cached",
            call_seq: 1,
            backend_id: "backend-test",
            behavior_id: "test",
            agent_did: "{}",
            call_kind: "inference",
            attempt: 1,
            call_state: "completed",
            queued_at: "2026-07-15T00:00:00Z",
            started_at: "2026-07-15T00:00:00Z",
            ended_at: "2026-07-15T00:00:01Z",
            priority: 0,
            queue_depth_at_enqueue: 0,
            controller_generation: 1,
            backend_config_fingerprint: "goal-test",
            prompt_tokens: 1000,
            completion_tokens: 50,
            cached_input_tokens: 800
        }}) {{ _docID }}
    }}"#,
        db.node_identity.did()
    );
    let response = db.node.execute(&usage).await;
    assert!(!response.has_errors(), "seed usage: {:?}", response.errors);

    let tokens = session_token_usage(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("session token usage");
    // Charged total is prompt_tokens (1000, cached input included) +
    // completion_tokens (50) == 1050, not the old "fresh input" formula
    // ((1000 - 800) + 50 == 250).
    assert_eq!(tokens, 1050);
}

#[tokio::test]
async fn provider_usage_limit_moves_active_goal_to_usage_limited() {
    let db = test_db("goal-provider-usage-limit").await;
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        "usage-limited-request",
        SESSION,
        "failed",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    let response = db
        .node
        .execute(
            r#"mutation {
                add_InferenceCall(input: {
                    call_id: "usage-limited-call",
                    request_id: "usage-limited-request",
                    call_seq: 1,
                    attempt: 1,
                    call_state: "failed",
                    failure_reason: "provider insufficient_quota: credit balance exhausted"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "seed usage limit: {:?}",
        response.errors
    );
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Stop on provider quota exhaustion"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::UsageLimited));
    assert!(goal
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("insufficient_quota")));
}

const RENDERED_LIMIT: &str =
    "provider usage limit reached (resets at 2026-07-15T05:00:00Z): capped";

async fn seed_failed_call(
    db: &TestDb,
    call_id: &str,
    call_kind: &str,
    attempt: i64,
    ended_at: &str,
    failure_reason: &str,
) {
    let response = db
        .node
        .execute(&format!(
            r#"mutation {{
                add_InferenceCall(input: {{
                    call_id: "{call_id}",
                    request_id: "usage-limited-request",
                    call_seq: 1,
                    call_kind: "{call_kind}",
                    attempt: {attempt},
                    call_state: "failed",
                    ended_at: "{ended_at}",
                    failure_reason: "{failure_reason}"
                }}) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "seed failed call: {:?}",
        response.errors
    );
}

async fn usage_limited_goal_after_failed_request(db: &TestDb) -> Option<String> {
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        "usage-limited-request",
        SESSION,
        "failed",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Stop on the call that ended the request"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    let (mut source, _snapshot_tx) = source(db).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    (goal.parsed_status() == Some(GoalStatus::UsageLimited))
        .then_some(goal.last_failure)
        .flatten()
}

#[tokio::test]
async fn compaction_usage_limit_moves_goal_to_usage_limited() {
    let db = test_db("goal-compaction-usage-limit").await;
    seed_failed_call(
        &db,
        "compaction-call",
        "compaction",
        1,
        "2026-07-15T00:00:10Z",
        RENDERED_LIMIT,
    )
    .await;
    let last_failure = usage_limited_goal_after_failed_request(&db).await;
    assert!(
        last_failure
            .as_deref()
            .is_some_and(|reason| reason.contains("resets at 2026-07-15T05:00:00Z")),
        "{last_failure:?}"
    );
}

#[tokio::test]
async fn a_later_limited_call_wins_over_an_earlier_retry() {
    let db = test_db("goal-later-limited-call").await;
    seed_failed_call(
        &db,
        "earlier-retry",
        "inference",
        2,
        "2026-07-15T00:00:05Z",
        "HttpError: Invalid status code 500",
    )
    .await;
    seed_failed_call(
        &db,
        "later-compaction",
        "compaction",
        1,
        "2026-07-15T00:00:10Z",
        RENDERED_LIMIT,
    )
    .await;
    let last_failure = usage_limited_goal_after_failed_request(&db).await;
    assert!(
        last_failure
            .as_deref()
            .is_some_and(|reason| reason.contains("resets at 2026-07-15T05:00:00Z")),
        "{last_failure:?}"
    );
}

/// An active Goal whose request failed on a limit whose reset has passed.
async fn seed_a_passed_reset(db: &TestDb, opted_in: bool) {
    let did = db.node_identity.did();
    crate::support::fixtures::bind_behavior_backend(
        db.node.as_ref(),
        did,
        crate::support::AGENT_NAME,
        "reset-backend",
        "http://127.0.0.1:1/v1",
        "test-model",
    )
    .await;
    // Resume authenticates its predecessor, so the request is signed.
    let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "usage-limited-request",
        did,
        did,
        crate::support::AGENT_NAME,
        SESSION,
        "limited work",
        "interactive",
        "2026-07-15T00:00:00Z",
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    gents::sign_agent_request_create(db.node_identity.as_ref(), &mut request)
        .await
        .expect("sign request");
    let response = db
        .node
        .execute(&request.graphql_mutation().expect("request mutation"))
        .await;
    assert!(
        !response.has_errors(),
        "seed request: {:?}",
        response.errors
    );
    let response = db
        .node
        .execute(&format!(
            r#"mutation {{
                update_AgentRequest(filter: {{ request_id: {{ _eq: "usage-limited-request" }} }}, input: {{
                    lifecycle_state: "failed"
                }}) {{ _docID }}
                add_InferenceCall(input: {{
                    call_id: "limited-call", request_id: "usage-limited-request", call_seq: 1,
                    backend_id: "reset-backend", behavior_id: "{agent}", agent_did: "{did}",
                    call_kind: "inference", attempt: 1, call_state: "failed",
                    queued_at: "2026-07-15T00:00:05Z", started_at: "2026-07-15T00:00:05Z",
                    ended_at: "2026-07-15T00:00:10Z", failure_reason: "{RENDERED_LIMIT}"
                }}) {{ _docID }}
            }}"#,
            agent = crate::support::AGENT_NAME,
        ))
        .await;
    assert!(!response.has_errors(), "seed call: {:?}", response.errors);
    set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Resume at the reported reset"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    if opted_in {
        set_goal_from_access(
            &ConfigAccess::Local(db.node.clone()),
            did,
            SESSION,
            None,
            None,
            None,
            Some(true),
        )
        .await
        .expect("opt in");
    }
}

/// The Goal's status and its continuation count.
async fn goal_and_children(db: &TestDb) -> (Option<GoalStatus>, usize) {
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    let response = db.node.execute(r#"{ AgentRequest { retry_key } }"#).await;
    let children = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            row["retry_key"]
                .as_str()
                .is_some_and(|key| key.starts_with("goal-continuation:"))
        })
        .count();
    (goal.parsed_status(), children)
}

/// One rescan pass over a Goal stopped by a limit whose reset has passed.
async fn goal_after_a_passed_reset(name: &str, opted_in: bool) -> (Option<GoalStatus>, usize) {
    let db = test_db(name).await;
    seed_a_passed_reset(&db, opted_in).await;
    let (mut source, _snapshot_tx) = source(&db).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), source.next_fire())
            .await
            .is_err()
    );
    goal_and_children(&db).await
}

#[tokio::test]
async fn a_database_update_leaves_a_passed_reset_to_the_rescan_tick() {
    let db = test_db("goal-reset-update").await;
    seed_a_passed_reset(&db, true).await;
    publish_behavior_readiness(
        &db,
        gents_protocol::row::BehaviorReadinessProcessState::Ready,
        None,
    )
    .await;
    let updates = MockUpdateSubscriptionSource::new();
    let (_snapshot_tx, rx) = watch::channel(snapshot(db.node_identity.did()));
    let mut source = GoalSource::with_subscription_source(
        Arc::new(updates.clone()),
        rx,
        db.node.clone(),
        CancellationToken::new(),
    )
    .with_rescan_interval(Duration::from_secs(3600));
    // The first tick stops the Goal on its limit; the next is an hour away.
    let pass = Duration::from_millis(300);
    assert!(tokio::time::timeout(pass, source.next_fire())
        .await
        .is_err());
    assert_eq!(
        goal_and_children(&db).await,
        (Some(GoalStatus::UsageLimited), 0)
    );
    updates.publish_update("Goal", "goal-doc");
    assert!(tokio::time::timeout(pass, source.next_fire())
        .await
        .is_err());
    assert_eq!(
        goal_and_children(&db).await,
        (Some(GoalStatus::UsageLimited), 0)
    );
}

#[tokio::test]
async fn opted_in_goal_resumes_at_its_reported_reset() {
    assert_eq!(
        goal_after_a_passed_reset("goal-reset-opted-in", true).await,
        (Some(GoalStatus::Active), 1)
    );
    assert_eq!(
        goal_after_a_passed_reset("goal-reset-not-opted-in", false).await,
        (Some(GoalStatus::UsageLimited), 0)
    );
}

#[tokio::test]
async fn failed_wrapup_retries_twice_then_is_durably_abandoned() {
    let db = test_db("goal-wrapup-retry-bound").await;
    seed_completed_request(&db, "parent-wrapup-retry").await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Bound failed wrap-up retries"),
        Some(GoalStatus::Active),
        Some(Some(1)),
    )
    .await
    .expect("set goal");
    let response = db
        .node
        .execute(&format!(
            r#"mutation {{
                add_InferenceCall(input: {{
                    call_id: "wrapup-budget-call",
                    request_id: "parent-wrapup-retry",
                    agent_did: "{}",
                    call_seq: 1,
                    attempt: 1,
                    call_state: "completed",
                    prompt_tokens: 2,
                    completion_tokens: 0,
                    cached_input_tokens: 0
                }}) {{ _docID }}
            }}"#,
            db.node_identity.did()
        ))
        .await;
    assert!(
        !response.has_errors(),
        "seed wrapup usage: {:?}",
        response.errors
    );

    let (mut source, _snapshot_tx) = source(&db).await;
    for expected_children in 1..=3 {
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .unwrap_or_else(|_| panic!("goal source timed out before child {expected_children}"))
            .expect("wrapup or retry intent");
        let children = goal_children(&db).await;
        assert_eq!(children.len(), expected_children);
        let child = children
            .iter()
            .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
            .expect("new pending wrap-up child");
        set_request_lifecycle_state(db.node.as_ref(), &child.doc_id, "failed").await;
    }

    assert!(
        tokio::time::timeout(Duration::from_millis(250), source.next_fire())
            .await
            .is_err(),
        "bounded failed wrap-up must not spawn a fourth child"
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::BudgetLimited));
    assert_eq!(goal.infrastructure_retry_count, Some(2));
    assert_eq!(goal.wrapup_completed, Some(true));
    assert!(goal
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("after 2 retries")));
    assert_eq!(goal_children(&db).await.len(), 3);
}

#[tokio::test]
async fn config_set_cannot_reactivate_without_publishing_a_continuation() {
    let db = test_db("goal-config-resume-denied").await;
    for status in [
        GoalStatus::Paused,
        GoalStatus::Blocked,
        GoalStatus::UsageLimited,
        GoalStatus::BudgetLimited,
        GoalStatus::Complete,
    ] {
        let session = format!("config-resume-{}", status.as_str());
        let before = set_goal(
            &db.node,
            db.node_identity.did(),
            &session,
            Some("Keep configuration separate from continuation admission"),
            Some(status),
            Some(Some(1000)),
        )
        .await
        .expect("create goal");
        let result = set_goal(
            &db.node,
            db.node_identity.did(),
            &session,
            None,
            Some(GoalStatus::Active),
            None,
        )
        .await;
        assert!(
            result.is_err(),
            "{} must require a typed continuation",
            status.as_str()
        );
        let after = load_canonical_goal(&db.node, db.node_identity.did(), &session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status, before.status);
        assert_eq!(
            after.continuation_sequence(),
            before.continuation_sequence()
        );
        assert_eq!(after.token_budget, before.token_budget);
    }
}

#[tokio::test]
async fn continuation_sequence_exhaustion_rejects_claims_without_retry_charge() {
    for retry in [false, true] {
        let name = if retry {
            "goal-retry-sequence-exhaustion"
        } else {
            "goal-sequence-exhaustion"
        };
        let db = test_db(name).await;
        let initial = set_goal(
            db.node.as_ref(),
            db.node_identity.did(),
            SESSION,
            Some("Never reuse an exhausted continuation sequence"),
            Some(GoalStatus::Active),
            None,
        )
        .await
        .unwrap();
        seed_goal_fields(
            db.node.as_ref(),
            &initial,
            &format!(
                "continuation_sequence: {}, infrastructure_retry_count: 0, last_failure: null, last_continued_from_request_id: null",
                i64::MAX - 1
            ),
        )
        .await
        .unwrap();
        let before = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        let claimed = if retry {
            claim_retry_continuation(db.node.as_ref(), &before, "last-parent", 1, "failed").await
        } else {
            claim_continuation(db.node.as_ref(), &before, "last-parent").await
        };
        assert!(claimed.expect("MAX-1 must permit one final sequence advance"));
        let exhausted = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exhausted.continuation_sequence(), i64::MAX);
        assert_eq!(
            exhausted.last_continued_from_request_id.as_deref(),
            Some("last-parent")
        );
        assert_eq!(exhausted.infrastructure_retry_count, Some(i64::from(retry)));
        let rejected = if retry {
            claim_retry_continuation(
                db.node.as_ref(),
                &exhausted,
                "must-not-be-claimed",
                2,
                "must-not-be-charged",
            )
            .await
        } else {
            claim_continuation(db.node.as_ref(), &exhausted, "must-not-be-claimed").await
        };
        assert!(
            rejected.is_err(),
            "exhaustion must fail explicitly: {rejected:?}"
        );
        let after = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&exhausted).unwrap(),
            "rejected claim must not change watermark, retry charge, failure, or timestamp"
        );
    }
}

#[tokio::test]
async fn stale_usage_refresh_preserves_completed_goal_time_accounting() {
    let db = test_db("goal-stale-usage-after-completion").await;
    let initial = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("A stale observer cannot restart completed goal accounting"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    seed_goal_fields(
        db.node.as_ref(),
        &initial,
        "active_time_seconds: 7, active_started_at: null",
    )
    .await
    .unwrap();
    let stale = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        None,
        Some(GoalStatus::Complete),
        None,
    )
    .await
    .unwrap();
    // Pin a distinguishable completed accounting value without elapsed-clock
    // assertions. The stale snapshot still carries seven active seconds.
    seed_goal_fields(
        db.node.as_ref(),
        &stale,
        "active_time_seconds: 73, active_started_at: null",
    )
    .await
    .unwrap();
    let completed = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, "complete");
    assert_eq!(completed.active_time_seconds, Some(73));
    assert!(completed.active_started_at.is_none());

    gents::goal::refresh_goal_usage(db.node.as_ref(), &stale)
        .await
        .expect("stale usage observation must safely skip obsolete accounting writes");
    let after = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, "complete");
    assert_eq!(after.active_time_seconds, completed.active_time_seconds);
    assert_eq!(after.active_started_at, completed.active_started_at);
    assert_eq!(
        after.continuation_sequence(),
        completed.continuation_sequence()
    );
}

async fn seed_operator_resume_parent(db: &TestDb, request_id: &str) -> String {
    let did = db.node_identity.did();
    let mut parent = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        did,
        did,
        crate::support::AGENT_NAME,
        SESSION,
        "Continue the original graph work",
        "interactive",
        "2026-07-15T00:00:00Z",
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    parent.caused_by_trigger_id = Some("original-graph-trigger".into());
    parent.caused_by_trigger_kind = Some("event".into());
    parent.caused_by_correlation = Some("original-graph-correlation".into());
    parent.caused_by_source_doc_id = Some("original-event-document".into());
    parent.caused_by_trigger_context = Some(r#"{"source_fields":{"artifact":"original"}}"#.into());
    parent.subagent_depth = 2;
    parent.caused_by_parent_request_id = Some("upstream-request".into());
    parent.caused_by_parent_request_doc_id = Some("upstream-document".into());
    parent.caused_by_parent_tool_call_id = Some("upstream-tool-call".into());
    parent.caused_by_parent_tool_call_doc_id = Some("upstream-tool-document".into());
    parent.workspace_id = Some("resume-workspace".into());
    parent.workspace_authority = Some("readOnly".into());
    parent.workspace_owner_agent_did = Some(did.to_string());
    parent.workspace_seal_hash = Some("resume-seal".into());
    gents::sign_agent_request_create(db.node_identity.as_ref(), &mut parent)
        .await
        .unwrap();
    let response = db.node.execute(&parent.graphql_mutation().unwrap()).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
        gents::graphql::escape_graphql_string(request_id),
    );
    let response = db.node.execute(&query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let doc_id = response.data.unwrap()["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    set_request_lifecycle_state(db.node.as_ref(), &doc_id, "interrupted").await;
    doc_id
}

async fn operator_resume_child_row(db: &TestDb, doc_id: &str) -> serde_json::Value {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
        _docID request_id agent_did requester_did session_id behavior_id lifecycle_state
        content created_at execution_origin retry_key input admission_kind admission_signer_did
        admission_signature runtime_issuer_did runtime_source_request_id runtime_source_kind
        caused_by_trigger_id caused_by_trigger_doc_id caused_by_trigger_kind
        caused_by_correlation caused_by_source_doc_id caused_by_trigger_context
        caused_by_parent_request_id caused_by_parent_request_doc_id
        caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id
        subagent_depth workspace_id workspace_authority workspace_owner_agent_did workspace_seal_hash
    }} }}"#,
        gents::graphql::escape_graphql_string(doc_id)
    );
    let response = db.node.execute(&query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()["AgentRequest"][0].clone()
}

#[tokio::test]
async fn operator_resume_publishes_one_lineage_child_and_fences_old_pause() {
    let db = test_db("operator-resume-lineage").await;
    let did = db.node_identity.did();
    let parent_doc = seed_operator_resume_parent(&db, "resume-parent").await;
    let stale_active = set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Complete graph work"),
        Some(GoalStatus::Active),
        Some(Some(1000)),
    )
    .await
    .unwrap();
    set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        None,
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    let access = ConfigAccess::Local(db.node.clone());
    let receipt = gents::goal::resume_goal_request(
        &access,
        db.node_identity.as_ref(),
        did,
        SESSION,
        "resume-parent",
    )
    .await
    .unwrap();
    assert!(receipt.created);
    let goal = load_canonical_goal(db.node.as_ref(), did, SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.goal_id, goal.goal_id);
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.continuation_sequence(), 1);
    assert_eq!(
        goal.last_continued_from_request_id.as_deref(),
        Some("resume-parent")
    );
    assert_eq!(goal.token_budget, Some(1000));
    assert_eq!(goal_children(&db).await.len(), 1);
    let child = operator_resume_child_row(&db, &receipt.doc_id).await;
    assert_eq!(child["request_id"], receipt.request_id);
    for field in [
        "agent_did",
        "requester_did",
        "admission_signer_did",
        "runtime_issuer_did",
    ] {
        assert_eq!(child[field], did, "{field}");
    }
    assert_eq!(child["session_id"], SESSION);
    assert_eq!(child["behavior_id"], crate::support::AGENT_NAME);
    assert_eq!(child["lifecycle_state"], "pending");
    assert_eq!(child["execution_origin"], "scheduled");
    assert_eq!(child["runtime_source_request_id"], "resume-parent");
    assert!(child["admission_signature"]
        .as_str()
        .is_some_and(|v| !v.is_empty()));
    assert_eq!(child["caused_by_trigger_id"], goal.goal_id);
    assert_eq!(child["caused_by_trigger_kind"], "goal");
    assert!(child["caused_by_trigger_doc_id"].is_null());
    assert_eq!(child["caused_by_correlation"], "original-graph-correlation");
    assert_eq!(child["caused_by_source_doc_id"], "original-event-document");
    assert_eq!(
        child["caused_by_trigger_context"],
        r#"{"source_fields":{"artifact":"original"}}"#
    );
    assert_eq!(child["caused_by_parent_request_id"], "resume-parent");
    assert_eq!(child["caused_by_parent_request_doc_id"], parent_doc);
    assert!(child["caused_by_parent_tool_call_id"].is_null());
    assert!(child["caused_by_parent_tool_call_doc_id"].is_null());
    assert_eq!(child["subagent_depth"], 2);
    assert_eq!(child["workspace_id"], "resume-workspace");
    assert_eq!(child["workspace_authority"], "readOnly");
    assert_eq!(child["workspace_owner_agent_did"], did);
    assert_eq!(child["workspace_seal_hash"], "resume-seal");
    assert!(
        !update_goal_fields_if_status(
            db.node.as_ref(),
            &stale_active,
            GoalStatus::Active,
            r#"status: "paused""#
        )
        .await
        .unwrap(),
        "pre-resume Active snapshot must lose ABA"
    );
    assert_eq!(
        load_canonical_goal(db.node.as_ref(), did, SESSION)
            .await
            .unwrap()
            .unwrap()
            .parsed_status(),
        Some(GoalStatus::Active)
    );
}

#[tokio::test]
async fn operator_resume_retry_after_goal_progress_returns_original_child_without_mutation() {
    let db = test_db("operator-resume-retry-progress").await;
    let did = db.node_identity.did();
    seed_operator_resume_parent(&db, "retry-parent").await;
    set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Original objective"),
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    let access = ConfigAccess::Local(db.node.clone());
    let first = gents::goal::resume_goal_request(
        &access,
        db.node_identity.as_ref(),
        did,
        SESSION,
        "retry-parent",
    )
    .await
    .unwrap();
    set_request_lifecycle_state(db.node.as_ref(), &first.doc_id, "interrupted").await;
    let before = set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Changed objective after progress"),
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    let child_before = operator_resume_child_row(&db, &first.doc_id).await;
    let retry = gents::goal::resume_goal_request(
        &access,
        db.node_identity.as_ref(),
        did,
        SESSION,
        "retry-parent",
    )
    .await
    .unwrap();
    assert!(!retry.created);
    assert_eq!(
        (retry.request_id, retry.doc_id),
        (first.request_id, first.doc_id)
    );
    let after = load_canonical_goal(db.node.as_ref(), did, SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(after).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert_eq!(
        operator_resume_child_row(&db, &child_before["_docID"].as_str().unwrap()).await,
        child_before
    );
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn operator_resume_rejects_foreign_identity_even_with_target_key_registered() {
    let target = test_db("operator-resume-target").await;
    let foreign = test_db("operator-resume-foreign").await;
    let did = target.node_identity.did();
    seed_operator_resume_parent(&target, "owned-parent").await;
    let before = set_goal(
        target.node.as_ref(),
        did,
        SESSION,
        Some("Owner work"),
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    let access = ConfigAccess::Local(target.node.clone());
    assert!(gents::goal::resume_goal_request(
        &access,
        foreign.node_identity.as_ref(),
        did,
        SESSION,
        "owned-parent"
    )
    .await
    .is_err());
    assert!(goal_children(&target).await.is_empty());
    assert_eq!(
        serde_json::to_value(
            load_canonical_goal(target.node.as_ref(), did, SESSION)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(before).unwrap()
    );
}

#[tokio::test]
async fn operator_resume_rejects_busy_session_and_old_predecessor() {
    for newer_status in ["pending", "completed"] {
        let db = test_db(&format!("operator-resume-newer-{newer_status}")).await;
        let did = db.node_identity.did();
        seed_operator_resume_parent(&db, "older-parent").await;
        create_request_for_agent_with_signed_fields(
            db.node.as_ref(),
            did,
            "newer-request",
            SESSION,
            newer_status,
            "2026-07-15T00:00:01Z",
            None,
            None,
            None,
            None,
        )
        .await;
        let before = set_goal(
            db.node.as_ref(),
            did,
            SESSION,
            Some("Respect latest work"),
            Some(GoalStatus::Paused),
            None,
        )
        .await
        .unwrap();
        let access = ConfigAccess::Local(db.node.clone());
        assert!(gents::goal::resume_goal_request(
            &access,
            db.node_identity.as_ref(),
            did,
            SESSION,
            "older-parent"
        )
        .await
        .is_err());
        assert!(goal_children(&db).await.is_empty());
        assert_eq!(
            serde_json::to_value(
                load_canonical_goal(db.node.as_ref(), did, SESSION)
                    .await
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
}

#[tokio::test]
async fn resume_resets_blocked_audit_identity_and_count() {
    let db = test_db("goal-resume-audit-reset").await;
    let did = db.node_identity.did();
    seed_operator_resume_parent(&db, "request-3").await;
    let goal = set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Resume with a fresh blocked audit"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    seed_goal_fields(db.node.as_ref(), &goal,
        r#"status: "blocked", consecutive_blocked_audits: 3, last_blocked_request_id: "request-3", last_blocked_reason: "needs approval", active_started_at: null"#).await.unwrap();
    let access = ConfigAccess::Local(db.node.clone());
    let receipt = gents::goal::resume_goal_request(
        &access,
        db.node_identity.as_ref(),
        did,
        SESSION,
        "request-3",
    )
    .await
    .unwrap();
    assert!(receipt.created);
    let resumed = load_canonical_goal(db.node.as_ref(), did, SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(resumed.consecutive_blocked_audits, Some(0));
    assert_eq!(resumed.last_blocked_request_id, None);
    assert_eq!(resumed.last_blocked_reason, None);
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn concurrent_operator_resumes_publish_one_child() {
    let db = test_db("operator-resume-concurrent").await;
    let did = db.node_identity.did();
    seed_operator_resume_parent(&db, "concurrent-parent").await;
    set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("One operator continuation"),
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    let access = ConfigAccess::Local(db.node.clone());
    let (left, right) = tokio::join!(
        gents::goal::resume_goal_request(
            &access,
            db.node_identity.as_ref(),
            did,
            SESSION,
            "concurrent-parent"
        ),
        gents::goal::resume_goal_request(
            &access,
            db.node_identity.as_ref(),
            did,
            SESSION,
            "concurrent-parent"
        ),
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_ne!(left.created, right.created);
    assert_eq!(
        (left.request_id, left.doc_id),
        (right.request_id, right.doc_id)
    );
    assert_eq!(goal_children(&db).await.len(), 1);
    let goal = load_canonical_goal(db.node.as_ref(), did, SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.continuation_sequence(), 1);
}

#[tokio::test]
async fn operator_resume_rejects_corrupted_child_receipt_without_reactivation() {
    let db = test_db("operator-resume-corrupt-receipt").await;
    let did = db.node_identity.did();
    seed_operator_resume_parent(&db, "corrupt-parent").await;
    set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Original signed work"),
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    let access = ConfigAccess::Local(db.node.clone());
    let first = gents::goal::resume_goal_request(
        &access,
        db.node_identity.as_ref(),
        did,
        SESSION,
        "corrupt-parent",
    )
    .await
    .unwrap();
    set_request_lifecycle_state(db.node.as_ref(), &first.doc_id, "interrupted").await;
    let goal_before = set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        None,
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    // Immutable content cannot be updated in place. Model hostile persisted
    // input by replacing the fixture row while retaining its original signature.
    let original = db.node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
            request_id agent_did requester_did admission_kind admission_signer_did admission_signature enrollment_request_id enrollment_request_digest enrollment_admin_did enrollment_authorization_sequence enrollment_authorization_expires_at runtime_issuer_did runtime_source_request_id runtime_source_kind behavior_id session_id retry_parent_request retry_parent_request_doc_id retry_root_request retry_key content max_total_tokens input execution_origin caused_by_trigger_id caused_by_trigger_doc_id caused_by_trigger_kind caused_by_correlation caused_by_trigger_context caused_by_source_doc_id created_at retry_count max_retries valid_until subagent_depth caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id workspace_id workspace_authority workspace_owner_agent_did workspace_seal_hash lifecycle_state
        }} }}"#, gents::graphql::escape_graphql_string(&first.doc_id),
    )).await;
    assert!(!original.has_errors(), "{:?}", original.errors);
    let mut forged = original.data.unwrap()["AgentRequest"][0].clone();
    forged["content"] = serde_json::json!("unsigned replacement");
    let removed = db
        .node
        .execute(&format!(
            r#"mutation {{ delete_AgentRequest(docID: "{}") {{ _docID }} }}"#,
            gents::graphql::escape_graphql_string(&first.doc_id),
        ))
        .await;
    assert!(!removed.has_errors(), "{:?}", removed.errors);
    let inserted = db
        .node
        .execute(&format!(
            "mutation {{ add_AgentRequest(input: {}) {{ _docID }} }}",
            gents_protocol::graphql::graphql_input_literal(&forged).unwrap(),
        ))
        .await;
    assert!(!inserted.has_errors(), "{:?}", inserted.errors);
    let inserted = &inserted.data.as_ref().unwrap()["add_AgentRequest"];
    let forged_doc_id = inserted
        .get("_docID")
        .or_else(|| inserted.get(0).and_then(|row| row.get("_docID")))
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let child_before = operator_resume_child_row(&db, &forged_doc_id).await;
    assert_eq!(child_before["content"], "unsigned replacement");
    assert!(
        gents::goal::resume_goal_request(
            &access,
            db.node_identity.as_ref(),
            did,
            SESSION,
            "corrupt-parent",
        )
        .await
        .is_err(),
        "a matching retry key must not authenticate changed content"
    );
    let goal_after = load_canonical_goal(db.node.as_ref(), did, SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(goal_after).unwrap(),
        serde_json::to_value(goal_before).unwrap()
    );
    assert_eq!(
        operator_resume_child_row(&db, &forged_doc_id).await,
        child_before
    );
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn operator_resume_rejects_older_unfinished_siblings_without_mutation() {
    for (label, state) in [
        ("missing", None),
        ("unknown", Some("unknownState")),
        ("workspace", Some("workspaceBindingPending")),
    ] {
        let db = test_db(&format!("operator-resume-unfinished-{label}")).await;
        let did = db.node_identity.did();
        // Both signed fixtures share created_at; the canonical secondary
        // ordering makes a-older precede z-parent. The latest-parent guard
        // therefore cannot hide a missing whole-session terminal check.
        let sibling = seed_operator_resume_parent(&db, "a-older").await;
        seed_operator_resume_parent(&db, "z-parent").await;
        let state = state
            .map(|value| format!("\"{}\"", gents::graphql::escape_graphql_string(value)))
            .unwrap_or_else(|| "null".to_owned());
        let response = db.node.execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: {state} }}) {{ _docID }} }}"#,
            gents::graphql::escape_graphql_string(&sibling)
        )).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let before = set_goal(
            db.node.as_ref(),
            did,
            SESSION,
            Some("Do not bypass unfinished sibling work"),
            Some(GoalStatus::Paused),
            None,
        )
        .await
        .unwrap();
        let error = gents::goal::resume_goal_request(
            &ConfigAccess::Local(db.node.clone()),
            db.node_identity.as_ref(),
            did,
            SESSION,
            "z-parent",
        )
        .await
        .expect_err("every unfinished sibling must block resume");
        let detail = format!("{error:#}");
        assert!(
            !detail.contains("no longer the latest"),
            "wrong guard masked unfinished row: {detail}"
        );
        if label != "unknown" {
            assert!(detail.contains("unfinished"), "unexpected guard: {detail}");
        }
        assert!(goal_children(&db).await.is_empty());
        let after = load_canonical_goal(db.node.as_ref(), did, SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(after).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
}

#[tokio::test]
async fn operator_resume_preserves_pending_wrapup_in_child_policy() {
    let db = test_db("operator-resume-retained-wrapup").await;
    let did = db.node_identity.did();
    seed_operator_resume_parent(&db, "wrapup-parent").await;
    let paused = set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Finish the already requested wrap-up"),
        Some(GoalStatus::Paused),
        None,
    )
    .await
    .unwrap();
    seed_goal_fields(
        db.node.as_ref(),
        &paused,
        "wrapup_requested: true, wrapup_completed: false",
    )
    .await
    .unwrap();
    let receipt = gents::goal::resume_goal_request(
        &ConfigAccess::Local(db.node.clone()),
        db.node_identity.as_ref(),
        did,
        SESSION,
        "wrapup-parent",
    )
    .await
    .unwrap();
    let goal = load_canonical_goal(db.node.as_ref(), did, SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.wrapup_requested, Some(true));
    assert_eq!(goal.wrapup_completed, Some(false));
    let child = operator_resume_child_row(&db, &receipt.doc_id).await;
    let input = &child["input"];
    assert_eq!(
        input
            .pointer("/goal_continuation/wrapup")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let content = child["content"].as_str().unwrap();
    assert!(content.contains("one final wrap-up turn"), "{content}");
    assert!(
        content.contains("Do not expect another automatic continuation"),
        "{content}"
    );
}

/// Fixture-only mutation for constructing boundary states. Production writes
/// go through the guarded Goal owner; tests do not require an unfenced API.
async fn seed_goal_fields(
    node: &defra_node::EmbeddedNode,
    goal: &gents::goal::GoalDocument,
    fields: &str,
) -> anyhow::Result<()> {
    let doc_id = gents::graphql::escape_graphql_string(&goal.doc_id);
    let agent_did = gents::graphql::escape_graphql_string(&goal.agent_did);
    let response = node.execute(&format!(
        r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{agent_did}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    )).await;
    anyhow::ensure!(
        !response.has_errors(),
        "seed Goal fields: {:?}",
        response.errors
    );
    Ok(())
}

async fn seed_same_second_canonical_goal_child(
    db: &TestDb,
    parent_id: &str,
    paused: bool,
) -> (String, String) {
    use sha2::{Digest, Sha256};
    let did = db.node_identity.did();
    // The generic completed-row fixture does not sign admission. This test
    // exercises authenticated ancestry, so create a real signed root first.
    let mut parent = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        parent_id,
        did,
        did,
        crate::support::AGENT_NAME,
        SESSION,
        "Signed root of the same-second continuation chain",
        "interactive",
        "2026-07-15T00:00:00Z",
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    gents::sign_agent_request_create(db.node_identity.as_ref(), &mut parent)
        .await
        .unwrap();
    let response = db.node.execute(&parent.graphql_mutation().unwrap()).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            gents::graphql::escape_graphql_string(parent_id),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let parent_doc = response.data.unwrap()["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    set_request_lifecycle_state(db.node.as_ref(), &parent_doc, "completed").await;
    let goal = set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Continue from the actual causal head"),
        Some(if paused {
            GoalStatus::Paused
        } else {
            GoalStatus::Active
        }),
        None,
    )
    .await
    .unwrap();
    let digest = Sha256::digest(format!("{}\0{parent_id}", goal.goal_id).as_bytes());
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let child_id = format!("goal-cont-{:020}-{suffix}", 1);
    assert!(
        child_id.as_str() < parent_id,
        "fixture must defeat lexical head selection"
    );
    let identity = gents::RequestIdentity {
        requester_did: None,
        request_id: child_id.clone(),
        agent_did: did.to_owned(),
        behavior_id: crate::support::AGENT_NAME.to_owned(),
        session_id: SESSION.to_owned(),
        content: "The canonical continuation made durable progress".to_owned(),
        execution_origin: gents::lifecycle::ExecutionOrigin::Scheduled,
        created_at: "2026-07-15T00:00:00Z".to_owned(),
    };
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
            did, parent_id,
        );
    let mut spec = gents::RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        identity,
        admission,
    );
    spec.trigger_lineage.trigger_id = Some(goal.goal_id.clone());
    spec.trigger_lineage.trigger_kind = Some("goal".to_owned());
    spec.subagent = Some(gents::ParentLink {
        parent_request_id: parent_id.to_owned(),
        parent_request_doc_id: parent_doc,
        ..Default::default()
    });
    spec.retry_key = Some(format!("goal-continuation:{suffix}"));
    spec.input = serde_json::from_value(serde_json::json!({
        "queue": {
            "source": "goal", "policy": "coalesce", "key": format!("goal:{suffix}"),
            "queued_after_request_id": parent_id
        },
        "goal_continuation": {"sequence": 1, "wrapup": false}
    }))
    .unwrap();
    let child = gents::build_signed_request(
        spec,
        gents::RequestSigner::Identity(db.node_identity.as_ref()),
    )
    .await
    .unwrap();
    let response = db.node.execute(&child.graphql_mutation().unwrap()).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let children = goal_children(db).await;
    assert_eq!(children.len(), 1);
    let child_doc = children[0].doc_id.clone();
    set_request_lifecycle_state(
        db.node.as_ref(),
        &child_doc,
        if paused { "interrupted" } else { "completed" },
    )
    .await;
    if paused {
        select_no_terminal_message(db, &child_doc).await;
    } else {
        publish_canonical_terminal_activity(
            db,
            &child_doc,
            "canonical child produced durable activity",
        )
        .await;
    }
    seed_goal_fields(
        db.node.as_ref(),
        &goal,
        &format!(
            "continuation_sequence: 1, last_continued_from_request_id: \"{}\"",
            gents::graphql::escape_graphql_string(parent_id),
        ),
    )
    .await
    .unwrap();
    (child_id, child_doc)
}

#[tokio::test]
async fn goal_source_continues_same_second_child_that_sorts_before_root() {
    for parent_id in ["graph-parent", "task-goal-request:parent"] {
        let db = test_db(&format!("goal-head-order-{}", parent_id.replace(':', "-"))).await;
        let (child_id, _) = seed_same_second_canonical_goal_child(&db, parent_id, false).await;
        let (mut controller, _snapshot_tx) = source(&db).await;
        let intent = tokio::time::timeout(Duration::from_secs(2), controller.next_fire())
            .await
            .expect("causal child must not be hidden by lexical root ordering")
            .expect("continuation intent");
        let next_id = intent
            .pre_materialized_request_id
            .expect("materialized child");
        let children = goal_children(&db).await;
        let next = children
            .iter()
            .find(|row| row.request_id == next_id)
            .unwrap();
        assert_eq!(
            next.caused_by_parent_request_id.as_deref(),
            Some(child_id.as_str())
        );
        let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.continuation_sequence(), 2);
        assert_eq!(
            goal.last_continued_from_request_id.as_deref(),
            Some(child_id.as_str())
        );
    }
}

#[tokio::test]
async fn operator_resume_accepts_same_second_child_that_sorts_before_root() {
    for parent_id in ["graph-parent", "task-goal-request:parent"] {
        let db = test_db(&format!(
            "resume-head-order-{}",
            parent_id.replace(':', "-")
        ))
        .await;
        let (child_id, child_doc) =
            seed_same_second_canonical_goal_child(&db, parent_id, true).await;
        let receipt = gents::goal::resume_goal_request(
            &ConfigAccess::Local(db.node.clone()),
            db.node_identity.as_ref(),
            db.node_identity.did(),
            SESSION,
            &child_id,
        )
        .await
        .expect("canonical child is latest despite its lexical position");
        let next = operator_resume_child_row(&db, &receipt.doc_id).await;
        assert_eq!(next["caused_by_parent_request_id"], child_id);
        assert_eq!(next["caused_by_parent_request_doc_id"], child_doc);
        let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.continuation_sequence(), 2);
        assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    }
}

async fn canonical_goal_json(db: &TestDb) -> serde_json::Value {
    serde_json::to_value(
        load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}

/// Routing's pre-claim rejection: terminal without an execution generation.
async fn reject_before_claim(db: &TestDb, doc_id: &str, reason: &str) {
    let doc_id = gents::graphql::escape_graphql_string(doc_id);
    let reason = gents::graphql::escape_graphql_string(reason);
    let response = db
        .node
        .execute(&format!(
            r#"mutation {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }}, lifecycle_state: {{ _eq: "pending" }} }},
                input: {{ lifecycle_state: "failed", failure_reason: "{reason}", terminalized_at: "{}" }}
            ) {{ _docID execution_generation }} }}"#,
            chrono::Utc::now().to_rfc3339()
        ))
        .await;
    assert!(
        !response.has_errors(),
        "reject goal child: {:?}",
        response.errors
    );
    let rows = response.data.unwrap()["update_AgentRequest"].clone();
    assert_eq!(rows.as_array().map(Vec::len), Some(1), "{rows}");
    assert!(rows[0]["execution_generation"].is_null());
}

#[tokio::test]
async fn unready_behavior_defers_goal_continuation_without_writes_or_retry_charge() {
    use gents_protocol::row::{
        BehaviorReadinessProcessState as Process, BehaviorReadinessUnavailableReason as Reason,
    };
    let db = test_db("goal-readiness-wait").await;
    seed_completed_request(&db, "parent-waits-for-readiness").await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Wait for the behavior instead of spending retries"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    publish_behavior_readiness(&db, Process::Recovering, None).await;
    publish_reconcile_phase(&db, "idle").await;
    let before = canonical_goal_json(&db).await;
    let (mut source, _tx) = unready_source(&db);

    for (process, reason, phase) in [
        (Process::Recovering, None, "idle"),
        (
            Process::Ready,
            Some(Reason::RuntimeConfigurationInvalid),
            "debouncing",
        ),
        (
            Process::Ready,
            Some(Reason::BackendTemporarilyUnavailable),
            "idle",
        ),
    ] {
        publish_behavior_readiness(&db, process, reason).await;
        publish_reconcile_phase(&db, phase).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), source.next_fire())
                .await
                .is_err(),
            "{process:?}/{reason:?}/{phase}: an unready behavior must not receive a continuation"
        );
        let waiting = canonical_goal_json(&db).await;
        assert!(
            waiting["last_failure"]
                .as_str()
                .is_some_and(|reason| reason.starts_with(gents::goal::GOAL_READINESS_WAIT_PREFIX)),
            "{process:?}/{reason:?}/{phase}: the wait must be recorded: {waiting}"
        );
        for field in [
            "status",
            "continuation_sequence",
            "infrastructure_retry_count",
            "tokens_used",
        ] {
            assert_eq!(
                waiting[field], before[field],
                "{process:?}/{reason:?}/{phase}: {field}"
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(200), source.next_fire())
                .await
                .is_err()
        );
        assert_eq!(
            canonical_goal_json(&db).await,
            waiting,
            "{process:?}/{reason:?}/{phase}: an unchanged wait must not rewrite the Goal"
        );
        assert!(goal_children(&db).await.is_empty());
    }

    publish_behavior_readiness(&db, Process::Ready, None).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out after readiness recovered")
        .expect("goal continuation intent");
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].session_id, SESSION);
    assert_eq!(
        children[0].behavior_id.as_deref(),
        Some(crate::support::AGENT_NAME)
    );
    assert_eq!(
        children[0].caused_by_parent_request_id.as_deref(),
        Some("parent-waits-for-readiness")
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.continuation_sequence(), 1);
    assert_eq!(goal.infrastructure_retry_count.unwrap_or_default(), 0);
    assert_eq!(goal.last_failure, None);

    drop(source);
    let (mut restarted, _restart_tx) = self::source(&db).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
            .await
            .is_err(),
        "restart must not duplicate the recovered continuation"
    );
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn readiness_rejected_goal_children_never_spend_the_retry_budget() {
    use gents_protocol::row::{
        BehaviorReadinessProcessState as Process, BehaviorReadinessUnavailableReason as Reason,
    };
    let db = test_db("goal-readiness-race").await;
    seed_completed_request(&db, "parent-before-race").await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Survive readiness races"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    let (mut source, _tx) = source(&db).await;

    // More consecutive races than the infrastructure retry budget allows.
    for round in 1..=(gents::goal::MAX_INFRASTRUCTURE_RETRIES as usize + 1) {
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .unwrap_or_else(|_| panic!("round {round}: goal source timed out"))
            .expect("goal continuation intent");
        let children = goal_children(&db).await;
        assert_eq!(children.len(), round);
        let pending = children
            .iter()
            .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
            .expect("pending goal child");
        assert_eq!(
            pending.input.as_ref().unwrap()["goal_continuation"]["wrapup"],
            false
        );

        publish_behavior_readiness(
            &db,
            Process::Ready,
            Some(Reason::RuntimeConfigurationInvalid),
        )
        .await;
        publish_reconcile_phase(&db, "debouncing").await;
        reject_before_claim(
            &db,
            &pending.doc_id,
            Reason::RuntimeConfigurationInvalid.public_message(),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(300), source.next_fire())
                .await
                .is_err(),
            "round {round}: a rejected child must wait for readiness"
        );
        let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.parsed_status(),
            Some(GoalStatus::Active),
            "round {round}"
        );
        assert_eq!(
            goal.infrastructure_retry_count.unwrap_or_default(),
            0,
            "round {round}"
        );
        publish_behavior_readiness(&db, Process::Ready, None).await;
        publish_reconcile_phase(&db, "idle").await;
    }

    // Readiness that still says Ready from before a rejection (lagging or
    // failing publication) must not re-issue: only a later write can.
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out after the final race")
        .expect("goal continuation intent");
    let children = goal_children(&db).await;
    let pending = children
        .iter()
        .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
        .expect("pending goal child");
    reject_before_claim(
        &db,
        &pending.doc_id,
        Reason::RuntimeConfigurationInvalid.public_message(),
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), source.next_fire())
            .await
            .is_err(),
        "stale readiness must not re-issue a rejected child"
    );
    assert_eq!(goal_children(&db).await.len(), children.len());
    let waiting = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waiting.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(waiting.infrastructure_retry_count.unwrap_or_default(), 0);
    assert!(waiting
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("not been republished since its rejection")));
    publish_behavior_readiness(&db, Process::Ready, None).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out after readiness was republished")
        .expect("goal continuation intent");
    assert_eq!(goal_children(&db).await.len(), children.len() + 1);
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(goal.infrastructure_retry_count.unwrap_or_default(), 0);
    assert_eq!(goal.last_failure, None);
}

#[tokio::test]
async fn settled_invalid_behavior_pauses_goal_with_its_reason() {
    use gents_protocol::row::{
        BehaviorReadinessProcessState as Process, BehaviorReadinessUnavailableReason as Reason,
    };
    let db = test_db("goal-readiness-settled").await;
    seed_completed_request(&db, "parent-invalid-behavior").await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Report a permanently invalid behavior"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    publish_behavior_readiness(
        &db,
        Process::Ready,
        Some(Reason::RuntimeConfigurationInvalid),
    )
    .await;
    publish_reconcile_phase(&db, "idle").await;
    let (mut source, _tx) = unready_source(&db);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Paused));
    assert_eq!(
        goal.last_failure.as_deref(),
        Some("behavior test is unavailable: runtime configuration is invalid")
    );
    assert_eq!(goal.infrastructure_retry_count.unwrap_or_default(), 0);
    assert_eq!(goal.continuation_sequence(), 0);
    assert!(goal_children(&db).await.is_empty());
}

#[tokio::test]
async fn claimed_continuation_waits_for_readiness_before_materializing() {
    use gents_protocol::row::BehaviorReadinessProcessState as Process;
    let db = test_db("goal-claimed-readiness").await;
    seed_completed_request(&db, "parent-claimed-waits").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Hold a durable claim until the behavior is ready"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    assert!(
        claim_continuation(db.node.as_ref(), &goal, "parent-claimed-waits")
            .await
            .expect("claim continuation")
    );
    publish_behavior_readiness(&db, Process::Recovering, None).await;
    let claimed = canonical_goal_json(&db).await;
    let (mut source, _tx) = unready_source(&db);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), source.next_fire())
            .await
            .is_err(),
        "a claimed child must not be published to an unready behavior"
    );
    assert!(goal_children(&db).await.is_empty());
    assert_eq!(
        canonical_goal_json(&db).await,
        claimed,
        "waiting must leave the durable claim untouched"
    );

    publish_behavior_readiness(&db, Process::Ready, None).await;
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out after readiness recovered")
        .expect("claimed continuation");
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0].caused_by_parent_request_id.as_deref(),
        Some("parent-claimed-waits")
    );
}
