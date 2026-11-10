use super::*;
use crate::lean_vocab_test::{lean_plugin_job_recovery_cases, LeanPluginJobRecoveryRow};
use gents::tools::background::job::admit::{is_stalled, Refusal};
use gents::tools::background::job::recovery::{
    step_invocation_allowed, tick_action, RecoveryFacts, TickAction,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use std::collections::BTreeMap;
use support::agent_requests::{RequestActors, RequestSeed};

const RECOVERY_CREATED_AT: &str = "2026-03-23T00:00:00Z";
const PLUGIN_JOB_SWEEP_ID: &str = "tool_call_lifecycle_recover_plugin_job_rows";

pub(super) async fn generated_recovery_sweep_cases_drive_startup_recovery_contract() {
    let cases = lean_recovery_sweep_cases();
    assert_eq!(
        cases.len(),
        55,
        "Lean should emit one row per registered recovery predicate witness"
    );

    let expected_sweep_ids = [
        "request_lifecycle_recover_all_requests",
        "tool_call_lifecycle_recover_all_running_calls",
        "tool_call_lifecycle_reconcile_orphaned_background_tools",
        "tool_call_lifecycle_reconcile_background_completion_side_effects",
        "tool_call_lifecycle_reconcile_terminal_parent_owned_tools",
        "tool_call_lifecycle_recover_session_message_rows",
        PLUGIN_JOB_SWEEP_ID,
        "inference_call_recover_all_stale_calls",
        "task_hook_recover_interrupted_cleanup",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual_sweep_ids = cases
        .iter()
        .map(|case| case.sweep_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_sweep_ids, expected_sweep_ids,
        "Lean recovery sweep registry drifted"
    );
    assert_periodic_recovery_registry_matches_lean(cases);

    // Plugin-job rows keep running when a tick re-drives them; their own
    // consumer drives and checks them.
    for case in cases
        .iter()
        .filter(|case| case.sweep_id != PLUGIN_JOB_SWEEP_ID)
    {
        assert_recovery_case_metadata(case);
        drive_recovery_sweep_case(case).await;
    }
}

/// One tick of the row: the owner's `tick_action` is the first application of
/// Lean's `pluginJobRecover`; a `Step` receives every terminal, and the stall
/// rule over that step's output (`is_stalled`, the admission owner) is the
/// second. Returns the owner action and the row after it.
pub(super) fn tick_row(row: LeanPluginJobRecoveryRow) -> (TickAction, LeanPluginJobRecoveryRow) {
    let facts = RecoveryFacts {
        running: row.running,
        session_stopped: row.session_stopped,
        terminal_children: row.terminal_children,
        observed: row.observed,
        live: row.live,
        step_new: row.step_new,
        deadline_expired: row.deadline_expired,
        available: if row.available {
            Ok(())
        } else {
            Err(Refusal::NotAvailable)
        },
        stepped: row.stepped,
        in_flight: row.step_in_flight,
    };
    let action = tick_action(&facts);
    let after = match action {
        TickAction::Untouched => row,
        TickAction::Kill(_) => LeanPluginJobRecoveryRow {
            running: false,
            ..row
        },
        TickAction::Step => {
            let observed = row.terminal_children;
            let stalled = is_stalled(row.live, observed, row.terminal_children, row.step_new);
            LeanPluginJobRecoveryRow {
                running: !stalled,
                observed,
                stepped: true,
                ..row
            }
        }
    };
    (action, after)
}

/// Lean `pluginJobRecoverySweep` ticks (`pluginJobStale`, then
/// `pluginJobRecover ∘ pluginJobRecover`) through the owner's first-application
/// decision `plugin_job::recovery::tick_action`, which
/// `redrive_running_jobs` applies to each running job row: a stale row is
/// killed or re-driven one step, and a row that is not stale is untouched.
/// Lean's measures stay with its own `recovery_regressions`.
#[tokio::test]
async fn generated_plugin_job_recovery_cases_drive_redrive_owner() {
    let cases = lean_plugin_job_recovery_cases();
    assert_eq!(
        cases.len(),
        13,
        "Lean should emit the plugin-job recovery rows"
    );
    let sweep_cases = lean_recovery_sweep_cases()
        .iter()
        .filter(|case| case.sweep_id == PLUGIN_JOB_SWEEP_ID)
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        sweep_cases,
        cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        "the registered sweep's witnesses are the plugin-job tick rows"
    );
    for case in cases {
        let name = case.name.as_str();
        assert_eq!(case.sweep_id, PLUGIN_JOB_SWEEP_ID, "{name}");
        assert_eq!(
            case.rust_function, "plugin_job::recovery::redrive_running_jobs",
            "{name}"
        );
        let (action, after) = tick_row(case.row);
        assert_eq!(
            matches!(action, TickAction::Untouched),
            !case.stale,
            "{name}: stale flag"
        );
        assert_eq!(after, case.expected_row, "{name}: row after one tick");
        assert_eq!(
            case.step_commit_allowed,
            step_invocation_allowed(true, case.row.session_stopped),
            "{name}: model-driven step commit gate"
        );
        assert_eq!(
            case.step_invocation_allowed,
            step_invocation_allowed(case.row.running, case.row.session_stopped),
            "{name}: model-driven guest invocation gate"
        );
        if case.row.session_stopped {
            assert_eq!(
                action,
                TickAction::Kill(gents::tools::background::job::control::KillCause::Stop),
                "{name}"
            );
        }
    }
}

fn assert_recovery_case_metadata(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let expected_cadence = if rust_periodic_recovery_sweep_ids().contains(case.sweep_id.as_str()) {
        "periodic"
    } else {
        "startup"
    };
    assert_eq!(case.cadence.as_str(), expected_cadence, "{}", case.name);
    assert_eq!(
        case.implementation_status.as_str(),
        "implemented",
        "recovery case {} must be implemented before the runtime drive can consume it",
        case.name
    );
    if case.name.ends_with("_deferred") {
        assert_eq!(case.measure_before, 0, "{}", case.name);
        assert_eq!(case.measure_after, 0, "{}", case.name);
        assert_eq!(case.terminal_state, "running", "{}", case.name);
    } else {
        assert!(
            case.measure_before > case.measure_after,
            "recovery case {} must decrease its measure",
            case.name
        );
        assert_ne!(
            case.terminal_state.as_str(),
            "running",
            "recovery case {} must not leave a stale row running",
            case.name
        );
    }
    assert_eq!(
        case.measure_after, 0,
        "recovery case {} must reach zero measure",
        case.name
    );
    assert!(
        !case.deadline_audit_ref.trim().is_empty(),
        "recovery case {} must name its audit reference",
        case.name
    );
}

fn assert_periodic_recovery_registry_matches_lean(
    cases: &[lean_vocab_test::LeanRecoverySweepCase],
) {
    let mut lean_periodic_by_id = BTreeMap::new();
    for case in cases.iter().filter(|case| case.cadence == "periodic") {
        if let Some(previous) =
            lean_periodic_by_id.insert(case.sweep_id.as_str(), case.rust_function.as_str())
        {
            assert_eq!(
                previous,
                case.rust_function.as_str(),
                "Lean emitted conflicting Rust functions for periodic recovery sweep {}",
                case.sweep_id
            );
        }
    }

    let mut rust_periodic_by_id = BTreeMap::new();
    for metadata in gents::periodic_recovery_sweep_metadata() {
        assert!(
            !metadata.sweep_ids.is_empty(),
            "periodic recovery registry entry {} must name at least one Lean sweep id",
            metadata.rust_function
        );
        for sweep_id in metadata.sweep_ids {
            assert!(
                rust_periodic_by_id
                    .insert(*sweep_id, metadata.rust_function)
                    .is_none(),
                "periodic recovery sweep id {sweep_id} registered more than once"
            );
        }
    }

    assert_eq!(
        rust_periodic_by_id.keys().copied().collect::<BTreeSet<_>>(),
        lean_periodic_by_id.keys().copied().collect::<BTreeSet<_>>(),
        "Rust periodic recovery registry drifted from Lean cadence=periodic sweeps"
    );
    for (sweep_id, rust_function) in rust_periodic_by_id {
        assert_eq!(
            Some(&rust_function),
            lean_periodic_by_id.get(sweep_id),
            "periodic recovery registry Rust function drifted for {sweep_id}"
        );
    }
}

fn rust_periodic_recovery_sweep_ids() -> BTreeSet<&'static str> {
    gents::periodic_recovery_sweep_metadata()
        .iter()
        .flat_map(|metadata| metadata.sweep_ids.iter().copied())
        .collect()
}

async fn drive_recovery_sweep_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    if case.collection == "AgentToolCall" {
        return;
    }
    if case.collection == "TaskHookRecord" {
        return;
    }
    match (case.collection.as_str(), case.sweep_id.as_str()) {
        ("AgentRequest", _) => drive_request_recovery_case(case).await,
        ("InferenceCall", _) => drive_inference_call_recovery_case(case).await,
        (other, _) => panic!("unhandled recovery collection {other} for {}", case.name),
    }
}

pub(super) async fn startup_recovery_order_terminalizes_crash_orphaned_calls() {
    let db = test_db("startup-recovery-order-1001").await;
    let actors = RequestActors::new(&db).await;
    let agent = actors.agent().clone();
    let request_id = "startup-order-1001-request";
    let session_id = "startup-order-1001-session";
    let request_doc_id = seed_scoped_request(&actors, request_id, session_id, "pending").await;
    seed_recovery_session(
        &actors,
        session_id,
        Some("startup recovery"),
        &request_doc_id,
        request_id,
    )
    .await;
    let _owner = own_child_fixture(&actors, &request_doc_id, request_id, session_id, true).await;
    seed_expired_execution_tuple(&actors, &request_doc_id).await;
    insert_scoped_inference_call(&actors, &request_doc_id, request_id, session_id, "running").await;

    let outcome = gents::runtime::startup_recovery::run_startup_recovery_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await;
    let requests = outcome.requests.expect("startup request recovery");
    assert!(
        requests.requests_recovered >= 1,
        "crash-stuck parent must terminalize at startup: {requests:?}"
    );
    let calls = outcome.inference_calls.expect("startup inference recovery");
    assert_eq!(
        calls.calls_recovered, 1,
        "crash-orphaned running call must be terminalized in the first startup \
         pass, not survive until the next restart (#1001)"
    );

    let parent = fetch_request_recovery_row(&actors, request_id).await;
    assert_eq!(
        parent.lifecycle_state.as_str(),
        "failed",
        "crash-stuck parent repairs to failed from its recovery error response"
    );
    let row = fetch_inference_recovery_row(&actors, request_id).await;
    assert_eq!(
        row.call_state.as_str(),
        "failed",
        "orphaned running call must not keep holding a reconstructed slot"
    );
    let slot_row = InferenceCallSlotRow::new(BACKEND_ID, row.call_state.as_str());
    assert_eq!(
        reconstructed_running_slot_count([slot_row], BACKEND_ID),
        0,
        "post-recovery rows must reconstruct zero held slots"
    );

    let second = gents::runtime::startup_recovery::run_startup_recovery_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await;
    assert_eq!(
        second
            .inference_calls
            .expect("second startup inference recovery")
            .calls_recovered,
        0,
        "startup recovery must be idempotent across restarts"
    );
}

#[tokio::test]
async fn live_startup_lease_expiry_converges_inference_rows_through_periodic_registry() {
    for (initial_call_state, terminal_call_state) in
        [("running", "failed"), ("queued", "cancelled")]
    {
        let db = test_db(&format!("deferred-inference-{initial_call_state}")).await;
        let actors = RequestActors::new(&db).await;
        let agent = actors.agent().clone();
        let request_id = format!("deferred-inference-{initial_call_state}");
        let session_id = format!("deferred-inference-{initial_call_state}-session");
        let doc_id = seed_scoped_request(&actors, &request_id, &session_id, "pending").await;
        seed_recovery_session(&actors, &session_id, None, &doc_id, &request_id).await;
        let _owner = own_child_fixture(&actors, &doc_id, &request_id, &session_id, true).await;
        insert_scoped_inference_call(
            &actors,
            &doc_id,
            &request_id,
            &session_id,
            initial_call_state,
        )
        .await;
        let startup = gents::runtime::startup_recovery::run_startup_recovery_with_identities(
            &db.node,
            db.node_identity.did(),
            std::slice::from_ref(&agent),
        )
        .await;
        startup.tool_calls.expect("startup tool recovery");
        assert_eq!(
            startup
                .requests
                .expect("startup request recovery")
                .requests_recovered,
            0
        );
        assert_eq!(
            startup
                .inference_calls
                .expect("startup inference recovery")
                .calls_recovered,
            0
        );
        let registry = gents::BackgroundExecutionRegistry::default();
        let live = gents::runtime::periodic_recovery::run_periodic_recovery_sweeps_with_identities(
            &db.node,
            db.node_identity.did(),
            std::slice::from_ref(&agent),
            &registry,
        )
        .await
        .expect("live periodic recovery");
        assert!(
            live.iter().all(|run| run.is_noop()),
            "live lease must preserve both rows: {live:?}"
        );
        assert_eq!(
            fetch_request_recovery_row(&actors, &request_id)
                .await
                .lifecycle_state
                .as_str(),
            "processing"
        );
        assert_eq!(
            fetch_inference_recovery_row(&actors, &request_id)
                .await
                .call_state,
            initial_call_state
        );

        actors
            .update_request(
                &doc_id,
                json!({"execution_lease_expires_at": RECOVERY_CREATED_AT}),
            )
            .await;
        let repaired =
            gents::runtime::periodic_recovery::run_periodic_recovery_sweeps_with_identities(
                &db.node,
                db.node_identity.did(),
                std::slice::from_ref(&agent),
                &registry,
            )
            .await
            .expect("expired periodic recovery");
        assert!(
            repaired.iter().any(|run| !run.is_noop()),
            "expired pair requires recovery"
        );
        assert_eq!(
            fetch_request_recovery_row(&actors, &request_id)
                .await
                .lifecycle_state
                .as_str(),
            "failed"
        );
        let call = fetch_inference_recovery_row(&actors, &request_id).await;
        assert_eq!(
            call.call_state, terminal_call_state,
            "periodic registry must recover the inference row after its parent"
        );
        assert!(!gents::call_state_holds_backend_slot(&call.call_state));
        let second =
            gents::runtime::periodic_recovery::run_periodic_recovery_sweeps_with_identities(
                &db.node,
                db.node_identity.did(),
                std::slice::from_ref(&agent),
                &registry,
            )
            .await
            .expect("second periodic recovery");
        assert!(
            second.iter().all(|run| run.is_noop()),
            "recovery must be idempotent: {second:?}"
        );
    }
}

async fn drive_request_recovery_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let db = test_db(&format!("recovery-sweep-{}", case.name)).await;
    let actors = RequestActors::new(&db).await;
    let request_id = format!("{}-request", case.name);
    let session_id = format!("{}-session", case.name);
    let doc_id = seed_scoped_request(&actors, &request_id, &session_id, "processing").await;
    seed_expired_execution_tuple(&actors, &doc_id).await;
    seed_recovery_session(
        &actors,
        &session_id,
        Some("recovery request"),
        &doc_id,
        &request_id,
    )
    .await;
    actors
        .set_lifecycle_state(
            &doc_id,
            RequestLifecycleState::parse(&case.pre_state).expect("Lean pre-state"),
        )
        .await;
    if case.terminal_state == "interrupted" {
        actors
            .set_interrupt_requested_at(&doc_id, "2026-07-09T00:00:00Z")
            .await;
    }

    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(actors.agent()),
    )
    .await
    .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "request recovery case {} should recover one request",
        case.name
    );

    let row = fetch_request_recovery_row(&actors, &request_id).await;
    assert_eq!(
        row.lifecycle_state.as_str(),
        case.terminal_state.as_str(),
        "request recovery case {} terminal state drifted",
        case.name
    );
}

async fn drive_inference_call_recovery_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let db = test_db(&format!("recovery-sweep-{}", case.name)).await;
    let actors = RequestActors::new(&db).await;
    let request_id = format!("{}-request", case.name);
    let session_id = format!("{}-session", case.name);
    let parent_doc_id = seed_scoped_request(&actors, &request_id, &session_id, "processing").await;
    // The call binds to its session while the parent is live; the parent then
    // reaches the case's state.
    insert_scoped_inference_call(
        &actors,
        &parent_doc_id,
        &request_id,
        &session_id,
        case.pre_state.as_str(),
    )
    .await;
    let parent_state = match case.name.as_str() {
        "inference_interrupted_parent_to_cancelled" => RequestLifecycleState::Interrupted,
        "inference_queued_stale_to_cancelled" | "inference_running_stale_to_failed" => {
            RequestLifecycleState::Completed
        }
        other => panic!("unhandled inference recovery case {other}"),
    };
    actors
        .set_lifecycle_state(&parent_doc_id, parent_state)
        .await;

    let report = InferenceCall::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(actors.agent()),
    )
    .await
    .unwrap();
    assert_eq!(
        report.calls_recovered, 1,
        "inference recovery case {} should recover one call",
        case.name
    );

    let row = fetch_inference_recovery_row(&actors, &request_id).await;
    assert_eq!(
        row.call_state.as_str(),
        case.terminal_state.as_str(),
        "inference recovery case {} terminal state drifted",
        case.name
    );
    let terminal_row = InferenceCallSlotRow::new(BACKEND_ID, row.call_state.as_str());
    assert_eq!(slot_contribution(terminal_row, BACKEND_ID), 0);
    assert_eq!(
        reconstructed_running_slot_count([terminal_row], BACKEND_ID),
        0,
        "terminal InferenceCall recovery case {} must reconstruct zero running slots",
        case.name
    );
}

async fn seed_scoped_request(
    actors: &RequestActors,
    request_id: &str,
    session_id: &str,
    state: &str,
) -> String {
    actors
        .create_request(RequestSeed::new(request_id, session_id, RECOVERY_CREATED_AT).state(state))
        .await
}

/// Titles the Agent-owned session and projects the processing request into it.
async fn seed_recovery_session(
    actors: &RequestActors,
    session_id: &str,
    title: Option<&str>,
    request_doc_id: &str,
    request_id: &str,
) {
    if let Some(text) = title {
        let session_doc = actors
            .session_doc(session_id)
            .await
            .expect("seeded request created its session");
        let title = gents_protocol::session::SessionTitle {
            text: text.into(),
            source: gents_protocol::session::SessionTitleSource::Placeholder,
        };
        let input = gents_protocol::graphql::graphql_input_literal(&json!({"title": title}))
            .expect("render session title fixture");
        actors
            .agent_access
            .write(
                "test.recovery_sweeps.session_title",
                &format!(
                    r#"mutation {{ update_AgentSession(docID: "{}", input: {input}) {{ _docID }} }}"#,
                    escape_graphql_string(&session_doc)
                ),
            )
            .await
            .expect("title session as its Agent");
    }
    let observation = gents_protocol::session::SessionObservation {
        reply_preview: None,
        last_activity_at: RECOVERY_CREATED_AT.into(),
        preview: Some(title.unwrap_or("recovery request").into()),
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: request_doc_id.to_owned(),
            request_id: request_id.to_owned(),
            lifecycle_state: RequestLifecycleState::Processing,
        }),
    };
    actors
        .seed_session_observation(session_id, &observation)
        .await;
}

/// Writes the call as its working Agent, related to the request's session as
/// the admission owner relates it.
async fn insert_scoped_inference_call(
    actors: &RequestActors,
    request_doc_id: &str,
    request_id: &str,
    session_id: &str,
    call_state: &str,
) {
    let now = support::agent_requests::canonical_now();
    let call_id = format!("{request_id}-call");
    let mutation = format!(
        r#"mutation {{ add_InferenceCall(input: {{
            call_id: "{}", runtime_instance_id: "runtime-recovery-test",
            request_id: "{}", request_doc_id: "{}", session_id: "{}", call_seq: 1,
            backend_id: "{}", agent_did: "{}", node_did: "{}",
            call_kind: "inference", attempt: 1, call_state: "{}",
            queued_at: "{}", started_at: "{}", priority: 0,
            queue_depth_at_enqueue: 0, controller_generation: 0,
            backend_config_fingerprint: "test"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&call_id),
        escape_graphql_string(request_id),
        escape_graphql_string(request_doc_id),
        escape_graphql_string(session_id),
        escape_graphql_string(BACKEND_ID),
        escape_graphql_string(actors.agent_did()),
        escape_graphql_string(actors.node_did()),
        escape_graphql_string(call_state),
        escape_graphql_string(&now),
        escape_graphql_string(&now),
    );
    let session_doc = actors
        .session_doc(session_id)
        .await
        .expect("inference call session");
    actors
        .agent_access
        .transact_write("test.recovery_sweeps.inference_call", |txn| {
            let mutation = &mutation;
            let session_doc = &session_doc;
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let created = &response["data"]["add_InferenceCall"];
                let doc = created
                    .get("_docID")
                    .or_else(|| created.get(0).and_then(|row| row.get("_docID")))
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("add_InferenceCall returned no _docID"))?
                    .to_owned();
                txn.add_resource_relationship(
                    "InferenceCall",
                    &doc,
                    "parent",
                    "sessions",
                    session_doc,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .expect("insert inference call as its Agent");
}

#[derive(Debug)]
struct RequestRecoveryRow {
    lifecycle_state: RequestLifecycleState,
    failure_reason: Option<String>,
    terminal_output: Option<gents_protocol::output::TerminalOutput>,
    terminalized_at: Option<String>,
}

async fn fetch_request_recovery_row(
    actors: &RequestActors,
    request_id: &str,
) -> RequestRecoveryRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                request_id
                lifecycle_state failure_reason terminal_output terminalized_at
            }}
        }}"#
    );
    let row: gents_protocol::row::AgentRequestRow =
        first_row(&actors.query(&query).await, "AgentRequest");
    RequestRecoveryRow {
        lifecycle_state: row
            .lifecycle_state
            .expect("AgentRequest.lifecycle_state must be present"),
        failure_reason: row.failure_reason,
        terminal_output: row.terminal_output,
        terminalized_at: row.terminalized_at,
    }
}

#[derive(Debug, Deserialize)]
struct InferenceRecoveryRow {
    call_state: String,
}

async fn fetch_inference_recovery_row(
    actors: &RequestActors,
    request_id: &str,
) -> InferenceRecoveryRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                call_state
            }}
        }}"#
    );
    first_row(&actors.query(&query).await, "InferenceCall")
}

async fn own_child_fixture(
    actors: &RequestActors,
    doc_id: &str,
    request_id: &str,
    session_id: &str,
    processing: bool,
) -> RequestLifecycle {
    let request = actors.build_request(
        doc_id.to_owned(),
        request_id.to_owned(),
        session_id.to_owned(),
        RECOVERY_CREATED_AT.to_owned(),
    );
    let mut owner = actors.lifecycle(
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(owner.claim().await.unwrap(), ClaimOutcome::Claimed);
    if processing {
        let writer = gents::DefraStreamWriter::new_managed(
            actors.node.clone(),
            actors.agent().as_ref(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        owner.begin_owned_execution(&writer).await.unwrap();
    }
    owner
}

async fn seed_expired_execution_tuple(actors: &RequestActors, request_doc_id: &str) {
    actors
        .update_request(
            request_doc_id,
            json!({
                "execution_generation": uuid::Uuid::new_v4().to_string(),
                "execution_lease_expires_at": RECOVERY_CREATED_AT,
            }),
        )
        .await;
}

#[tokio::test]
async fn request_recovery_before_output_persists_canonical_terminal_selection() {
    let db = test_db("recovery-no-response-failed").await;
    let actors = RequestActors::new(&db).await;
    let agent = actors.agent().clone();
    let request_id = "recovery-no-response-request";
    let session_id = "recovery-no-response-session";
    let doc_id = seed_scoped_request(&actors, request_id, session_id, "processing").await;
    seed_expired_execution_tuple(&actors, &doc_id).await;
    seed_recovery_session(
        &actors,
        session_id,
        Some("recovery no response"),
        &doc_id,
        request_id,
    )
    .await;

    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "an expired request without published output must still terminalize"
    );

    let row = fetch_request_recovery_row(&actors, request_id).await;
    assert_eq!(
        row.lifecycle_state.as_str(),
        "failed",
        "absent output and no interrupt latch must default to failed"
    );
    assert_eq!(
        row.failure_reason.as_deref(),
        Some("execution lease expired"),
        "canonical recovery must carry its terminal reason, got {row:?}"
    );
    assert_eq!(
        row.terminal_output,
        Some(gents_protocol::output::TerminalOutput::NoMessage),
        "pre-output recovery must explicitly select no terminal message"
    );
    assert!(row.terminalized_at.is_some());

    let second = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(
        second.requests_recovered, 0,
        "a repaired terminal request must leave the active-recovery scope"
    );

    let db = test_db("recovery-no-response-interrupted").await;
    let actors = RequestActors::new(&db).await;
    let agent = actors.agent().clone();
    let request_id = "recovery-no-response-interrupted-request";
    let session_id = "recovery-no-response-interrupted-session";
    let doc_id = seed_scoped_request(&actors, request_id, session_id, "processing").await;
    seed_expired_execution_tuple(&actors, &doc_id).await;
    actors
        .set_interrupt_requested_at(&doc_id, "2026-03-23T00:00:30Z")
        .await;
    seed_recovery_session(
        &actors,
        session_id,
        Some("recovery no response interrupted"),
        &doc_id,
        request_id,
    )
    .await;

    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "a latched response-less interrupt must terminalize as interrupted"
    );

    let row = fetch_request_recovery_row(&actors, request_id).await;
    assert_eq!(
        row.lifecycle_state.as_str(),
        "interrupted",
        "a latched interrupt must outrank the absent-response Failed default"
    );
    assert_eq!(
        row.failure_reason.as_deref(),
        Some("execution lease expired"),
        "interrupted recovery retains the canonical recovery reason"
    );
    assert_eq!(
        row.terminal_output,
        Some(gents_protocol::output::TerminalOutput::NoMessage),
        "an interrupted pre-output request must not fabricate an answer"
    );
    assert!(row.terminalized_at.is_some());
}
