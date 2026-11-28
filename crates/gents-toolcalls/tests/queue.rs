use gents_protocol::message::Message;
use gents_session::request::lifecycle::queue::*;
use gents_session::request::lifecycle::ExecutionOrigin;
use gents_session::test_support::AgentHome;
use gents_store::graphql::escape_graphql_string;

use gents_session::request::lifecycle::RequestTerminalOutcome;
use gents_toolcalls::test_support::background_completion::background_hints;

use gents_toolcalls::test_support::queue::*;

#[tokio::test]
async fn notification_is_atomically_bound_to_coalesced_wake() {
    let db = AgentHome::start("atomic-background-notification").await;
    let mut fixture = canonical_background_fixture(&db, "atomic-background-session").await;
    let parent = fixture.parent.clone();
    let first_tool = fixture
        .prepare_notification("first notification")
        .await
        .unwrap();
    let second_tool = fixture
        .prepare_notification("second notification")
        .await
        .unwrap();
    let selection = fixture.writer.terminal_output(&parent.doc_id).await;
    fixture
        .lifecycle
        .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
        .await
        .unwrap();
    let first = fixture
        .publish_prepared_notification(
            "first notification",
            "background-completion-notification:first:tool",
            "review notifications",
            background_hints(&parent),
            None,
            &first_tool,
        )
        .await
        .unwrap();
    let second = fixture
        .publish_prepared_notification(
            "second notification",
            "background-completion-notification:second:tool",
            "review notifications",
            background_hints(&parent),
            None,
            &second_tool,
        )
        .await
        .unwrap();
    assert!(first.created_request);
    assert!(!second.created_request);
    assert_eq!(
        first.request.as_ref().expect("non-Goal wake").doc_id,
        second.request.as_ref().expect("non-Goal wake").doc_id
    );

    let wake_query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
            }}
        }}"#,
        escape_graphql_string(&first.request.as_ref().expect("non-Goal wake").doc_id)
    );
    let wake_response = db.agent_access.execute(&wake_query).await.unwrap();
    let wake = &wake_response["data"]["AgentRequest"][0];
    assert_eq!(
        wake["caused_by_parent_request_id"].as_str(),
        Some(parent.request_id.as_str())
    );
    assert_eq!(
        wake["caused_by_parent_request_doc_id"].as_str(),
        Some(parent.doc_id.as_str())
    );

    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{}" }}, message_key: {{ _like: "background-completion-notification:%:tool" }} }},
                order: {{ sequence: ASC }}
            ) {{ request_doc_id }}
        }}"#,
        escape_graphql_string(&parent.session_id)
    );
    let response = db.agent_access.execute(&query).await.unwrap();
    let rows = response["data"]["AgentMessage"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(
            row["request_doc_id"].as_str(),
            Some(
                first
                    .request
                    .as_ref()
                    .expect("non-Goal wake")
                    .doc_id
                    .as_str()
            )
        );
    }
}

#[tokio::test]
async fn duplicate_notification_key_recovers_its_original_wake_binding() {
    let db = AgentHome::start("atomic-background-idempotent-notification").await;
    let mut fixture =
        canonical_background_fixture(&db, "atomic-background-idempotent-notification-session")
            .await;
    let parent = fixture.parent.clone();
    let message_key = "background-completion-notification:idempotent:tool";
    let tool = fixture
        .prepare_notification("idempotent notification")
        .await
        .unwrap();
    let selection = fixture.writer.terminal_output(&parent.doc_id).await;
    fixture
        .lifecycle
        .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
        .await
        .unwrap();
    let first = fixture
        .publish_prepared_notification(
            "idempotent notification",
            message_key,
            "review notifications",
            background_hints(&parent),
            None,
            &tool,
        )
        .await
        .unwrap();
    let retry = fixture
        .publish_prepared_notification(
            "idempotent notification",
            message_key,
            "review notifications",
            background_hints(&parent),
            None,
            &tool,
        )
        .await
        .unwrap();

    assert_eq!(
        retry.request.as_ref().expect("non-Goal wake").doc_id,
        first.request.as_ref().expect("non-Goal wake").doc_id
    );
    assert_eq!(
        retry.request.as_ref().expect("non-Goal wake").request_id,
        first.request.as_ref().expect("non-Goal wake").request_id
    );
    assert_eq!(
        retry.request.as_ref().expect("non-Goal wake").session_id,
        first.request.as_ref().expect("non-Goal wake").session_id
    );
    assert_eq!(retry.message_sequence, first.message_sequence);
    assert!(!retry.created_request);

    let conflict = fixture
        .publish_prepared_notification(
            "changed notification",
            message_key,
            "review notifications",
            background_hints(&parent),
            None,
            &tool,
        )
        .await
        .unwrap_err();
    assert!(
        conflict
            .to_string()
            .contains("replay conflicts with authority, scope, or content"),
        "unexpected conflict: {conflict:#}"
    );
}

#[tokio::test]
async fn many_concurrent_notifications_publish_one_wake_identity() {
    let db = AgentHome::start("atomic-background-many-race").await;
    let mut fixture =
        canonical_background_fixture(&db, "atomic-background-many-race-session").await;
    let parent = fixture.parent.clone();
    let mut tools = Vec::new();
    for index in 0..16 {
        tools.push(
            fixture
                .prepare_notification(&format!("concurrent notification {index}"))
                .await
                .unwrap(),
        );
    }
    let selection = fixture.writer.terminal_output(&parent.doc_id).await;
    fixture
        .lifecycle
        .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
        .await
        .unwrap();
    let enqueued = futures::future::join_all(tools.iter().enumerate().map(|(index, tool)| {
        let fixture = &fixture;
        let parent = &parent;
        async move {
            fixture
                .publish_prepared_notification(
                    &format!("concurrent notification {index}"),
                    &format!("background-completion-notification:many-race-{index}:tool"),
                    "review notifications",
                    background_hints(parent),
                    None,
                    tool,
                )
                .await
                .unwrap()
        }
    }))
    .await;

    let wake_doc_id = &enqueued[0].request.as_ref().expect("non-Goal wake").doc_id;
    assert!(enqueued
        .iter()
        .all(|result| result.request.as_ref().expect("non-Goal wake").doc_id == *wake_doc_id));
    assert_eq!(
        enqueued
            .iter()
            .filter(|result| result.created_request)
            .count(),
        1
    );
}

#[tokio::test]
async fn background_wake_identity_notifier_traces() {
    use gents_lean_vocab::tla::replay::{Red, Trace};
    use gents_store::test_support::paused_transaction::PausedTransaction;
    use rapidhash::RapidHashMap;
    for json in [
        include_str!("../../gents/proofs/tla/generated/traces/MCBackgroundWakeIdentityProduction.sample-1.json"),
        include_str!("../../gents/proofs/tla/generated/traces/MCBackgroundWakeIdentityNoGate.json"),
        include_str!(
            "../../gents/proofs/tla/generated/traces/MCBackgroundWakeIdentityNoGateNoConflict.json"
        ),
        include_str!(
            "../../gents/proofs/tla/generated/traces/MCBackgroundWakeIdentityWallClock.json"
        ),
    ] {
        let trace = Trace::load(json, "BackgroundWakeIdentity", &[]);
        let home = AgentHome::start("wake-identity").await;
        let mut fixture = canonical_background_fixture(&home, "wake-identity").await;
        let mut observation_time = fixture.claimed_at;
        let mut tools = Vec::new();
        let mut published = RapidHashMap::default();
        let mut writes = RapidHashMap::default();
        let mut reading = None;
        let mut actions = trace.steps.iter().peekable();
        while let Some(step) =
            actions.next_if(|step| matches!(step.action.as_str(), "Prepare" | "Tick"))
        {
            match step.action.as_str() {
                "Tick" => {
                    let bound =
                        fixture.lifecycle.claimed_deadline_at().unwrap() - fixture.claimed_at;
                    let ticks = trace.constants["Deadline"].as_i64().unwrap();
                    let progress = step.after["clock"].as_i64().unwrap();
                    observation_time = fixture.claimed_at
                        + bound * i32::try_from(progress).unwrap() / i32::try_from(ticks).unwrap();
                }
                "Prepare" => tools.push(
                    fixture
                        .prepare_notification_at("notification", observation_time)
                        .await
                        .unwrap(),
                ),
                _ => unreachable!(),
            }
        }
        if trace.config == "MCBackgroundWakeIdentityWallClock" {
            assert!(observation_time > fixture.lifecycle.claimed_deadline_at().unwrap());
            trace.check(Red::Defect, &[("SetupNotRefused", !tools.is_empty())]);
            continue;
        }
        let selection = fixture.writer.terminal_output(&fixture.parent.doc_id).await;
        fixture
            .lifecycle
            .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
            .await
            .unwrap();
        for step in actions {
            let actor = step.actor.as_ref().unwrap()["$model"].as_str().unwrap();
            if published.contains_key(actor) {
                continue;
            }
            match step.action.as_str() {
                "Enqueue" => {
                    let i: usize = actor.strip_prefix('n').unwrap().parse().unwrap();
                    let fixture = &fixture;
                    let tool = &tools[i - 1];
                    let call = PausedTransaction::before_commit(async move {
                        fixture
                            .publish_prepared_notification(
                                "notification",
                                &format!("background-completion-notification:trace-{i}:tool"),
                                "review notifications",
                                background_hints(&fixture.parent),
                                None,
                                tool,
                            )
                            .await
                    });
                    assert!(writes.insert(actor, call).is_none());
                }
                "Read" if reading.is_none() => {
                    assert!(writes.get_mut(actor).unwrap().advance().await);
                    reading = Some(actor);
                }
                "Read" => assert!(futures::poll!(std::pin::pin!(writes
                    .get_mut(actor)
                    .unwrap()
                    .advance()))
                .is_pending()),
                "Commit" => {
                    let call = writes.remove(actor).unwrap();
                    assert!(reading.is_none() || reading == Some(actor));
                    published.insert(actor, call.finish().await.unwrap());
                    reading = None;
                }
                action => panic!("unmapped wake action {action}"),
            }
        }
        assert!(writes.is_empty());
        let unique = published.values().filter(|p| p.created_request).count();
        let first = published.values().next().unwrap().request.as_ref().unwrap();
        trace.check(
            Red::Defect,
            &[
                ("NoneFailed", !published.is_empty()),
                (
                    "OneWake",
                    unique == 1
                        && published
                            .values()
                            .all(|p| p.request.as_ref().unwrap().doc_id == first.doc_id),
                ),
            ],
        );
    }
}

#[tokio::test]
async fn restart_before_claim_preserves_pending_input_until_the_wake_completes() {
    let db = AgentHome::start("background-restart-before-claim").await;
    let node = db.node.clone();
    let mut fixture =
        canonical_background_fixture(&db, "background-restart-before-claim-session").await;
    let parent = fixture.parent.clone();
    let tool = fixture
        .prepare_notification("restart-safe notification")
        .await
        .unwrap();
    let selection = fixture.writer.terminal_output(&parent.doc_id).await;
    fixture
        .lifecycle
        .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
        .await
        .unwrap();
    let enqueued = fixture
        .publish_prepared_notification(
            "restart-safe notification",
            "background-completion-notification:restart-before-claim:tool",
            "review notifications",
            background_hints(&parent),
            None,
            &tool,
        )
        .await
        .unwrap();

    let recovery =
        gents_session::request::lifecycle::RequestLifecycle::recover_all_with_identities(
            &node,
            db.node_did(),
            std::slice::from_ref(&db.agent_identity),
        )
        .await
        .unwrap();
    assert_eq!(recovery.background_wakes_redriven, 0);
    let before = gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
        &db.agent_access,
        db.node_did(),
    )
    .await
    .unwrap();
    assert_eq!(before.pending_notifications, 1);
    assert_eq!(before.acknowledged_notifications, 0);
    assert_eq!(before.epochs[0].state, "pending");

    let wake_doc_id = enqueued
        .request
        .as_ref()
        .expect("non-Goal wake")
        .doc_id
        .clone();
    let mut lifecycle =
        gents_session::test_support::ClaimedRequest::existing(&db, &db.binding, &wake_doc_id)
            .origin(ExecutionOrigin::Scheduled)
            .backend("backend-test")
            .batch_interval(std::time::Duration::ZERO)
            .drive(gents_session::test_support::Stage::Executing)
            .await
            .lifecycle;
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let after = gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
        &db.agent_access,
        db.node_did(),
    )
    .await
    .unwrap();
    assert_eq!(after.pending_notifications, 0);
    assert_eq!(after.acknowledged_notifications, 1);
    assert_eq!(after.epochs[0].state, "acknowledged");
}

#[tokio::test]
async fn canonical_terminal_commit_makes_acknowledgement_restart_atomic() {
    let db = AgentHome::start("background-response-repair-ack").await;
    let node = db.node.clone();
    let mut fixture =
        canonical_background_fixture(&db, "background-response-repair-ack-session").await;
    let parent = fixture.parent.clone();
    let tool = fixture
        .prepare_notification("response-persisted notification")
        .await
        .unwrap();
    let selection = fixture.writer.terminal_output(&parent.doc_id).await;
    fixture
        .lifecycle
        .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
        .await
        .unwrap();
    let enqueued = fixture
        .publish_prepared_notification(
            "response-persisted notification",
            "background-completion-notification:response-persisted:tool",
            "review notifications",
            background_hints(&parent),
            None,
            &tool,
        )
        .await
        .unwrap();

    let wake_doc_id = enqueued
        .request
        .as_ref()
        .expect("non-Goal wake")
        .doc_id
        .clone();
    let claimed =
        gents_session::test_support::ClaimedRequest::existing(&db, &db.binding, &wake_doc_id)
            .origin(ExecutionOrigin::Scheduled)
            .backend("backend-test")
            .batch_interval(std::time::Duration::ZERO)
            .drive(gents_session::test_support::Stage::Executing)
            .await;
    let mut lifecycle = claimed.lifecycle;
    let writer = claimed.writer.expect("executing wake has its writer");
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            gents_protocol::rendered_request::CaptureScope {
                kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                seq: 0,
            },
        )
        .await;
    let published = writer
        .publish_native_turn(
            &lifecycle,
            0,
            0,
            &Message::assistant("integrated notification"),
        )
        .await
        .unwrap();
    let unpublished_terminal =
        gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
            &db.agent_access,
            db.node_did(),
        )
        .await
        .unwrap();
    assert_eq!(unpublished_terminal.pending_notifications, 1);
    assert_eq!(unpublished_terminal.acknowledged_notifications, 0);
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: published.message_doc_id.clone(),
            },
            None,
        )
        .await
        .unwrap();

    let terminal = db.agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state terminal_output }} }}"#,
            escape_graphql_string(&lifecycle.request().doc_id),
        ))
        .await.unwrap();
    let terminal_row = &terminal["data"]["AgentRequest"][0];
    assert_eq!(terminal_row["lifecycle_state"], "completed");
    assert_eq!(
        terminal_row["terminal_output"],
        serde_json::to_value(gents_protocol::output::TerminalOutput::Message {
            message_doc_id: published.message_doc_id,
        })
        .unwrap()
    );

    let first_repair =
        gents_session::request::lifecycle::RequestLifecycle::repair_terminal_requests_with_identities(
            &node,
            db.node_did(),
            std::slice::from_ref(&db.agent_identity),
        )
        .await
        .unwrap();
    assert_eq!(first_repair.repaired, 0);
    let first = gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
        &db.agent_access,
        db.node_did(),
    )
    .await
    .unwrap();
    assert_eq!(first.pending_notifications, 0);
    assert_eq!(first.acknowledged_notifications, 1);
    assert_eq!(first.epochs[0].state, "acknowledged");

    let second_repair =
        gents_session::request::lifecycle::RequestLifecycle::repair_terminal_requests_with_identities(
            &node,
            db.node_did(),
            std::slice::from_ref(&db.agent_identity),
        )
        .await
        .unwrap();
    assert_eq!(second_repair.repaired, 0);
    let second = gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
        &db.agent_access,
        db.node_did(),
    )
    .await
    .unwrap();
    assert_eq!(
        second, first,
        "acknowledgement projection must be restart-idempotent"
    );
}

struct PublishedWake {
    db: gents_session::test_support::AgentHome,
    session_id: String,
    wake_doc_id: String,
}

async fn published_wake(name: &str) -> PublishedWake {
    let db = AgentHome::start(name).await;
    let session_id = format!("{name}-session");
    let mut fixture = canonical_background_fixture(&db, &session_id).await;
    let parent = fixture.parent.clone();
    let tool = fixture.prepare_notification(name).await.unwrap();
    let selection = fixture.writer.terminal_output(&parent.doc_id).await;
    fixture
        .lifecycle
        .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
        .await
        .unwrap();
    let enqueued = fixture
        .publish_prepared_notification(
            name,
            &format!("background-completion-notification:{name}:tool"),
            "review notifications",
            background_hints(&parent),
            None,
            &tool,
        )
        .await
        .unwrap();
    PublishedWake {
        db,
        session_id,
        wake_doc_id: enqueued.request.expect("non-Goal wake").doc_id,
    }
}

impl PublishedWake {
    async fn claim_and_begin(&self) -> gents_session::request::lifecycle::RequestLifecycle {
        gents_session::test_support::ClaimedRequest::existing(
            &self.db,
            &self.db.binding,
            &self.wake_doc_id,
        )
        .origin(ExecutionOrigin::Scheduled)
        .backend("backend-test")
        .batch_interval(std::time::Duration::ZERO)
        .drive(gents_session::test_support::Stage::Executing)
        .await
        .lifecycle
    }

    async fn restart(&self) {
        gents_session::request::lifecycle::RequestLifecycle::recover_all_with_identities(
            &self.db.node,
            self.db.node_did(),
            std::slice::from_ref(&self.db.agent_identity),
        )
        .await
        .unwrap();
    }

    async fn observe(
        &self,
        session_key: &str,
    ) -> (String, u64, u64, Option<String>, Option<String>) {
        let response = self
            .db
            .agent_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state background_completion_notification_keys_json }} }}"#,
                escape_graphql_string(&self.wake_doc_id),
            ))
            .await
            .unwrap();
        let row = &response["data"]["AgentRequest"][0];
        let attempted = row["background_completion_notification_keys_json"]
            .as_str()
            .map(|encoded| {
                gents_session::request::lifecycle::notification_receipt_keys(encoded)
                    .unwrap()
                    .len() as u64
            })
            .unwrap_or_default();
        let diagnostics =
            gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
                &self.db.agent_access,
                self.db.node_did(),
            )
            .await
            .unwrap();
        assert_eq!(diagnostics.epochs.len(), 1);
        let epoch = &diagnostics.epochs[0];
        let wake = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
            &self.db.agent_access,
            &self.wake_doc_id,
        )
        .await
        .unwrap()
        .unwrap();
        let queue = wake.input.queue.expect("wake queue");
        assert_eq!(queue.key.as_deref(), Some(epoch.coalescing_key.as_str()));
        (
            row["lifecycle_state"].as_str().unwrap().to_owned(),
            attempted,
            epoch.acknowledged_notification_keys.len() as u64,
            serde_json::to_value(queue.source)
                .unwrap()
                .as_str()
                .map(str::to_owned),
            queue
                .key
                .as_deref()
                .and_then(|key| key.strip_suffix(self.session_id.as_str()))
                .map(|prefix| format!("{prefix}{session_key}")),
        )
    }
}

#[tokio::test]
async fn claimed_wake_terminal_state_acknowledges_its_claim_snapshot() {
    for case in gents_lean_vocab::families::contracts_r6_acknowledgement::cases() {
        let input = &case.input;
        let outcome = match input.terminal_state.as_str() {
            "completed" => RequestTerminalOutcome::Completed,
            "failed" => RequestTerminalOutcome::Failed,
            other => panic!("{}: no terminal outcome for {other}", input.name),
        };
        let wake = published_wake(&input.name).await;
        let mut lifecycle = wake.claim_and_begin().await;
        lifecycle
            .terminalize_owned(
                outcome,
                gents_protocol::output::TerminalOutput::NoMessage,
                Some(input.name.as_str()),
            )
            .await
            .unwrap();
        let (terminal_state, attempted, acknowledged, queue_source, queue_key) =
            wake.observe(&input.session_key).await;
        assert_eq!(terminal_state, input.terminal_state, "{}", input.name);
        case.check(
            gents_lean_vocab::families::contracts_r6_acknowledgement::DeliveryOutput {
                attempted,
                acknowledged,
                queue_source,
                queue_key,
            },
        );
    }
}

#[tokio::test]
async fn wake_delivery_restart_boundaries_project_the_claim_snapshot() {
    for case in gents_lean_vocab::families::contracts_r6_crash_boundary::cases() {
        let input = &case.input;
        let wake = published_wake(&input.name).await;
        let lifecycle = match input.point.as_str() {
            "beforeClaim" => None,
            "duringInference" => Some(wake.claim_and_begin().await),
            "duringAcknowledgement" => {
                let mut lifecycle = wake.claim_and_begin().await;
                lifecycle
                    .terminalize_owned(
                        RequestTerminalOutcome::Completed,
                        gents_protocol::output::TerminalOutput::NoMessage,
                        None,
                    )
                    .await
                    .unwrap();
                Some(lifecycle)
            }
            other => panic!("{}: no restart point {other}", input.name),
        };
        wake.restart().await;
        let first = wake.observe(&input.session_key).await;
        wake.restart().await;
        assert_eq!(
            wake.observe(&input.session_key).await,
            first,
            "{}: a second restart leaves the projection unchanged",
            input.name
        );
        drop(lifecycle);
        let (terminal_state, attempted, acknowledged, queue_source, queue_key) = first;
        case.check(
            gents_lean_vocab::families::contracts_r6_crash_boundary::CrashOutput {
                terminal_state,
                attempted,
                acknowledged,
                queue_source,
                queue_key,
            },
        );
    }
}

#[tokio::test]
async fn a_lower_hop_wake_never_consumes_a_higher_hop_notification() {
    let db = AgentHome::start("wake-hop-raise").await;
    let mut fixture = canonical_background_fixture(&db, "wake-hop-raise-session").await;
    let parent = fixture.parent.clone();
    let native_tool = fixture
        .prepare_notification("native process done")
        .await
        .unwrap();
    let raised_tool = fixture.prepare_notification("agent result").await.unwrap();
    let later_tool = fixture
        .prepare_notification("another native process done")
        .await
        .unwrap();
    let selection = fixture.writer.terminal_output(&parent.doc_id).await;
    fixture
        .lifecycle
        .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
        .await
        .unwrap();
    let native = fixture
        .publish_prepared_notification(
            "native process done",
            "background-completion-notification:native:tool",
            "review notifications",
            background_hints(&parent),
            None,
            &native_tool,
        )
        .await
        .unwrap();
    let native_wake = native.request.expect("native wake").doc_id;
    let raised = persist_background_completion_with_message_canonical_with_access(
        &db.agent_access,
        &db.node,
        &parent,
        gents_session::request::lifecycle::queue::BackgroundCompletionNotification {
            content: "agent result",
            message_key: "background-completion-notification:agent:tool",
            wake_content: "review notifications",
            queue: background_hints(&parent),
            existing_notification_doc_id: None,
            wake: gents_protocol::request_lifecycle::RequestHopCause::CrossSession { cause_hop: 1 },
        },
        &raised_tool,
    )
    .await
    .unwrap();
    assert!(raised.created_request);
    let raised_wake = raised.request.expect("raised wake").doc_id;
    assert_ne!(raised_wake, native_wake);
    let later = fixture
        .publish_prepared_notification(
            "another native process done",
            "background-completion-notification:native-2:tool",
            "review notifications",
            background_hints(&parent),
            None,
            &later_tool,
        )
        .await
        .unwrap();
    assert!(!later.created_request);
    assert_eq!(later.request.expect("joined wake").doc_id, raised_wake);

    let response = db
        .agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ _docID lifecycle_state request_hop superseded_by_request_doc_id }} }}"#,
            escape_graphql_string(&parent.session_id)
        ))
        .await.unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap().clone();
    let pending = rows
        .iter()
        .filter(|row| row["lifecycle_state"] == "pending")
        .collect::<Vec<_>>();
    assert_eq!(pending.len(), 1, "{rows:?}");
    assert_eq!(pending[0]["_docID"], raised_wake.as_str());
    assert_eq!(pending[0]["request_hop"], 2);
    let lower = rows
        .iter()
        .find(|row| row["_docID"] == native_wake.as_str())
        .unwrap();
    assert_eq!(lower["lifecycle_state"], "superseded");
    assert_eq!(lower["superseded_by_request_doc_id"], raised_wake.as_str());
}

#[tokio::test]
async fn a_returned_result_keeps_the_callers_hop_until_the_session_is_refused() {
    let db = AgentHome::start("wake-hop-refusal").await;
    // Node is configuration, so the fixture's User signs the cap.
    gents_documents::test_support::set_test_node_field_as(
        &db.user_access,
        db.node_did(),
        "max_request_hop",
        serde_json::json!(8),
    )
    .await;
    let mut fixture = canonical_background_fixture(&db, "wake-hop-refusal-session").await;
    let parent = fixture.parent.clone();
    let native_tool = fixture.prepare_notification("sleep done").await.unwrap();
    let returned_tool = fixture.prepare_notification("B result").await.unwrap();
    let refused_tool = fixture.prepare_notification("C message").await.unwrap();
    let later_tool = fixture
        .prepare_notification("another B result")
        .await
        .unwrap();
    let selection = fixture.writer.terminal_output(&parent.doc_id).await;
    fixture
        .lifecycle
        .terminalize_owned(RequestTerminalOutcome::Completed, selection, None)
        .await
        .unwrap();
    let bound = 8;
    let hop_of = |doc_id: String| {
        let access = db.agent_access.clone();
        async move {
            let response = access
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_hop lifecycle_state }} }}"#,
                    escape_graphql_string(&doc_id)
                ))
                .await.unwrap();
            response["data"]["AgentRequest"][0].clone()
        }
    };
    let native = persist_background_completion_with_message_canonical_with_access(
        &db.agent_access,
        &db.node,
        &parent,
        gents_session::request::lifecycle::queue::BackgroundCompletionNotification {
            content: "sleep done",
            message_key: "background-completion-notification:sleep:tool",
            wake_content: "review notifications",
            queue: background_hints(&parent),
            existing_notification_doc_id: None,
            wake: gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        },
        &native_tool,
    )
    .await
    .unwrap();
    let native_wake = native.request.expect("native wake").doc_id;
    let returned = persist_background_completion_with_message_canonical_with_access(
        &db.agent_access,
        &db.node,
        &parent,
        gents_session::request::lifecycle::queue::BackgroundCompletionNotification {
            content: "B result",
            message_key: "background-completion-notification:b:tool",
            wake_content: "review notifications",
            queue: background_hints(&parent),
            existing_notification_doc_id: None,
            wake: gents_protocol::request_lifecycle::RequestHopCause::Return,
        },
        &returned_tool,
    )
    .await
    .unwrap();
    assert!(!returned.created_request);
    assert_eq!(returned.request.expect("joined wake").doc_id, native_wake);
    assert_eq!(hop_of(native_wake.clone()).await["request_hop"], 0);
    let refused = persist_background_completion_with_message_canonical_with_access(
        &db.agent_access,
        &db.node,
        &parent,
        gents_session::request::lifecycle::queue::BackgroundCompletionNotification {
            content: "C message",
            message_key: "background-completion-notification:c:tool",
            wake_content: "review notifications",
            queue: background_hints(&parent),
            existing_notification_doc_id: None,
            wake: gents_protocol::request_lifecycle::RequestHopCause::CrossSession {
                cause_hop: bound,
            },
        },
        &refused_tool,
    )
    .await
    .unwrap();
    let refused_wake = refused.request.expect("over-bound wake").doc_id;
    let refused_row = hop_of(refused_wake.clone()).await;
    assert_eq!(refused_row["request_hop"], bound + 1);
    assert!(!gents_protocol::request_lifecycle::request_hop_within_bound(Some(bound), bound + 1));
    assert_eq!(hop_of(native_wake).await["lifecycle_state"], "superseded");
    db.agent_access
        .write("test.refuse_wake", &format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "dead" }}) {{ _docID }} }}"#,
            escape_graphql_string(&refused_wake)
        ))
        .await.unwrap();
    let later = persist_background_completion_with_message_canonical_with_access(
        &db.agent_access,
        &db.node,
        &parent,
        gents_session::request::lifecycle::queue::BackgroundCompletionNotification {
            content: "another B result",
            message_key: "background-completion-notification:b-2:tool",
            wake_content: "review notifications",
            queue: background_hints(&parent),
            existing_notification_doc_id: None,
            wake: gents_protocol::request_lifecycle::RequestHopCause::Return,
        },
        &later_tool,
    )
    .await
    .unwrap();
    assert!(later.created_request);
    let later_row = hop_of(later.request.expect("later wake").doc_id).await;
    assert_eq!(later_row["request_hop"], bound + 1);
}

use gents_toolcalls::test_support::queue::background_goal_repair::*;

#[tokio::test]
async fn canonical_publication_replay_preserves_one_input_and_one_wake() {
    let (db, mut fixture) = fixture("ordinary-canonical-publication").await;
    let parent = fixture.parent.clone();
    let prepared = fixture
        .prepare_notification("terminal output")
        .await
        .unwrap();
    let mut first_wake = None;
    let mut first_message = None;
    for first in [true, false] {
        let result = fixture
            .publish_prepared_notification(
                "terminal output",
                "canonical-ordinary-notification",
                "review notifications",
                gents_toolcalls::test_support::queue::background_goal_repair::hints(&parent),
                None,
                &prepared,
            )
            .await
            .unwrap();
        assert_eq!(result.created_request, first);
        let wake = result
            .request
            .expect("ordinary publication returns wake")
            .doc_id;
        if first {
            first_wake = Some(wake.clone());
        }
        assert_eq!(Some(&wake), first_wake.as_ref());
        let messages = message_ids(&db, "canonical-ordinary-notification").await;
        assert_eq!(messages.len(), 1);
        if first {
            first_message = messages.first().cloned();
        }
        assert_eq!(messages.first(), first_message.as_ref());
        assert_eq!(queue_rows(&db, &parent.session_id).await.len(), 2);
    }
}
