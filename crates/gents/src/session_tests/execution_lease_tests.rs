use anyhow::Result;
use chrono::{DateTime, Utc};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::lifecycle::execution_lease::*;
use gents_session::request::lifecycle::test_support::{
    agent_access, agent_identity, agent_write_value, claimed_owner, claimed_owner_caused_by,
    observed_execution, owner, request_row, test_node, PendingNativeOutcome,
};
use gents_session::request::lifecycle::{
    ClaimOutcome, ExecutionOrigin, RequestLifecycle, TriggerLineage,
};
use gents_store::graphql::escape_graphql_string;
use std::{sync::Arc, time::Duration};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

struct PendingNativeObserver(Arc<tokio::sync::Notify>);

#[tokio::test]
async fn managed_agent_signs_transcript_and_plugin_effect_blocks() {
    let (node, _directory, runtime, binding) = test_node().await;
    let access = agent_access(&node, &runtime, &binding);
    let agent = gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let mut lifecycle = RequestLifecycle::materialize_pending_with_execution_binding(
        node.clone(),
        &binding.agent_did,
        runtime.clone(),
        "hello",
        60,
        ExecutionOrigin::Interactive,
        "lease-test",
        TriggerLineage::default(),
        Some(agent.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
        node.clone(),
        agent.as_ref(),
        Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        request_id: None,
        id: Some("signer-test-message".into()),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                raw_arguments: None,
                id: "signer-test-tool".into(),
                call_id: None,
                function: gents_protocol::message::ToolFunction::new(
                    "fixture_parent".into(),
                    serde_json::json!({}),
                ),
                signature: None,
                additional_params: None,
            },
        )],
    };
    let mut published = writer
        .publish_native_turn(&lifecycle, 0, 0, &message)
        .await
        .unwrap();
    let accepted = published
        .accepted_tools
        .pop()
        .expect("accepted parent tool");
    let mut tool = gents_toolcalls::call::lifecycle::ToolCallLifecycle::from_accepted(
        node.clone(),
        runtime.did().to_owned(),
        lifecycle.request().requester_did.clone(),
        accepted,
        lifecycle.claimed_deadline_at().unwrap(),
        gents_protocol::tool_call::AwaitMode::Foreground,
    )
    .unwrap();
    tool.start_running().await.unwrap();
    let effect = tool
        .admit_plugin_effect(1, "fixture_child", "{}")
        .await
        .unwrap();
    let signer = gents_store::identity::commit_signer_identity_for_did(agent.did()).unwrap();
    let node_signer = gents_store::identity::commit_signer_identity_for_did(runtime.did()).unwrap();
    for doc in [
        published.message_doc_id.as_str(),
        tool.doc_id().unwrap(),
        effect.doc_id().unwrap(),
    ] {
        let identities = commit_signer_identities(&access, doc).await;
        assert!(identities.contains(&signer), "{doc}");
        assert!(!identities.contains(&node_signer), "{doc}");
    }
    node.shutdown().await.unwrap();
}

async fn commit_signer_identities(
    access: &gents_store::config_client::ConfigAccess,
    doc_id: &str,
) -> Vec<String> {
    let response = access
        .execute(&format!(
            r#"query {{ _commits(docID: "{}") {{ cid height fieldName signature {{ identity type }} }} }}"#,
            escape_graphql_string(doc_id),
        ))
        .await
        .expect("commit history read as its Agent");
    response["data"]["_commits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|commit| commit["signature"]["identity"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn dead_revocation_accounts_tools_without_reading_corrupt_delivery_payload() {
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use gents_protocol::output::rows::{
        output_segment_create_variables, transcript_message_create_variables, AGENT_MESSAGE_FIELDS,
        AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, ToolResultPart, TranscriptMessage,
    };
    use gents_protocol::tool_call::AwaitMode;
    use gents_toolcalls::call::lifecycle::ToolCallLifecycle;

    let (node, _dir, identity, agent) = test_node().await;

    let access = agent_access(&node, &identity, &agent);
    let mut lifecycle = claimed_owner(&node, &identity, &agent).await;
    let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
        node.clone(),
        gents_store::identity::resolve_agent_identity(identity.as_ref(), &agent)
            .unwrap()
            .as_ref(),
        Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let request_doc_id = lifecycle.request().doc_id.clone();
    writer
        .start_provider_attempt(&request_doc_id, 0, 0, "inference.1".parse().unwrap())
        .await;
    let tool = |id: &str| {
        AssistantContent::ToolCall(ToolCall {
            raw_arguments: None,
            id: id.into(),
            call_id: None,
            function: ToolFunction {
                name: "echo".into(),
                arguments: serde_json::json!({"value": id}),
            },
            signature: None,
            additional_params: None,
        })
    };
    let published = writer
        .publish_native_turn(
            &lifecycle,
            0,
            0,
            &Message::Assistant {
                request_id: None,
                id: None,
                content: vec![tool("pending-call"), tool("running-call")],
            },
        )
        .await
        .unwrap();
    let mut accepted = published.accepted_tools.into_iter();
    let pending = accepted.next().unwrap();
    let running = accepted.next().unwrap();
    let mut running_lifecycle = ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        running.clone(),
        Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Foreground,
    )
    .unwrap();
    running_lifecycle.start_running().await.unwrap();

    // Physically valid delivery header with an immutably corrupt selected
    // tool-output extent; revocation must never read or rewrite these bytes.
    let now = Utc::now().to_rfc3339();
    let payload = "durable-but-corrupt";
    let segment = OutputSegment {
        node_did: lifecycle.request().node_did.clone(),
        requester_did: lifecycle.request().requester_did.clone(),
        session_id: lifecycle.request().session_id.clone(),
        request_doc_id: request_doc_id.clone(),
        source: OutputSource::ToolCall {
            tool_call_doc_id: running.tool_call_doc_id.clone(),
        },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: running.tool_call_doc_id.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: u32::try_from(payload.len()).unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }],
        payload: payload.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![u64::try_from(payload.len()).unwrap() + 1],
        }),
        created_at: now.clone(),
    };
    let response = agent_write_value(
        &access,
        "test.canonical_fixture_write",
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        output_segment_create_variables(&segment).unwrap(),
    )
    .await;
    let close_doc_id =
        gents_store::graphql::created_doc_id(&response, "AgentOutputSegment").unwrap();
    let delivery = TranscriptMessage {
        native_request_id: None,
        message_key: "corrupt-running-delivery".into(),
        session_id: lifecycle.request().session_id.clone(),
        node_did: identity.did().to_owned(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.clone()),
        publication: MessagePublication::ToolDelivery {
            tool_call_doc_id: running.tool_call_doc_id.clone(),
        },
        outcome: OutputOutcome::Complete,
        sequence: published.sequence + 1,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::ToolResult {
            tool_call_doc_id: running.tool_call_doc_id.clone(),
            id: running.id.clone(),
            call_id: running.call_id.clone(),
            parts: vec![ToolResultPart::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
        }],
        created_at: now,
    };
    agent_write_value(
        &access,
        "test.canonical_fixture_write",
        CREATE_AGENT_MESSAGE_MUTATION,
        transcript_message_create_variables(&delivery).unwrap(),
    )
    .await;

    let facts_query = format!(
        r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
        escape_graphql_string(&request_doc_id),
        escape_graphql_string(&request_doc_id),
    );
    let facts_before = access
        .execute(&facts_query)
        .await
        .expect("load pre-revocation canonical facts");
    let observed = observed_execution(&access, &request_doc_id).await;
    assert_eq!(
        revoke_execution_preserving_output_with_access(
            &access,
            &observed,
            RequestTerminalOutcome::Dead,
            "corrupt delivery",
        )
        .await
        .unwrap(),
        TerminalizeResult::Won
    );
    let facts_after = access
        .execute(&facts_query)
        .await
        .expect("load post-revocation canonical facts");
    assert_eq!(
        facts_after, facts_before,
        "revocation must preserve corrupt facts"
    );

    let response = access
        .execute(&format!(r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }} }}, order: {{ message_sequence: ASC }}) {{ _docID lifecycle_state cancel_cause stuck_since tool_failure_class }} }}"#,
        escape_graphql_string(&request_doc_id),
    ))
        .await
        .expect("read as its Agent");
    let rows = response["data"]["AgentToolCall"].as_array().unwrap();
    let row = |doc_id: &str| rows.iter().find(|row| row["_docID"] == doc_id).unwrap();
    assert_eq!(
        row(&pending.tool_call_doc_id)["lifecycle_state"],
        "cancelled"
    );
    assert_eq!(
        row(&pending.tool_call_doc_id)["cancel_cause"],
        "interrupted"
    );
    assert!(row(&pending.tool_call_doc_id)["tool_failure_class"].is_null());
    assert_eq!(row(&running.tool_call_doc_id)["lifecycle_state"], "running");
    assert!(row(&running.tool_call_doc_id)["stuck_since"].is_string());
    assert!(row(&running.tool_call_doc_id)["cancel_cause"].is_null());
    assert!(row(&running.tool_call_doc_id)["tool_failure_class"].is_null());
}

#[derive(serde::Deserialize)]
struct NativeCompletionRenewalCase {
    name: String,
    periodic_renewal: bool,
    initial: NativeCompletionRenewalInitial,
    events: Vec<String>,
    observations: Vec<NativeCompletionRenewalObservation>,
}

#[derive(serde::Deserialize)]
struct NativeCompletionRenewalInitial {
    cleanup_required: bool,
    native_outstanding: usize,
    gate_held: bool,
}

#[derive(serde::Deserialize)]
struct NativeCompletionRenewalObservation {
    renewal_attempt_returned: bool,
    next_renewal_started: bool,
    gate_held: bool,
}

#[tokio::test]
async fn lean_periodic_renewal_waits_for_abandoned_native_operation_before_next_attempt() {
    let cases: Vec<NativeCompletionRenewalCase> = serde_json::from_value(serde_json::Value::from(
        tokio::task::spawn_blocking(|| {
            gents_lean_vocab::lean_contract_snapshot()
                .native_completion_cases
                .clone()
        })
        .await
        .expect("Lean snapshot load"),
    ))
    .unwrap();
    let cases: Vec<_> = cases
        .into_iter()
        .filter(|case| case.periodic_renewal)
        .collect();
    assert_eq!(cases.len(), 2, "Lean owns both renewal completion cases");

    for case in cases {
        assert!(case.initial.cleanup_required, "{}", case.name);
        assert_eq!(case.initial.native_outstanding, 0, "{}", case.name);
        assert!(!case.initial.gate_held, "renewal bypasses mutation gate");
        assert_eq!(case.events.len(), case.observations.len(), "{}", case.name);

        let (node, _dir, identity, agent) = test_node().await;

        let access = agent_access(&node, &identity, &agent);
        let mut lifecycle = owner(&node, &identity, &agent).await;
        lifecycle.drop_renewal_task();
        let doc_id = lifecycle.request().doc_id.clone();
        let row = request_row(&access, &doc_id).await;
        let generation = row
            .execution_generation
            .clone()
            .expect("active execution generation");
        let deadline = DateTime::parse_from_rfc3339(
            row.execution_lease_expires_at
                .as_deref()
                .expect("active execution deadline"),
        )
        .unwrap()
        .with_timezone(&Utc);

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let pending_native = Arc::new(tokio::sync::Notify::new());
        let subscriber = tracing::Dispatch::new(
            Registry::default().with(PendingNativeObserver(Arc::clone(&pending_native))),
        );
        let access_for_attempt = access.clone();
        let request_for_attempt = doc_id.clone();
        let generation_for_attempt = generation.clone();
        let mut first_attempt = tokio::spawn(
            gents_store::config_client::ConfigApplyTxn::with_native_operation_pause(
                Arc::clone(&entered),
                Arc::clone(&release),
                async move {
                    gents_session::request::lifecycle::renew_execution_lease_once_at_with_access(
                        &access_for_attempt,
                        &request_for_attempt,
                        &generation_for_attempt,
                        deadline,
                        Utc::now(),
                    )
                    .await
                },
            )
            .with_subscriber(subscriber),
        );

        let mut first_result = None;
        for (event, observed) in case.events.iter().zip(&case.observations) {
            match event.as_str() {
                "native_started" => {
                    tokio::time::timeout(Duration::from_secs(10), entered.notified())
                        .await
                        .expect("renewal admitted one native operation");
                }
                "caller_timed_out" => {
                    tokio::time::pause();
                    tokio::time::advance(
                        gents_store::config_client::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                    )
                    .await;
                    tokio::time::resume();
                    tokio::time::timeout(Duration::from_secs(10), pending_native.notified())
                        .await
                        .expect("transaction owner records native work as pending before release");
                }
                "native_returned" => {
                    release.notify_one();
                    first_result = Some(
                        tokio::time::timeout(Duration::from_secs(10), &mut first_attempt)
                            .await
                            .expect("renewal owner drains after native completion")
                            .expect("renewal attempt task"),
                    );
                }
                "renewal_attempt_returned" => {
                    if !observed.renewal_attempt_returned && first_result.is_none() {
                        if let Ok(result) =
                            tokio::time::timeout(Duration::from_millis(50), &mut first_attempt)
                                .await
                        {
                            first_result = Some(result.expect("renewal attempt task"));
                        }
                    }
                    let actually_returned = first_result.is_some() || first_attempt.is_finished();
                    if actually_returned != observed.renewal_attempt_returned {
                        release.notify_one();
                        panic!(
                            "{}: renewal return observation diverged at {event}",
                            case.name
                        );
                    }
                    assert_eq!(
                        actually_returned, observed.renewal_attempt_returned,
                        "{}: {event}",
                        case.name
                    );
                }
                "renewal_requested" => {
                    let actually_returned = first_result.is_some() || first_attempt.is_finished();
                    assert_eq!(
                        actually_returned, observed.renewal_attempt_returned,
                        "{}: next renewal cannot be admitted while the prior attempt is pending",
                        case.name
                    );
                    if observed.next_renewal_started {
                        assert!(first_result.is_some(), "{}", case.name);
                        let current = request_row(&access, &doc_id).await;
                        let next_deadline = DateTime::parse_from_rfc3339(
                            current
                                .execution_lease_expires_at
                                .as_deref()
                                .expect("active execution deadline after rollback"),
                        )
                        .unwrap()
                        .with_timezone(&Utc);
                        let next =
                            gents_session::request::lifecycle::renew_execution_lease_once_at_with_access(
                                &access,
                                &doc_id,
                                &generation,
                                next_deadline,
                                Utc::now(),
                            )
                            .await
                            .unwrap();
                        assert!(
                            matches!(
                                next,
                                gents_session::request::lifecycle::RenewalAttemptOutcome::Skipped
                                    | gents_session::request::lifecycle::RenewalAttemptOutcome::Committed
                            ),
                            "{}: fresh renewal runs after the prior native call drains",
                            case.name
                        );
                    }
                }
                other => panic!("unexpected Lean native-completion event: {other}"),
            }
            assert!(!observed.gate_held, "renewal remains outside mutation gate");
        }

        if first_result.is_none() {
            release.notify_one();
            first_result = Some(
                tokio::time::timeout(Duration::from_secs(10), &mut first_attempt)
                    .await
                    .expect("renewal owner drains after native completion")
                    .expect("renewal attempt task"),
            );
        }
        let first_error = first_result
            .take()
            .expect("native completion releases the owner")
            .expect_err("the timed-out attempt must not replay or commit");
        assert!(
            gents_store::config_client::is_transaction_step_unavailable(&first_error),
            "{}: abandoned native renewal remains an availability failure",
            case.name
        );
    }
}

#[tokio::test]
async fn latest_only_supersession_settles_parked_and_resuming_fires() {
    use gents_session::request::lifecycle::backend_wait::{wake_parked_execution, WakeResult};
    for resuming in [false, true] {
        let (node, _db, identity, agent) = test_node().await;
        let access = agent_access(&node, &identity, &agent);
        let mut lifecycle = claimed_owner_caused_by(
            &node,
            &identity,
            &agent,
            TriggerLineage {
                trigger_id: Some("latest".to_owned()),
                ..Default::default()
            },
        )
        .await;
        let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
            node.clone(),
            agent_identity(&identity, &agent).as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let doc_id = lifecycle.request().doc_id.clone();
        let node_did = lifecycle.request().node_did.clone();
        assert!(lifecycle.park_for_backend().await.unwrap());
        if resuming {
            assert!(matches!(
                wake_parked_execution(
                    &access,
                    &doc_id,
                    "lease-test",
                    Some(chrono::Utc::now()),
                    chrono::Utc::now(),
                )
                .await
                .unwrap(),
                WakeResult::Resumed { .. }
            ));
        }
        let owner = &node_did;
        let superseded = access
            .transact_write("test.supersede_parked_fire", move |txn| {
                Box::pin(async move {
                    gents_automation::triggers::engine::durable::supersede_trigger_requests_in_txn(
                        txn, owner, "latest", None,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        assert_eq!(superseded, 1, "resuming={resuming}");
        let row = request_row(&access, &doc_id).await;
        assert_eq!(
            row.lifecycle_state,
            Some(RequestLifecycleState::Superseded),
            "resuming={resuming}"
        );
        assert!(row.terminalized_at.is_some());
        assert_eq!(
            wake_parked_execution(
                &access,
                &doc_id,
                "lease-test",
                Some(chrono::Utc::now()),
                chrono::Utc::now(),
            )
            .await
            .unwrap(),
            WakeResult::Lost,
            "a superseded fire must not resume"
        );
    }
}

impl<S> Layer<S> for PendingNativeObserver
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: LayerContext<'_, S>) {
        if event.metadata().target()
            != gents_store::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET
        {
            return;
        }
        let mut outcome = PendingNativeOutcome::default();
        event.record(&mut outcome);
        if outcome.0 {
            self.0.notify_one();
        }
    }
}

mod request_row_writers;

mod workspace_recovery;
