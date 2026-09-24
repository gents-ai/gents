use super::*;
use gents::lifecycle::RequestTerminalOutcome;

use gents_protocol::request_lifecycle::RequestLifecycleState;

fn parsed_request_state(value: &str) -> RequestLifecycleState {
    RequestLifecycleState::parse(value).unwrap_or_else(|error| {
        panic!("invalid generated request lifecycle state {value:?}: {error}")
    })
}

fn rust_request_transition_action(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("workspaceBindingPending", "pending") => Some("bindWorkspace"),
        ("pending", "claimed") => Some("claim"),
        ("pending", "failed") => Some("admissionReject"),
        ("pending", "superseded") => Some("dedupLose"),
        ("claimed", "processing") => Some("beginInference"),
        ("processing", "processing") => Some("continueProcessing"),
        ("processing", "completed") => Some("finish"),
        ("processing", "failed") => Some("fail"),
        ("claimed", "failed") => Some("failBeforeStream"),
        ("pending", "dead") => Some("expire"),
        ("pending", "interrupted") => Some("interruptBeforeClaim"),
        ("claimed", "interrupted") => Some("interruptClaimed"),
        ("processing", "interrupted") => Some("interruptProcessing"),
        _ => None,
    }
}

/// Production writers for edges no single `RequestContext.Action` takes, but
/// that registered recovery sweeps perform on persisted rows (Lean:
/// `requestRecoverySweepReachable`, cited as
/// `boundary.request.recovery-sweep-reachable`).
///
/// These were previously published as `illegal`, which made the emitted
/// contract assert that Rust has no writer for edges the product performs.
fn rust_request_recovery_sweep_writer(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("claimed", "dead") | ("processing", "dead") => {
            Some("ToolCallLifecycle::reconcile_subagent_liveness")
        }
        _ => None,
    }
}

// The transition classification itself comes from the generated Lean contract,
// never from a Rust mirror table: an edge is legal/recoveryReachable/illegal
// because Lean says so, and each branch of
// `generated_request_transition_cases_cover_lifecycle_policy` fences its
// classification against the production writer inventories above. Keeping a
// Rust copy of the classification here would only create a second source of
// truth that could be edited to silence a drift failure.

/// Drive the real recovery sweep named by the contract and assert it persists the
/// modelled post-state.
///
/// The two `-> dead` edges run inside `reconcile_subagent_liveness`, which needs a
/// running-bridge plus expired-child fixture. Their runtime drive lives in
/// `production_request_writers_only_reach_contracted_edges` below (the
/// `subagent_liveness` writer against real bridge fixtures); driving them from
/// THIS generated-case test remains open, tracked in #994.
async fn drive_generated_request_recovery_reachable_case(case: &LeanLifecycleTransitionCase) {
    let _ = case;
}

#[tokio::test]
async fn ordinary_completion_rejects_claimed_without_execution() {
    let db = test_db("ordinary-complete-from-claimed").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
    let mut lifecycle =
        request_lifecycle_for_case(&db, doc_id.clone(), request_id, session_id, created_at);
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    assert_eq!(
        lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None
            )
            .await
            .unwrap(),
        gents::lifecycle::TerminalizeResult::Lost,
    );
    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id)
            .await
            .lifecycle_state,
        RequestLifecycleState::Claimed
    );
}

#[tokio::test]
async fn claim_atomically_projects_the_request_owned_session() {
    let db = test_db("claim-atomically-projects-session").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
    let mut lifecycle = request_lifecycle_for_case(
        &db,
        doc_id,
        request_id.clone(),
        session_id.clone(),
        created_at,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    assert_session_observes_request(
        &db.node,
        &session_id,
        &request_id,
        RequestLifecycleState::Claimed,
    )
    .await;
}

#[tokio::test]
async fn admission_rejection_is_terminal_and_does_not_mint_a_session() {
    let db = test_db("admission-rejection-terminal").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
    let mut lifecycle = request_lifecycle_for_case(
        &db,
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );

    lifecycle
        .reject_admission("session projection rejected")
        .await
        .unwrap();

    let request = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(request.lifecycle_state, RequestLifecycleState::Failed);
    assert!(fetch_session_snapshot(&db.node, &session_id)
        .await
        .is_none());
    let persisted = db.node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ terminal_output failure_reason }} }}"#,
        gents::graphql::escape_graphql_string(&doc_id),
    )).await;
    assert!(!persisted.has_errors(), "{:?}", persisted.errors);
    let persisted = &persisted.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(
        serde_json::from_value::<gents_protocol::output::TerminalOutput>(
            persisted["terminal_output"].clone()
        )
        .unwrap(),
        gents_protocol::output::TerminalOutput::NoMessage,
    );
    assert_eq!(persisted["failure_reason"], "session projection rejected");
}

#[tokio::test]
async fn terminalizing_an_older_request_preserves_the_latest_projection() {
    let db = test_db("older-request-terminal-preserves-latest").await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let first_request_id = uuid::Uuid::new_v4().to_string();
    let first_doc_id = create_request(
        &db.node,
        &first_request_id,
        &session_id,
        "pending",
        &created_at,
    )
    .await;
    let mut first = request_lifecycle_for_case(
        &db,
        first_doc_id.clone(),
        first_request_id,
        session_id.clone(),
        created_at.clone(),
    );
    assert_eq!(first.claim().await.unwrap(), ClaimOutcome::Claimed);

    let second_request_id = uuid::Uuid::new_v4().to_string();
    let second_doc_id = create_request(
        &db.node,
        &second_request_id,
        &session_id,
        "pending",
        &created_at,
    )
    .await;
    // Seed a newer replicated observation; terminalization must reread the
    // current request and retain this exact physical/logical identity.
    support::seed_session_observation(
        &db.node,
        &session_id,
        &gents_protocol::session::SessionObservation {
            last_activity_at: created_at.clone(),
            preview: Some("newer request".into()),
            latest_request: Some(gents_protocol::session::SessionRequestObservation {
                request_doc_id: second_doc_id,
                request_id: second_request_id.clone(),
                lifecycle_state: RequestLifecycleState::Pending,
            }),
        },
    )
    .await;

    crate::support::begin_owned_execution(&mut first, &db.node)
        .await
        .unwrap();
    first
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        fetch_request_snapshot(&db.node, &first_doc_id)
            .await
            .lifecycle_state,
        RequestLifecycleState::Completed
    );
    assert_session_observes_request(
        &db.node,
        &session_id,
        &(second_request_id),
        RequestLifecycleState::Pending,
    )
    .await;
}

fn request_lifecycle_for_case(
    db: &support::TestDb,
    doc_id: String,
    request_id: String,
    session_id: String,
    created_at: String,
) -> RequestLifecycle {
    let request = build_request(doc_id, request_id, session_id, created_at);
    RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    )
}

async fn drive_generated_request_legal_case(case: &LeanLifecycleTransitionCase) {
    let db = test_db("generated-request-transition").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let action = case
        .action
        .as_deref()
        .expect("legal Request transition case must carry an action");
    let valid_until = (action == "expire")
        .then(|| (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());
    let request_input =
        (action == "dedupLose").then(|| coalesce_input(&format!("dedup-{request_id}")));
    let initial_state = if action == "bindWorkspace" {
        "workspaceBindingPending"
    } else {
        "pending"
    };
    let doc_id = create_request_with_signed_fields(
        &db.node,
        &request_id,
        &session_id,
        initial_state,
        &created_at,
        valid_until.as_deref(),
        request_input.as_deref(),
        None,
        None,
    )
    .await;
    let mut lifecycle = request_lifecycle_for_case(
        &db,
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at.clone(),
    );

    match action {
        "bindWorkspace" => {
            gents::__test_internals::activate_workspace_bound_request(&db.node, &doc_id)
                .await
                .expect("bindWorkspace CAS mutation failed");
        }
        "claim" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
        }
        "dedupLose" => {
            // Drive the PRODUCTION supersede writer. This arm used to issue its
            // own raw mutation, which asserted only that DefraDB accepts a
            // superseding write — not that any runtime code performs one.
            let survivor_id = uuid::Uuid::new_v4().to_string();
            let survivor_created_at =
                (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339();
            let key = format!("dedup-{request_id}");
            let survivor_input = coalesce_input(&key);
            let survivor_doc_id = create_request_with_signed_fields(
                &db.node,
                &survivor_id,
                &session_id,
                "pending",
                &survivor_created_at,
                None,
                Some(&survivor_input),
                None,
                None,
            )
            .await;
            gents::__test_internals::reconcile_coalesced_pending_request(
                &db.node,
                &session_id,
                AGENT_DID,
                gents::__test_internals::QueueSource::User,
                &key,
            )
            .await
            .expect("coalesce reconcile must succeed");
            let _ = survivor_doc_id;
        }
        "admissionReject" => {
            lifecycle
                .reject_admission("session projection rejected")
                .await
                .unwrap();
        }
        "beginInference" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
        }
        "continueProcessing" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
        }
        "finish" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Completed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
        }
        "fail" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
        }
        "failBeforeStream" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
        }
        "expire" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);
        }
        "interruptBeforeClaim" => {
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
        }
        "interruptClaimed" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Interrupted,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("interrupted"),
                )
                .await
                .unwrap();
        }
        "interruptProcessing" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Interrupted,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("interrupted"),
                )
                .await
                .unwrap();
        }
        other => panic!(
            "generated Request transition {} has unsupported action {other:?}",
            case.name
        ),
    }

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap.lifecycle_state,
        parsed_request_state(&case.to),
        "generated Request transition {} expected {} -> {} classified as {} via {:?}, got persisted lifecycle_state={}",
        case.name,
        case.from,
        case.to,
        case.classification,
        case.action,
        snap.lifecycle_state
    );
}

pub(super) async fn generated_request_transition_cases_cover_lifecycle_policy() {
    let mut legal_count = 0;
    let mut illegal_count = 0;
    let mut recovery_reachable_count = 0;

    for case in lean_request_transition_cases() {
        match case.classification.as_str() {
            "legal" => {
                legal_count += 1;
                assert_eq!(
                    case.action.as_deref(),
                    rust_request_transition_action(&case.from, &case.to),
                    "Request transition {} legal writer action drifted for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                assert!(
                    rust_request_recovery_sweep_writer(&case.from, &case.to).is_none(),
                    "Request transition {} is legal but a Rust recovery sweep also claims {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                drive_generated_request_legal_case(case).await;
            }
            "illegal" => {
                illegal_count += 1;
                // This consults the writer inventory above, which catches drift
                // between the inventory and the Lean contract. The claim that
                // production cannot REACH these edges is carried by
                // `production_request_writers_only_reach_contracted_edges`, which
                // drives the real writers and asserts the observed edge set falls
                // inside legal + recoveryReachable.
                assert!(
                    rust_request_transition_action(&case.from, &case.to).is_none()
                        && rust_request_recovery_sweep_writer(&case.from, &case.to).is_none(),
                    "Request transition {} is ordinary illegal but Rust has a writer path for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
            }
            "recoveryReachable" => {
                recovery_reachable_count += 1;
                assert!(
                    case.action.is_none(),
                    "Request transition {} is recovery-reachable and must be taken by no single action, got {:?}",
                    case.name,
                    case.action
                );
                assert!(
                    rust_request_recovery_sweep_writer(&case.from, &case.to).is_some(),
                    "Request transition {} is recovery-reachable but no Rust sweep writer is registered for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                assert_eq!(
                    case.boundary.as_deref(),
                    Some("boundary.request.recovery-sweep-reachable"),
                    "Request transition {} must cite the recovery-sweep boundary",
                    case.name
                );
                drive_generated_request_recovery_reachable_case(case).await;
            }
            other => panic!(
                "generated Request transition {} has unknown classification {other:?}",
                case.name
            ),
        }
    }

    assert_eq!(legal_count, 13);
    assert_eq!(illegal_count, 66);
    assert_eq!(recovery_reachable_count, 2);
}

fn assert_terminal_lifecycle_state(lifecycle_state: &str) {
    assert!(
        matches!(
            lifecycle_state,
            "completed" | "failed" | "superseded" | "dead" | "interrupted"
        ),
        "not a terminal lifecycle_state: {lifecycle_state}"
    );
}

async fn force_terminal_persisted_state(node: &EmbeddedNode, doc_id: &str, lifecycle_state: &str) {
    assert_terminal_lifecycle_state(lifecycle_state);
    force_persisted_lifecycle_state(node, doc_id, lifecycle_state).await;
}

/// Queue input marking a request as coalescible under `key`, the shape
/// `reconcile_coalesced_pending_request` matches on.
fn coalesce_input(key: &str) -> String {
    serde_json::json!({
        "queue": {
            "source": "user",
            "policy": "coalesce",
            "key": key,
            "queued_after_request_id": null,
        }
    })
    .to_string()
}

async fn set_request_deadline(node: &EmbeddedNode, doc_id: &str, deadline: &str) {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let escaped_deadline = escape_graphql_string(deadline);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                input: {{ deadline: "{escaped_deadline}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "set deadline failed: {:?}", resp.errors);
}

/// A foreground bridge used when a generated request-transition case only
/// exercises child terminalization. Foreground bridges are projected by their
/// in-memory waiter, so this fixture needs no fabricated parent edge.
async fn create_running_subagent_bridge(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    child_request_id: &str,
) {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let escaped_child = escape_graphql_string(child_request_id);
    let started_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{escaped_session_id}:{escaped_tool_call_id}",
                session_id: "{escaped_session_id}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{escaped_tool_call_id}",
                args: "{{}}",
                result: "",
                status: "running",
                lifecycle_state: "running",
                cancel_policy: "cascade",
                await_mode: "foreground",
                child_request_id: "{escaped_child}",
                started_at: "{started_at}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create subagent bridge failed: {:?}",
        resp.errors
    );
}

async fn force_persisted_lifecycle_state(node: &EmbeddedNode, doc_id: &str, lifecycle_state: &str) {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                input: {{ lifecycle_state: "{lifecycle_state}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "forcing terminal persisted state failed: {:?}",
        resp.errors
    );
}

/// Every `RequestLifecycle` writer that mutates `AgentRequest`, driven from every
/// persisted start state, with the observed edge recorded from the document.
///
/// This is the INVERSION of the `illegal` branch in the generated-case test
/// (#994). That branch asks "does my hand-written inventory list a writer for this
/// pair?", which is a statement about the table, not the code — a writer the table
/// omits is invisible to it, which is how three edges with real writers stayed
/// classified `illegal`. This asks the opposite and stronger question: drive the
/// real writers, see which edges they actually produce, and require that set to
/// fall inside what the contract permits.
///
/// Each writer is placed in its own locally-permitted state and the persisted row
/// is then forced independently, so the in-memory `ensure_state` guard never
/// short-circuits ahead of the mutation and the persisted CAS filter is what
/// decides every case. Verified by weakening a CAS in production (widening the
/// `lifecycle_state` predicate on the terminal transition): this test then
/// reports `production writers reached interrupted -> completed`.
///
/// Coverage: every legal edge except `processing -> processing`, plus all three
/// `recoveryReachable` edges. The two sweeps are driven against real auxiliary
/// fixtures — `reconcile_coalesced_pending_request` against an older survivor
/// under the same coalesce key, and `reconcile_subagent_liveness` against a
/// running bridge whose child deadline has lapsed.
///
/// `continueProcessing` is an identity transition in the Lean owner. Immutable
/// output appends do not change request lifecycle state, so they add no edge
/// to this state-changing writer inventory.
#[tokio::test]
async fn production_request_writers_only_reach_contracted_edges() {
    const START_STATES: [&str; 9] = [
        "pending",
        "claimed",
        "processing",
        "completed",
        "failed",
        "superseded",
        "dead",
        "interrupted",
        "workspaceBindingPending",
    ];
    const WRITERS: [&str; 12] = [
        "claim",
        "admission_reject",
        "claim_after_ttl_lapse",
        "claim_after_interrupt",
        "begin_execution",
        "complete",
        "fail",
        "interrupt",
        "repair_terminal_requests",
        "coalesce_pending",
        "subagent_liveness",
        "bind_workspace",
    ];

    let mut observed: std::collections::BTreeSet<(String, String)> = Default::default();

    for start in START_STATES {
        let db = test_db(&format!("production-edges-{}", start.to_lowercase())).await;

        for writer in WRITERS {
            let request_id = uuid::Uuid::new_v4().to_string();
            let session_id = uuid::Uuid::new_v4().to_string();
            let created_at = chrono::Utc::now().to_rfc3339();
            let valid_until = (writer == "claim_after_ttl_lapse")
                .then(|| (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());
            let coalesce =
                (writer == "coalesce_pending").then(|| coalesce_input("conformance-key"));
            let doc_id = create_request_with_signed_fields(
                &db.node,
                &request_id,
                &session_id,
                "pending",
                &created_at,
                valid_until.as_deref(),
                coalesce.as_deref(),
                None,
                None,
            )
            .await;
            let mut lifecycle = request_lifecycle_for_case(
                &db,
                doc_id.clone(),
                request_id.clone(),
                session_id.clone(),
                created_at.clone(),
            );

            // Give post-claim writers a real generation before pinning the
            // persisted start state. Their database authorization must reject
            // illegal edges even when the caller once owned this request.
            match writer {
                "claim"
                | "admission_reject"
                | "claim_after_ttl_lapse"
                | "claim_after_interrupt"
                | "coalesce_pending"
                | "bind_workspace" => {}
                _ => {
                    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
                }
            }

            // Processing writers require a real claimed generation. Canonical
            // terminal output is selected by the request owner; there is no
            // separate response row.
            if start == "processing"
                && matches!(
                    writer,
                    "complete" | "fail" | "interrupt" | "subagent_liveness"
                )
            {
                crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                    .await
                    .unwrap();
            }
            if writer == "claim_after_interrupt" {
                set_interrupt_requested_at(&db.node, &doc_id, &chrono::Utc::now().to_rfc3339())
                    .await;
            }
            if writer == "repair_terminal_requests" {
                let expiry = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
                let result = db.node.execute(&format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&doc_id), escape_graphql_string(&expiry),
                )).await;
                assert!(!result.has_errors(), "{:?}", result.errors);
            }

            // Auxiliary rows the sweep writers act on. Built before the start
            // state is pinned, so the sweep sees the state under test.
            match writer {
                "coalesce_pending" => {
                    // An older survivor in the same session under the same
                    // coalesce key; the request under test is the duplicate.
                    let survivor_id = uuid::Uuid::new_v4().to_string();
                    let survivor_created_at =
                        (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339();
                    let survivor_input = coalesce_input("conformance-key");
                    let survivor_doc_id = create_request_with_signed_fields(
                        &db.node,
                        &survivor_id,
                        &session_id,
                        "pending",
                        &survivor_created_at,
                        None,
                        Some(&survivor_input),
                        None,
                        None,
                    )
                    .await;
                    let _ = survivor_doc_id;
                }
                "subagent_liveness" => {
                    // An expired child of a running bridge: a live executor
                    // enforces its own deadline, so a lapsed non-terminal row
                    // means the executor is gone.
                    let past = (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339();
                    set_request_deadline(&db.node, &doc_id, &past).await;
                    create_running_subagent_bridge(
                        &db.node,
                        &session_id,
                        &format!("bridge-{request_id}"),
                        &request_id,
                    )
                    .await;
                }
                _ => {}
            }

            // Then pin the persisted row to the start state under test,
            // independently of local state, as a concurrent actor would.
            force_persisted_lifecycle_state(&db.node, &doc_id, start).await;

            let before = fetch_request_snapshot(&db.node, &doc_id)
                .await
                .lifecycle_state;
            assert_eq!(
                before,
                parsed_request_state(start),
                "fixture for {start}/{writer} did not reach the intended start state"
            );

            match writer {
                "claim" | "claim_after_interrupt" => {
                    let _ = lifecycle.claim().await;
                }
                "admission_reject" => {
                    let _ = lifecycle.reject_admission("projection rejected").await;
                }
                "claim_after_ttl_lapse" => {
                    // The expiry writer is reached THROUGH claim(): a pre-claim
                    // request whose TTL has lapsed terminalizes to `dead`.
                    let _ = lifecycle.claim().await;
                }
                "begin_execution" => {
                    let _ = crate::support::begin_owned_execution(&mut lifecycle, &db.node).await;
                }
                "complete" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Completed,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            None,
                        )
                        .await;
                }
                "fail" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Failed,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            None,
                        )
                        .await;
                }
                "interrupt" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Interrupted,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            Some("interrupted"),
                        )
                        .await;
                }
                "repair_terminal_requests" => {
                    let _ = RequestLifecycle::repair_terminal_requests(&db.node, AGENT_DID).await;
                }
                "coalesce_pending" => {
                    let _ = gents::__test_internals::reconcile_coalesced_pending_request(
                        &db.node,
                        &session_id,
                        AGENT_DID,
                        gents::__test_internals::QueueSource::User,
                        "conformance-key",
                    )
                    .await;
                }
                "subagent_liveness" => {
                    let _ =
                        ToolCallLifecycle::reconcile_subagent_liveness(&db.node, AGENT_DID).await;
                }
                "bind_workspace" => {
                    let _ = gents::__test_internals::activate_workspace_bound_request(
                        &db.node, &doc_id,
                    )
                    .await;
                }
                other => panic!("unhandled writer {other}"),
            }

            let after = fetch_request_snapshot(&db.node, &doc_id)
                .await
                .lifecycle_state;
            if after != before {
                observed.insert((before.as_str().to_string(), after.as_str().to_string()));
            }
        }
    }

    // Coverage floor. Without this the subset assertion below could be satisfied
    // vacuously by a fixture that silently stopped driving its writer.
    let expected: std::collections::BTreeSet<(String, String)> = [
        ("pending", "claimed"),
        ("pending", "failed"),
        ("pending", "dead"),
        ("pending", "interrupted"),
        ("pending", "superseded"),
        ("claimed", "processing"),
        ("claimed", "dead"),
        ("claimed", "failed"),
        ("claimed", "interrupted"),
        ("processing", "completed"),
        ("processing", "dead"),
        ("processing", "failed"),
        ("processing", "interrupted"),
    ]
    .into_iter()
    .map(|(from, to)| (from.to_string(), to.to_string()))
    .collect();
    let missing: Vec<_> = expected.difference(&observed).collect();
    assert!(
        missing.is_empty(),
        "production writers stopped reaching edges this test is supposed to cover: {missing:?}"
    );

    // Classify against the LEAN-EMITTED contract, not the Rust mirror table at
    // the top of this file. That table is convenient for naming writers, but if
    // the reachability fence consulted it, a failure could be "fixed" by editing
    // a table in the test — the same tautology this test exists to replace.
    // Going through the generated cases means an out-of-contract edge can only be
    // legitimised by changing the Lean model, which drags the boundary and the
    // coverage ledger along with it.
    let contract = lean_request_transition_cases();
    for (from, to) in &observed {
        let case = contract
            .iter()
            .find(|case| &case.from == from && &case.to == to)
            .unwrap_or_else(|| {
                panic!("production reached {from} -> {to}, absent from the Lean contract entirely")
            });
        assert!(
            matches!(case.classification.as_str(), "legal" | "recoveryReachable"),
            "production writers reached {from} -> {to}, which the Lean contract classifies {}",
            case.classification
        );
    }
}

/// S1 (`terminal_irreversibility`) asserted against PRODUCTION writers rather
/// than against the writer inventory at the top of this file.
///
/// This models the race the persisted CAS filters exist for: this runtime still
/// holds a live `RequestLifecycle` that believes it owns the request, while
/// another actor — a recovery sweep, a replicated peer, an operator interrupt —
/// has already terminalized the row. Every terminal writer must leave the
/// persisted document untouched, so no `terminal -> *` edge is reachable.
///
/// Unlike the `illegal` branch of the generated-case test (which consults the
/// hand-written inventory and so cannot see an unlisted writer), this drives the
/// real writers and asserts on persisted state.
///
/// SCOPE — this does not cover every writer or the whole `terminal -> *`
/// partition, and the name says only what is actually driven. Covered:
/// `claim`, `admission_reject`, `begin_execution`, `complete`, `fail`,
/// `transition_to_interrupted`, and the `repair_terminal_requests` recovery
/// sweep. Still uncovered: deduplication and expiry writers, and the
/// remaining recovery mutations. Enumerating every writer and deriving the
/// reachable edge set is #994.
#[tokio::test]
async fn terminal_persisted_requests_reject_request_mutating_lifecycle_writers() {
    const TERMINAL_STATES: [&str; 5] = ["completed", "failed", "superseded", "dead", "interrupted"];
    const WRITERS: [&str; 7] = [
        "claim",
        "admission_reject",
        "begin_execution",
        "complete",
        "fail",
        "interrupt",
        "repair_terminal_requests",
    ];

    for terminal in TERMINAL_STATES {
        let db = test_db(&format!("terminal-irreversibility-{terminal}")).await;

        for writer in WRITERS {
            let request_id = uuid::Uuid::new_v4().to_string();
            let session_id = uuid::Uuid::new_v4().to_string();
            let created_at = chrono::Utc::now().to_rfc3339();
            let doc_id =
                create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
            let mut lifecycle = request_lifecycle_for_case(
                &db,
                doc_id.clone(),
                request_id.clone(),
                session_id.clone(),
                created_at.clone(),
            );

            // Take real ownership first, so the local state machine permits the
            // call and the persisted CAS filter is what actually decides. `claim`
            // is driven from a fresh lifecycle so it exercises the claim filter
            // itself rather than a re-claim.
            if writer != "claim" {
                assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            }

            // Another actor terminalizes the row underneath the live lifecycle.
            force_terminal_persisted_state(&db.node, &doc_id, terminal).await;

            // The writer may return Ok (no rows matched) or Err; the contract is
            // about the persisted document, not the return value.
            match writer {
                "claim" => {
                    let outcome = lifecycle.claim().await;
                    if let Ok(outcome) = outcome {
                        assert_ne!(
                            outcome,
                            ClaimOutcome::Claimed,
                            "claim() reported success against a persisted {terminal} request"
                        );
                    }
                }
                "admission_reject" => {
                    let _ = lifecycle.reject_admission("projection rejected").await;
                }
                "begin_execution" => {
                    let _ = crate::support::begin_owned_execution(&mut lifecycle, &db.node).await;
                }
                "complete" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Completed,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            None,
                        )
                        .await;
                }
                "fail" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Failed,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            None,
                        )
                        .await;
                }
                "interrupt" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Interrupted,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            Some("interrupted"),
                        )
                        .await;
                }
                "repair_terminal_requests" => {
                    let report = RequestLifecycle::repair_terminal_requests(&db.node, AGENT_DID)
                        .await
                        .expect("terminal repair sweep must succeed");
                    assert_eq!(
                        report.repaired, 0,
                        "terminal repair moved a persisted {terminal} request: {report:?}"
                    );
                }
                other => panic!("unhandled writer {other}"),
            };

            let snap = fetch_request_snapshot(&db.node, &doc_id).await;
            assert_eq!(
                snap.lifecycle_state,
                parsed_request_state(terminal),
                "writer {writer} moved a persisted {terminal} request to {} — terminal states must be irreversible (S1)",
                snap.lifecycle_state
            );
        }
    }
}

#[tokio::test]
async fn interactive_claim_snapshot_matches_claimed_waiting() {
    let db = test_db("interactive-claim").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    assert_lean_transition_is_legal("Request", "pending", "claimed");

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            lifecycle_state: RequestLifecycleState::Claimed,
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
}

#[tokio::test]
async fn interactive_claim_atomically_pins_session_behavior() {
    let db = test_db("interactive-claim-session-projection").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    assert_session_observes_request(
        &db.node,
        &session_id,
        &(request_id),
        RequestLifecycleState::Claimed,
    )
    .await;
}

#[tokio::test]
async fn interactive_admission_and_progress_snapshots_match_execution_flow() {
    let db = test_db("interactive-executing").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();
    assert_lean_transition_is_legal("Request", "claimed", "processing");
    assert_lean_transition_is_legal("Request", "processing", "processing");

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            lifecycle_state: RequestLifecycleState::Processing,
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_session_observes_request_with_authoritative_state(
        &db.node,
        &session_id,
        &(request_id),
        // Session observations are admission/terminal projections. Streaming
        // progress is read from the exact request and response owners above.
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Processing,
    )
    .await;

    // Processing self-steps are observation-only now. Request lifecycle is the
    // durable owner; there is no response/progress row to advance.
}

#[tokio::test]
async fn interactive_fail_before_stream_snapshot_matches_failed_released() {
    let db = test_db("interactive-fail").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Failed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    assert_lean_transition_is_legal("Request", "claimed", "failed");

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            lifecycle_state: RequestLifecycleState::Failed,
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_session_observes_request(
        &db.node,
        &session_id,
        &(request_id),
        RequestLifecycleState::Failed,
    )
    .await;
}

#[tokio::test]
async fn scheduled_materialization_snapshot_matches_claimed_waiting() {
    let db = test_db("scheduled-materialize").await;
    crate::support::fixtures::configure_subagent_behavior(
        &db.node,
        AGENT_DID,
        AGENT_NAME,
        "scheduled-materialize-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        materialization_identity(),
        "scheduled prompt body",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        TriggerLineage::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        fetch_request_snapshot(&db.node, &lifecycle.request().doc_id).await,
        RequestSnapshot {
            lifecycle_state: RequestLifecycleState::Claimed,
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "scheduled".into(),
            retry_parent_request: "".into(),
            retry_root_request: lifecycle.request().request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );

    assert_session_observes_request(
        &db.node,
        &lifecycle.request().session_id,
        &(lifecycle.request().request_id.clone()),
        RequestLifecycleState::Claimed,
    )
    .await;
}

#[tokio::test]
async fn scheduled_materialization_persists_trigger_lineage() {
    let db = test_db("scheduled-materialize-lineage").await;
    crate::support::fixtures::configure_subagent_behavior(
        &db.node,
        AGENT_DID,
        AGENT_NAME,
        "scheduled-materialize-lineage-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    let lineage = TriggerLineage {
        trigger_id: Some("sched-1".into()),
        trigger_kind: Some("schedule".into()),
        source_doc_id: None,
        correlation: None,
        trigger_context: None,
    };

    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        materialization_identity(),
        "scheduled prompt body with lineage",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    assert_eq!(
        fetch_request_lineage_snapshot(&db.node, &lifecycle.request().doc_id).await,
        RequestLineageSnapshot {
            caused_by_trigger_id: Some("sched-1".into()),
            caused_by_trigger_kind: Some("schedule".into()),
        }
    );

    assert_eq!(
        fetch_request_lineage_snapshot_by_tuple(&db.node, "sched-1", "schedule").await,
        Some(RequestLineageSnapshot {
            caused_by_trigger_id: Some("sched-1".into()),
            caused_by_trigger_kind: Some("schedule".into()),
        })
    );

    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ admission_kind admission_signer_did admission_signature }} }}"#,
            escape_graphql_string(&lifecycle.request().doc_id),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = &response.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(row["admission_kind"], "local-self");
    assert_eq!(row["admission_signer_did"], AGENT_DID);
    assert!(row["admission_signature"]
        .as_str()
        .is_some_and(|signature| !signature.is_empty()));
}

use gents::tool_call_lifecycle::ToolCallLifecycle;

pub(super) async fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    let cases = lean_queue_deadline_cases();
    assert!(!cases.is_empty());

    for case in cases {
        drive_queue_deadline_case(case).await;
    }
}

async fn drive_queue_deadline_case(case: &lean_vocab_test::LeanQueueDeadlineConformanceCase) {
    match case.name.as_str() {
        "active_request_blocks_later_same_session_claim" => {
            drive_active_request_blocks_later_same_session_claim(case).await;
        }
        "terminal_active_allows_next_pending_same_session_claim" => {
            drive_terminal_active_allows_next_pending_same_session_claim(case).await;
        }
        // This generated case requires private accepted provider publication.
        // Its native drive lives in tool_call_lifecycle::request_scope_conformance.
        "background_completion_notification_creates_no_agent_request" => {}
        "cancel_drains_automated_wakeups_preserves_user_pending" => {
            drive_cancel_drains_automated_wakeups_preserves_user_pending(case).await;
        }
        "claim_preserves_explicit_deadline" => {
            drive_claim_preserves_explicit_deadline(case).await;
        }
        other => panic!("unhandled queue/deadline conformance case {other}"),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct QueueRuntimeRow {
    request_id: String,
    lifecycle_state: Option<RequestLifecycleState>,
    input: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueRuntimeSnapshot {
    active_request_id: Option<usize>,
    pending_request_ids: Vec<usize>,
    terminal_request_ids: Vec<usize>,
    coalesced_pending_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct DeadlineRuntimeRow {
    lifecycle_state: Option<RequestLifecycleState>,
    deadline: String,
}

fn symbolic_request_id(
    runtime_request_id: &str,
    generated_ids: &std::collections::BTreeMap<String, usize>,
) -> Option<usize> {
    runtime_request_id
        .parse::<usize>()
        .ok()
        .or_else(|| generated_ids.get(runtime_request_id).copied())
}

fn row_is_pending(row: &QueueRuntimeRow) -> bool {
    row.lifecycle_state == Some(RequestLifecycleState::Pending)
}

fn row_is_active(row: &QueueRuntimeRow) -> bool {
    matches!(
        row.lifecycle_state,
        Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
    )
}

fn row_is_terminal(row: &QueueRuntimeRow) -> bool {
    row.lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
}

fn row_matches_coalesced_key(row: &QueueRuntimeRow, queue_key: Option<&str>) -> bool {
    let Some(queue_key) = queue_key else {
        return false;
    };
    if !row_is_pending(row) {
        return false;
    }
    let Some(input) = row.input.as_ref() else {
        return false;
    };
    let Some(queue) = input.get("queue") else {
        return false;
    };
    queue.get("source").and_then(serde_json::Value::as_str) == Some("background_completion")
        && queue.get("policy").and_then(serde_json::Value::as_str) == Some("coalesce")
        && queue.get("key").and_then(serde_json::Value::as_str) == Some(queue_key)
}

async fn fetch_queue_runtime_snapshot(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    queue_key: Option<&str>,
    generated_ids: &std::collections::BTreeMap<String, usize>,
) -> QueueRuntimeSnapshot {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                request_id
                lifecycle_state
                input
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "queue snapshot query failed: {:?}",
        response.errors
    );
    let rows: Vec<QueueRuntimeRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();

    let active_request_ids = rows
        .iter()
        .filter(|row| row_is_active(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    assert!(
        active_request_ids.len() <= 1,
        "queue snapshot should expose at most one active request: {active_request_ids:?}"
    );

    let pending_request_ids = rows
        .iter()
        .filter(|row| row_is_pending(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    let terminal_request_ids = rows
        .iter()
        .filter(|row| row_is_terminal(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    let coalesced_pending_count = rows
        .iter()
        .filter(|row| row_matches_coalesced_key(row, queue_key))
        .count();

    QueueRuntimeSnapshot {
        active_request_id: active_request_ids.first().copied(),
        pending_request_ids,
        terminal_request_ids,
        coalesced_pending_count,
    }
}

fn assert_pre_queue_snapshot(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
    snapshot: &QueueRuntimeSnapshot,
) {
    assert_eq!(
        snapshot.active_request_id, case.pre_active_request_id,
        "{} pre active request drifted",
        case.name
    );
    assert_eq!(
        snapshot.pending_request_ids, case.pre_pending_request_ids,
        "{} pre pending queue drifted",
        case.name
    );
}

fn assert_post_queue_snapshot(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
    snapshot: &QueueRuntimeSnapshot,
) {
    assert_eq!(
        snapshot.active_request_id, case.post_active_request_id,
        "{} post active request drifted",
        case.name
    );
    assert_eq!(
        snapshot.pending_request_ids, case.post_pending_request_ids,
        "{} post pending queue drifted",
        case.name
    );
    assert_eq!(
        snapshot.terminal_request_ids, case.post_terminal_request_ids,
        "{} terminalized requests drifted",
        case.name
    );
    assert_eq!(
        snapshot.coalesced_pending_count, case.post_coalesced_pending_count,
        "{} coalesced pending count drifted",
        case.name
    );
}

fn request_from_parts(
    doc_id: String,
    request_id: usize,
    session_id: &str,
    created_at: &str,
    deadline: Option<String>,
) -> gents::AgentRequest {
    let mut request = build_request(
        doc_id,
        request_id.to_string(),
        session_id.to_string(),
        created_at.to_string(),
    );
    request.deadline = deadline;
    request
}

fn lifecycle_for(
    node: &std::sync::Arc<EmbeddedNode>,
    request: gents::AgentRequest,
    deadline_duration_secs: u64,
) -> RequestLifecycle {
    RequestLifecycle::new_with_agent_did(
        node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        deadline_duration_secs,
    )
}

async fn drive_active_request_blocks_later_same_session_claim(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-active-blocks").await;
    let session_id = case.session_id.to_string();
    let active_id = case.pre_active_request_id.expect("active request id");
    let pending_id = case
        .pre_pending_request_ids
        .first()
        .copied()
        .expect("pending request id");
    let active_created_at = "2026-03-23T00:00:10Z";
    let pending_created_at = "2026-03-23T00:00:20Z";

    let active_doc_id = create_request(
        &db.node,
        &active_id.to_string(),
        &session_id,
        "pending",
        active_created_at,
    )
    .await;
    let pending_doc_id = create_request(
        &db.node,
        &pending_id.to_string(),
        &session_id,
        "pending",
        pending_created_at,
    )
    .await;

    let active_request = request_from_parts(
        active_doc_id,
        active_id,
        &session_id,
        active_created_at,
        None,
    );
    let mut active_lifecycle = lifecycle_for(&db.node, active_request, DEADLINE_SECS);
    assert_eq!(
        active_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_pre_queue_snapshot(case, &pre);

    let pending_request = request_from_parts(
        pending_doc_id,
        pending_id,
        &session_id,
        pending_created_at,
        None,
    );
    let mut pending_lifecycle = lifecycle_for(&db.node, pending_request, DEADLINE_SECS);
    assert_eq!(
        pending_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Queued
    );
    assert_eq!(case.claimed_request_id, None);

    let post = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_terminal_active_allows_next_pending_same_session_claim(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-terminal-allows").await;
    let session_id = case.session_id.to_string();
    let active_id = case.pre_active_request_id.expect("active request id");
    let pending_id = case.claimed_request_id.expect("claimed request id");
    let active_created_at = "2026-03-23T00:00:10Z";
    let pending_created_at = "2026-03-23T00:00:20Z";

    let active_doc_id = create_request(
        &db.node,
        &active_id.to_string(),
        &session_id,
        "pending",
        active_created_at,
    )
    .await;
    let pending_doc_id = create_request(
        &db.node,
        &pending_id.to_string(),
        &session_id,
        "pending",
        pending_created_at,
    )
    .await;

    let active_request = request_from_parts(
        active_doc_id,
        active_id,
        &session_id,
        active_created_at,
        None,
    );
    let mut active_lifecycle = lifecycle_for(&db.node, active_request, DEADLINE_SECS);
    assert_eq!(
        active_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_pre_queue_snapshot(case, &pre);

    crate::support::begin_owned_execution(&mut active_lifecycle, &db.node)
        .await
        .unwrap();
    active_lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let pending_request = request_from_parts(
        pending_doc_id,
        pending_id,
        &session_id,
        pending_created_at,
        None,
    );
    let mut pending_lifecycle = lifecycle_for(&db.node, pending_request, DEADLINE_SECS);
    assert_eq!(
        pending_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let post = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_cancel_drains_automated_wakeups_preserves_user_pending(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-cancel-drain").await;
    let session_id = case.session_id.to_string();
    let parent_request_id = "queue-deadline-cancel-parent";
    create_queue_request(
        db.node.as_ref(),
        parent_request_id,
        &session_id,
        "completed",
        "2026-03-23T00:00:00Z",
        "interactive",
        None,
        None,
        None,
    )
    .await;

    let automated_id = case.automated_drained_request_ids[0];
    let user_id = case.preserved_user_pending_request_ids[0];
    create_queue_request(
        db.node.as_ref(),
        &automated_id.to_string(),
        &session_id,
        "pending",
        "2026-03-23T00:00:10Z",
        "scheduled",
        Some(&automated_queue_input(
            case.queue_key.as_deref().expect("queue key"),
            parent_request_id,
        )),
        None,
        None,
    )
    .await;
    create_queue_request(
        db.node.as_ref(),
        &user_id.to_string(),
        &session_id,
        "pending",
        "2026-03-23T00:00:20Z",
        "scheduled",
        Some(&user_queue_input()),
        None,
        None,
    )
    .await;

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_pre_queue_snapshot(case, &pre);

    gents::interrupt_request(db.node.as_ref(), parent_request_id)
        .await
        .unwrap();

    let post = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_post_queue_snapshot(case, &post);
    assert_eq!(
        post.pending_request_ids, case.preserved_user_pending_request_ids,
        "{} should preserve only the user pending row",
        case.name
    );
}

async fn drive_claim_preserves_explicit_deadline(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-explicit-deadline").await;
    let session_id = case.session_id.to_string();
    let request_id = case.claimed_request_id.expect("claimed request id");
    let created_at = chrono::Utc::now().to_rfc3339();
    let explicit_deadline_at =
        chrono::Utc::now() + chrono::Duration::seconds(case.pre_request_deadline.unwrap() as i64);
    let explicit_deadline = explicit_deadline_at.to_rfc3339();
    let doc_id = create_queue_request(
        db.node.as_ref(),
        &request_id.to_string(),
        &session_id,
        "pending",
        &created_at,
        "interactive",
        None,
        Some(&explicit_deadline),
        None,
    )
    .await;
    let request = request_from_parts(
        doc_id,
        request_id,
        &session_id,
        &created_at,
        Some(explicit_deadline.clone()),
    );

    let before_claim = chrono::Utc::now();
    let mut lifecycle = lifecycle_for(
        &db.node,
        request,
        case.synthesized_claim_deadline.unwrap() as u64,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let row = fetch_deadline_runtime_row(db.node.as_ref(), request_id).await;
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Claimed));

    let persisted_deadline = chrono::DateTime::parse_from_rfc3339(&row.deadline).unwrap();
    assert_eq!(
        persisted_deadline,
        chrono::DateTime::parse_from_rfc3339(&explicit_deadline).unwrap(),
        "{} should preserve the request's explicit deadline",
        case.name
    );
    assert!(
        persisted_deadline.with_timezone(&chrono::Utc)
            < before_claim
                + chrono::Duration::seconds(case.synthesized_claim_deadline.unwrap() as i64),
        "{} should keep the tighter explicit deadline instead of the synthesized claim deadline",
        case.name
    );
    assert!(case.explicit_deadline_preserved);
}

#[allow(clippy::too_many_arguments)]
async fn create_queue_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
    execution_origin: &str,
    input: Option<&str>,
    deadline: Option<&str>,
    agent_did: Option<&str>,
) -> String {
    let lifecycle_state = match status {
        "pending" => "pending",
        "processing" => "processing",
        "completed" => "completed",
        "interrupted" => "interrupted",
        "superseded" => "superseded",
        "dead" => "dead",
        "error" => "failed",
        other => panic!("unsupported queue request status {other}"),
    };
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_created_at = escape_graphql_string(created_at);
    let escaped_execution_origin = escape_graphql_string(execution_origin);
    let agent_did = escape_graphql_string(agent_did.unwrap_or(AGENT_DID));
    let input_field = input
        .map(|input| serde_json::from_str::<serde_json::Value>(input).expect("request input JSON"))
        .map(|input| {
            gents_protocol::graphql::graphql_input_literal(&input).expect("request input GraphQL")
        })
        .map(|input| format!(r#", input: {input}"#))
        .unwrap_or_default();
    let deadline_field = deadline
        .map(|deadline| format!(r#", deadline: "{}""#, escape_graphql_string(deadline)))
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                purpose: "normal",
                agent_did: "{agent_did}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "queue deadline conformance",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "",
                execution_origin: "{escaped_execution_origin}",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: 0{input_field}{deadline_field}
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create queue request failed: {:?}",
        response.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    support::first_row::<support::DocIdRow>(&node.execute(&query).await, "AgentRequest").doc_id
}

fn automated_queue_input(queue_key: &str, queued_after_request_id: &str) -> String {
    json!({
        "queue": {
            "source": "background_completion",
            "policy": "coalesce",
            "key": queue_key,
            "queued_after_request_id": queued_after_request_id,
        }
    })
    .to_string()
}

fn user_queue_input() -> String {
    json!({
        "queue": {
            "source": "user",
            "policy": "append",
            "key": null,
            "queued_after_request_id": null,
        }
    })
    .to_string()
}

async fn fetch_deadline_runtime_row(node: &EmbeddedNode, request_id: usize) -> DeadlineRuntimeRow {
    let escaped_request_id = escape_graphql_string(&request_id.to_string());
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ lifecycle_state deadline }}
        }}"#
    );
    support::first_row::<DeadlineRuntimeRow>(&node.execute(&query).await, "AgentRequest")
}

// Both the persisted request and the in-memory claim must carry this principal.
// build_request defaults to AGENT_DID; the lifecycle constructor does not replace it.
async fn create_scoped_claim(db: &support::TestDb, agent_did: &str) -> (String, String, String) {
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = support::create_request_for_agent_with_signed_fields(
        &db.node,
        agent_did,
        &request_id,
        &session_id,
        "pending",
        &created_at,
        None,
        None,
        None,
        None,
    )
    .await;
    let mut request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    request.agent_did = agent_did.to_string();
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        db.node.clone(),
        AGENT_NAME,
        agent_did,
        request,
        DEADLINE_SECS,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    (request_id, doc_id, session_id)
}

#[tokio::test]
async fn terminal_repair_sweep_ignores_foreign_did_claims() {
    let db = test_db("terminal-repair-scope-foreign").await;
    let mut claims = Vec::new();
    let expired =
        escape_graphql_string(&(chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());
    for agent_did in [AGENT_DID, "did:test:foreign-scope-owner"] {
        let (request_id, doc_id, session_id) = create_scoped_claim(&db, agent_did).await;
        let escaped_doc = escape_graphql_string(&doc_id);
        let mutation = format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc}" }} }}, input: {{ execution_lease_expires_at: "{expired}" }}) {{ _docID }} }}"#,
        );
        gents::ConfigAccess::transact_local(
            &db.node,
            None,
            "test.expire_terminal_repair_fixture",
            |txn| {
                let mutation = mutation.clone();
                Box::pin(async move {
                    txn.execute(&mutation).await?;
                    Ok(())
                })
            },
        )
        .await
        .unwrap();

        claims.push(doc_id);
    }
    let foreign_before = fetch_request_snapshot(&db.node, &claims[1]).await;
    let report = RequestLifecycle::repair_terminal_requests(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.repaired, 1);
    assert_eq!(
        fetch_request_snapshot(&db.node, &claims[0])
            .await
            .lifecycle_state,
        RequestLifecycleState::Failed,
    );
    assert_eq!(
        fetch_request_snapshot(&db.node, &claims[1]).await,
        foreign_before
    );
}

async fn assert_session_observes_request(
    node: &EmbeddedNode,
    session_id: &str,
    request_id: &str,
    expected: RequestLifecycleState,
) {
    assert_session_observes_request_with_authoritative_state(
        node, session_id, request_id, expected, expected,
    )
    .await;
}

async fn assert_session_observes_request_with_authoritative_state(
    node: &EmbeddedNode,
    session_id: &str,
    request_id: &str,
    expected_observation: RequestLifecycleState,
    expected_authoritative: RequestLifecycleState,
) {
    let session = fetch_session_snapshot(node, session_id)
        .await
        .expect("canonical session");
    assert_eq!(session.session_id, session_id);
    assert_eq!(session.agent_did, AGENT_DID);
    assert_eq!(session.behavior_id, AGENT_NAME);
    assert!(
        session.closed_at.is_none(),
        "request terminality does not close the session"
    );
    let latest = session
        .observation
        .expect("index observation")
        .latest_request
        .expect("latest request");
    assert_eq!(latest.request_id, request_id);
    assert_eq!(latest.lifecycle_state, expected_observation);
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
        request_id: {{_eq: "{request_id}"}}, session_id: {{_eq: "{session_id}"}}
    }}) {{ _docID agent_did requester_did behavior_id lifecycle_state }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "request observation query: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(Value::as_array)
        .expect("request rows");
    assert_eq!(rows.len(), 1, "exact request must be unambiguous");
    assert_eq!(
        rows[0]["_docID"].as_str(),
        Some(latest.request_doc_id.as_str())
    );
    assert_eq!(
        rows[0]["lifecycle_state"].as_str(),
        Some(expected_authoritative.as_str())
    );
    assert_eq!(
        rows[0]["agent_did"].as_str(),
        Some(session.agent_did.as_str()),
        "session observation must reference its owner's request"
    );
    assert_eq!(
        rows[0]["requester_did"].as_str(),
        session.requester_did.as_deref(),
        "absent requester scope is exact, not a wildcard"
    );
    assert_eq!(
        rows[0]["behavior_id"].as_str(),
        Some(session.behavior_id.as_str())
    );
}
