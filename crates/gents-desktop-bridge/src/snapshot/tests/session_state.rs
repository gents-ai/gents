use super::*;
use crate::types::ReconstructionState;
use gents_protocol::request_lifecycle::RequestLifecycleState;

use gents_lean_vocab::lean_transcript_cases;

fn session(requester_did: Option<&str>) -> AgentSession {
    AgentSession {
        session_id: "session-1".into(),
        node_did: "did:test:amy".into(),
        requester_did: requester_did.map(str::to_owned),
        agent_did: "amy-default".into(),
        created_at: "2026-04-21T12:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: Some(SessionTitle {
            text: "conversation".into(),
            source: SessionTitleSource::Generated,
        }),
        tags: Vec::new(),
        provenance: None,
        observation: Some(SessionObservation {
            reply_preview: None,
            last_activity_at: "2026-04-21T12:02:00Z".into(),
            preview: Some("turn two".into()),
            latest_request: Some(SessionRequestObservation {
                request_doc_id: "req-2".into(),
                request_id: "req-2".into(),
                lifecycle_state: RequestLifecycleState::Processing,
            }),
        }),
    }
}

fn request(id: &str, state: RequestLifecycleState) -> AgentRequestRow {
    AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some(id.into()),
        request_id: id.into(),
        node_did: Some("did:test:amy".into()),
        agent_did: Some("amy-default".into()),
        session_id: Some("session-1".into()),
        content: Some(format!("{id} prompt")),
        lifecycle_state: Some(state),
        execution_origin: Some("interactive".into()),
        created_at: Some("2026-04-21T12:00:00Z".into()),
        ..Default::default()
    }
}

fn contract_session_id(id: usize) -> String {
    format!("session-{id}")
}

fn contract_request_id(id: usize) -> String {
    format!("req-{id}")
}

fn request_state_for_turn(turn_state: Option<&str>) -> RequestLifecycleState {
    match turn_state {
        Some("waitingForClaim") => RequestLifecycleState::Pending,
        Some("running") => RequestLifecycleState::Processing,
        Some("completed") => RequestLifecycleState::Completed,
        Some("failed") => RequestLifecycleState::Failed,
        Some("superseded") => RequestLifecycleState::Superseded,
        Some("interrupted") => RequestLifecycleState::Interrupted,
        Some(other) => panic!("unsupported Lean ClientShell turn state {other:?}"),
        None => RequestLifecycleState::Pending,
    }
}

const CONTRACT_NODE: &str = "did:test:contract-agent";

fn lean_requester(id: usize) -> Option<String> {
    (id != 1).then(|| format!("did:test:requester-{id}"))
}

fn lean_session_rows(
    session_id: &str,
    rows: &[gents_lean_vocab::LeanSessionTurnRow],
) -> Vec<AgentRequestRow> {
    use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let owner = row
                .folded_into
                .or(row.replaced_by)
                .map(|doc| rows.iter().find(|candidate| candidate.doc == doc));
            let lifecycle = match (
                row.state.as_str(),
                row.folded_into.or(row.replaced_by),
                row.completed,
            ) {
                ("unclaimed", _, _) => RequestLifecycleState::Pending,
                ("active", _, _) => RequestLifecycleState::Processing,
                ("terminal", Some(_), _) => RequestLifecycleState::Superseded,
                ("terminal", None, true) => RequestLifecycleState::Completed,
                ("terminal", None, false) => RequestLifecycleState::Failed,
                (other, _, _) => panic!("unsupported Lean row state {other:?}"),
            };
            AgentRequestRow {
                purpose: Some(if row.public_input {
                    gents_protocol::request_admission::RequestPurpose::Normal
                } else {
                    gents_protocol::request_admission::RequestPurpose::TitleAudit
                }),
                doc_id: Some(format!("doc-{}", row.doc)),
                request_id: contract_request_id(row.request),
                node_did: Some(CONTRACT_NODE.into()),
                requester_did: lean_requester(row.requester),
                agent_did: Some("contract-agent".into()),
                session_id: Some(session_id.into()),
                content: Some(format!("input {}", row.request)),
                lifecycle_state: Some(lifecycle),
                execution_origin: Some("interactive".into()),
                created_at: Some(format!("2026-04-21T12:00:{index:02}Z")),
                retry_parent_request: row.retry_parent.map(contract_request_id),
                retry_parent_request_doc_id: row.retry_parent_doc.map(|doc| format!("doc-{doc}")),
                superseded_by_request: owner.map(|owner| {
                    owner.map_or("req-missing".into(), |o| contract_request_id(o.request))
                }),
                superseded_by_request_doc_id: row
                    .folded_into
                    .or(row.replaced_by)
                    .map(|doc| format!("doc-{doc}")),
                failure_reason: row
                    .folded_into
                    .map(|_| gents_session::request::lifecycle::FOLDED_REASON.to_owned())
                    .or_else(|| {
                        row.replaced_by
                            .map(|_| gents_session::request::lifecycle::REPLACED_REASON.to_owned())
                    }),
                input: (row.queued_after.is_some() || row.background_wake).then(|| RequestInput {
                    queue: Some(RequestQueue {
                        delivery: Default::default(),
                        position: None,
                        source: if row.background_wake {
                            QueueSource::BackgroundCompletion
                        } else {
                            QueueSource::User
                        },
                        policy: QueuePolicy::Append,
                        key: None,
                        queued_after_request_id: row.queued_after.map(contract_request_id),
                        interrupted_request_id: None,
                        background_completion_wake_version: None,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }
        })
        .collect()
}

fn contract_session(
    session_id: &str,
    requester_did: Option<String>,
    newest: Option<&AgentRequestRow>,
) -> AgentSession {
    AgentSession {
        session_id: session_id.into(),
        node_did: CONTRACT_NODE.into(),
        requester_did,
        agent_did: "contract-agent".into(),
        created_at: "2026-04-21T12:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: newest.map(|newest| SessionObservation {
            reply_preview: None,
            last_activity_at: "2026-04-21T12:01:00Z".into(),
            preview: Some("contract prompt".into()),
            latest_request: Some(SessionRequestObservation {
                request_doc_id: newest.doc_id.clone().expect("contract rows are physical"),
                request_id: newest.request_id.clone(),
                lifecycle_state: newest.lifecycle_state.expect("contract rows have state"),
            }),
        }),
    }
}

fn client_shell_contract_store(
    input: &gents_lean_vocab::families::client_shell_contracts_desktop::DesktopInput<String>,
) -> ClientStore {
    let session_id = contract_session_id(input.desktop_selected_session_id as usize);
    let requests = if input.desktop_rows.is_empty() {
        input
            .desktop_observed_request_id
            .map(|id| AgentRequestRow {
                purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                doc_id: Some(contract_request_id(id as usize)),
                request_id: contract_request_id(id as usize),
                node_did: Some(CONTRACT_NODE.into()),
                agent_did: Some("contract-agent".into()),
                session_id: Some(session_id.clone()),
                content: Some("contract prompt".into()),
                lifecycle_state: Some(request_state_for_turn(
                    input.desktop_observed_turn_state.as_deref(),
                )),
                execution_origin: Some("interactive".into()),
                created_at: Some("2026-04-21T12:00:00Z".into()),
                ..Default::default()
            })
            .into_iter()
            .collect()
    } else {
        let modeled: Vec<gents_lean_vocab::LeanSessionTurnRow> =
            gents_lean_vocab::family::recode(&input.desktop_rows);
        lean_session_rows(&session_id, &modeled)
    };
    let mut rows = ClientStoreRows::default();
    if !requests.is_empty() {
        rows.sessions
            .push(contract_session(&session_id, None, requests.last()));
    }
    rows.requests = requests;
    ClientStore::from_rows(rows)
}

fn contract_number(prefix: &str, id: &str) -> u64 {
    id.strip_prefix(prefix)
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("{id:?} is not a contract {prefix} id"))
}

fn request_ids_for_docs(rows: &[AgentRequestRow], docs: &[usize]) -> Vec<String> {
    docs.iter()
        .map(|doc| {
            rows.iter()
                .find(|row| row.doc_id.as_deref() == Some(format!("doc-{doc}").as_str()))
                .expect("expected doc is a case row")
                .request_id
                .clone()
        })
        .collect()
}

#[test]
fn display_owner_binds_generated_session_turn_cases() {
    for case in gents_lean_vocab::lean_client_session_turn_cases() {
        let rows = lean_session_rows("session-1", &case.rows);
        let selected = rows
            .iter()
            .find(|row| {
                row.doc_id.as_deref() == Some(format!("doc-{}", case.selected_doc).as_str())
            })
            .expect("selected case row");
        let refs = rows.iter().collect::<Vec<_>>();
        assert_eq!(
            gents_desktop_core::client::session_display_request(&refs, selected).doc_id,
            Some(format!("doc-{}", case.expected_display_turn_doc)),
            "{}",
            case.name
        );
    }
}

#[test]
fn session_snapshot_binds_generated_session_turn_cases() {
    let cases = gents_lean_vocab::lean_client_session_turn_cases();
    assert_eq!(cases.len(), 43);
    let public_cases = cases
        .iter()
        .filter(|case| {
            case.rows
                .iter()
                .any(|row| row.doc == case.selected_doc && row.public_input)
        })
        .collect::<Vec<_>>();
    assert_eq!(public_cases.len(), 42);
    for case in public_cases {
        let rows = lean_session_rows("session-1", &case.rows);
        let selected = rows
            .iter()
            .find(|row| {
                row.doc_id.as_deref() == Some(format!("doc-{}", case.selected_doc).as_str())
            })
            .expect("selected case row");
        let refs = rows.iter().collect::<Vec<_>>();
        assert_eq!(
            gents_desktop_core::client::session_turn_request(&refs, selected).doc_id,
            Some(format!("doc-{}", case.expected_turn_doc)),
            "{}",
            case.name
        );
        let answer = gents_desktop_core::client::request_answer_owner(&refs, selected);
        assert_eq!(
            answer.doc_id,
            Some(format!("doc-{}", case.expected_answer_doc)),
            "{}",
            case.name
        );
        let store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![contract_session(
                "session-1",
                selected.requester_did.clone(),
                Some(selected),
            )],
            requests: rows.iter().rev().cloned().collect(),
            ..ClientStoreRows::default()
        });
        let snapshot =
            build_session_snapshot_from_store(&store, "session-1", None).expect("snapshot");
        assert_eq!(
            snapshot.latest_request_id,
            request_ids_for_docs(&rows, &[case.expected_display_turn_doc]).pop(),
            "{}",
            case.name
        );
        let summaries =
            session_summaries(&store.sessions, &store.requests, CONTRACT_NODE, &[], &[]);
        assert_eq!(
            summaries[0].latest_request_id, snapshot.latest_request_id,
            "{}",
            case.name
        );
        assert_eq!(
            summaries[0].latest_request_doc_id,
            Some(format!("doc-{}", case.expected_display_turn_doc)),
            "{}",
            case.name
        );
        assert_eq!(
            summaries[0].turn_state, snapshot.turn_state,
            "{}",
            case.name
        );
        if let Some(background) = case.rows.iter().find(|row| row.background_wake) {
            let request_id = contract_request_id(background.request);
            let tracked = build_session_snapshot_from_store(&store, "session-1", Some(&request_id))
                .expect("tracked background snapshot");
            assert_eq!(
                tracked.latest_request_id, snapshot.latest_request_id,
                "{}",
                case.name
            );
            assert_eq!(tracked.turn_state, snapshot.turn_state, "{}", case.name);
        }
        assert_eq!(
            snapshot
                .queued_turns
                .iter()
                .map(|turn| turn.request_id.clone())
                .collect::<Vec<_>>(),
            request_ids_for_docs(&rows, &case.expected_queued_docs),
            "{}",
            case.name
        );
        assert_eq!(
            snapshot
                .folded_inputs
                .iter()
                .map(|input| input.request_id.clone())
                .collect::<Vec<_>>(),
            case.expected_folded_requests
                .iter()
                .copied()
                .map(contract_request_id)
                .collect::<Vec<_>>(),
            "{}",
            case.name
        );

        let read_docs = case
            .expected_read_docs
            .iter()
            .map(|doc| format!("doc-{doc}"))
            .collect::<rapidhash::RapidHashSet<_>>();
        let selected_requester = case
            .rows
            .iter()
            .find(|row| row.doc == case.selected_doc)
            .unwrap()
            .requester;
        for row in case
            .rows
            .iter()
            .filter(|row| row.requester == selected_requester && row.public_input)
        {
            if !read_docs.contains(&format!("doc-{}", row.doc)) {
                assert!(
                    row.completed
                        && row.public_input
                        && row.authored_input_materialized
                        && row.retry_parent_doc.is_none(),
                    "{} omitted a request without the modeled durable-prompt premise: {row:?}",
                    case.name
                );
            }
        }
        let bounded_rows = rows
            .iter()
            .filter(|row| {
                row.doc_id
                    .as_ref()
                    .is_some_and(|doc| read_docs.contains(doc))
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            bounded_rows.len(),
            case.expected_read_docs.len(),
            "{} read closure has each expected physical row once",
            case.name
        );
        let bounded_refs = bounded_rows.iter().collect::<Vec<_>>();
        assert_eq!(
            gents_desktop_core::client::request_answer_owner(&bounded_refs, selected).doc_id,
            answer.doc_id,
            "{} bounded read closure retains the modeled answer owner",
            case.name
        );
        let bounded_store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![contract_session(
                "session-1",
                selected.requester_did.clone(),
                Some(selected),
            )],
            requests: bounded_rows,
            ..ClientStoreRows::default()
        });
        let bounded = build_session_snapshot_from_store(&bounded_store, "session-1", None)
            .expect("bounded dependency closure retains the session projection");
        assert_eq!(
            bounded.latest_request_id, snapshot.latest_request_id,
            "{}",
            case.name
        );
        assert_eq!(bounded.turn_state, snapshot.turn_state, "{}", case.name);
        assert_eq!(
            bounded
                .queued_turns
                .iter()
                .map(|turn| &turn.request_doc_id)
                .collect::<Vec<_>>(),
            snapshot
                .queued_turns
                .iter()
                .map(|turn| &turn.request_doc_id)
                .collect::<Vec<_>>(),
            "{} physical queue owners survive the generated read closure",
            case.name
        );
        assert_eq!(
            serde_json::to_value(&bounded.queued_turns).unwrap(),
            serde_json::to_value(&snapshot.queued_turns).unwrap(),
            "{} queue output survives the generated read closure",
            case.name
        );
        assert_eq!(
            serde_json::to_value(&bounded.folded_inputs).unwrap(),
            serde_json::to_value(&snapshot.folded_inputs).unwrap(),
            "{} complete fold output survives the generated read closure",
            case.name
        );
    }
}

#[test]
fn session_queue_order_matches_native_arrival_model_after_claim() {
    gents_lean_vocab::families::session_queue_order_arrival::replay::<String>(|i| {
        let modeled: Vec<gents_lean_vocab::LeanSessionTurnRow> =
            gents_lean_vocab::family::recode(&i.rows);
        let rows = lean_session_rows("session-1", &modeled);
        let store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![contract_session(
                "session-1",
                rows.last().unwrap().requester_did.clone(),
                rows.last(),
            )],
            requests: rows.clone(),
            ..Default::default()
        });
        let mut snapshot = build_session_snapshot_from_store(&store, "session-1", None).unwrap();
        let arrivals = i
            .arrivals
            .iter()
            .map(|id| format!("doc-{id}"))
            .collect::<Vec<_>>();
        super::super::session::order_queued_turns(&mut snapshot, &arrivals);
        snapshot
            .queued_turns
            .iter()
            .filter_map(|turn| turn.request_doc_id.as_deref())
            .map(|doc| {
                doc.strip_prefix("doc-")
                    .and_then(|id| id.parse().ok())
                    .unwrap_or_else(|| panic!("queued turn {doc:?} is not a contract row"))
            })
            .collect()
    });
}

/// Only the Agent's verified observation shows a Goal (Lean
/// `SessionObservation.no_goal_only_from_verified_absence`), so a readable
/// Goal row never reaches the snapshot projection.
#[test]
fn session_snapshot_never_projects_goal_rows() {
    let goal = GoalRow {
        goal_id: "goal-1".into(),
        creation_key: None,
        session_id: "session-1".into(),
        node_did: "did:test:amy".into(),
        objective: Some("Ship the durable controller".into()),
        tags: Vec::new(),
        status: Some("active".into()),
        token_budget: Some(50_000),
        tokens_used: Some(1_200),
        active_time_seconds: Some(42),
        active_started_at: None,
        consecutive_blocked_audits: Some(2),
        last_blocked_request_id: None,
        last_blocked_reason: Some("needs approval".into()),
        last_continued_from_request_id: None,
        continuation_sequence: Some(3),
        wrapup_requested: Some(false),
        wrapup_completed: Some(false),
        infrastructure_retry_count: Some(0),
        last_failure: None,
        completion_evidence: None,
        created_at: None,
        updated_at: None,
    };
    let snapshot = build_session_snapshot_from_store_for_node(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            goals: vec![goal],
            ..ClientStoreRows::default()
        }),
        Some("did:test:amy"),
        "session-1",
        None,
    )
    .expect("snapshot");
    assert!(snapshot.goal.is_none(), "{:?}", snapshot.goal);
}

#[test]
fn requester_scope_does_not_cross_canonical_output_facts() {
    let mut rows = ClientStoreRows {
        sessions: vec![session(None), session(Some("did:test:other"))],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message_for_node(
        &mut rows,
        "owner",
        "session-1",
        None,
        1,
        MessageRole::User,
        "owner prompt",
        "did:test:amy",
        None,
    );
    push_canonical_text_message_for_node(
        &mut rows,
        "other",
        "session-1",
        None,
        2,
        MessageRole::User,
        "other prompt",
        "did:test:amy",
        Some("did:test:other"),
    );
    let store = ClientStore::from_rows(rows);
    let snapshot =
        build_session_snapshot_from_store_for_node(&store, Some("did:test:amy"), "session-1", None)
            .expect("snapshot");
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].message_key, "owner");
}

#[test]
fn canonical_transcript_keeps_partial_reconstruction_explicit() {
    let mut rows = ClientStoreRows {
        sessions: vec![session(None)],
        requests: vec![request("req-1", RequestLifecycleState::Processing)],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "incomplete",
        "session-1",
        Some("req-1"),
        1,
        MessageRole::Assistant,
        "must not be fabricated",
    );
    rows.output_segments.clear();
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "session-1", None)
            .expect("snapshot");
    assert!(
        matches!(snapshot.timeline_items.as_slice(), [RenderedTimelineItem::PendingUserTurn { .. }, RenderedTimelineItem::AssistantMessage { reconstruction, content: None, .. }] if reconstruction.state == ReconstructionState::Loading)
    );
}

#[test]
fn session_snapshot_uses_canonical_session_without_materialized_observation() {
    let mut session = session(None);
    session.observation = None;
    let store = ClientStore::from_rows(ClientStoreRows {
        sessions: vec![session],
        requests: vec![request("req-1", RequestLifecycleState::Completed)],
        ..ClientStoreRows::default()
    });
    let snapshot = build_session_snapshot_from_store(&store, "session-1", None).expect("snapshot");
    assert_eq!(snapshot.session_id, "session-1");
    assert_eq!(snapshot.node_did.as_deref(), Some("did:test:amy"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
}

#[test]
fn session_snapshot_does_not_report_unobserved_preferred_request() {
    let mut session = session(None);
    session
        .observation
        .as_mut()
        .expect("observation")
        .latest_request = Some(SessionRequestObservation {
        request_doc_id: "req-old".into(),
        request_id: "req-old".into(),
        lifecycle_state: RequestLifecycleState::Processing,
    });
    let old = AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some("req-old".into()),
        request_id: "req-old".into(),
        node_did: Some("did:test:amy".into()),
        session_id: Some("session-1".into()),
        lifecycle_state: Some(RequestLifecycleState::Completed),
        ..Default::default()
    };
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session],
            requests: vec![old],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-new"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-old"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
}

#[test]
fn session_snapshot_projection_consumes_generated_client_shell_contract_cases() {
    for case in gents_lean_vocab::families::client_shell_contracts_desktop::cases::<String>() {
        let input = &case.input;
        let store = client_shell_contract_store(input);
        let session_id = contract_session_id(input.desktop_selected_session_id as usize);
        let preferred = input
            .desktop_preferred_request_id
            .map(|id| contract_request_id(id as usize));
        let snapshot = build_session_snapshot_from_store(&store, &session_id, preferred.as_deref());
        let latest = snapshot.as_ref().and_then(|snapshot| {
            store
                .requests
                .iter()
                .find(|row| Some(&row.request_id) == snapshot.latest_request_id.as_ref())
        });
        if let Some(pending) = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.pending_turn.as_ref())
        {
            assert_eq!(
                pending.lifecycle_state,
                Some(request_state_for_turn(
                    input.desktop_observed_turn_state.as_deref()
                )),
                "{}",
                input.name
            );
        }
        case.check(
            gents_lean_vocab::families::client_shell_contracts_desktop::DesktopOutput {
                desktop_snapshot_present: snapshot.is_some(),
                desktop_expected_latest_request_id: snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.latest_request_id.as_deref())
                    .map(|id| contract_number("req-", id)),
                desktop_expected_turn_state: snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.turn_state.clone()),
                desktop_queued_request_ids: snapshot
                    .as_ref()
                    .map(|snapshot| {
                        snapshot
                            .queued_turns
                            .iter()
                            .map(|turn| contract_number("req-", &turn.request_id))
                            .collect()
                    })
                    .unwrap_or_default(),
                desktop_folded_request_ids: snapshot
                    .as_ref()
                    .map(|snapshot| {
                        snapshot
                            .folded_inputs
                            .iter()
                            .map(|folded| contract_number("req-", &folded.request_id))
                            .collect()
                    })
                    .unwrap_or_default(),
                desktop_expect_pending_turn: latest
                    .filter(|row| {
                        row.lifecycle_state
                            .is_some_and(|state| !state.is_terminal())
                    })
                    .map(|_| {
                        snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.pending_turn.is_some())
                    }),
            },
        );
    }
}

#[test]
fn session_snapshot_live_overlay_consumes_generated_contract_cases() {
    for case in gents_lean_vocab::families::contract_cases_live_overlay::cases() {
        let input = &case.input;
        let lifecycle = if input.turn_terminal {
            RequestLifecycleState::Completed
        } else {
            RequestLifecycleState::Processing
        };
        let mut request = request("req-2", lifecycle);
        request.doc_id = Some("req-2".into());
        request.execution_generation = input
            .live_output_available
            .then(|| "generation-1".to_string());
        request.execution_lease_secs = input.live_output_available.then_some(300);
        request.execution_lease_expires_at = input
            .live_output_available
            .then(|| "2026-04-21T12:05:00Z".to_string());
        let mut rows = ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![request],
            ..ClientStoreRows::default()
        };
        if input.has_durable_owner {
            push_canonical_text_message(
                &mut rows,
                &format!("{}-owner", input.name),
                "session-1",
                Some("req-2"),
                1,
                MessageRole::Assistant,
                "materialized answer",
            );
        }
        if input.live_output_available && !input.has_durable_owner {
            let mut runs = Vec::new();
            let mut payload = String::new();
            if input.has_content {
                payload.push('C');
                runs.push(SegmentRun {
                    stream: 0,
                    bytes: 1,
                    declaration: Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::Text,
                    }),
                });
            }
            if input.has_reasoning {
                payload.push('R');
                runs.push(SegmentRun {
                    stream: u32::from(input.has_content),
                    bytes: 1,
                    declaration: Some(StreamDeclaration {
                        block_index: 0,
                        part_index: u32::from(input.has_content),
                        payload: StreamPayload::Reasoning,
                    }),
                });
            }
            rows.output_segments.push(OutputSegmentRow {
                doc_id: format!("{}-open", input.name),
                segment: OutputSegment {
                    node_did: "did:test:amy".into(),
                    requester_did: None,
                    session_id: "session-1".into(),
                    request_doc_id: "req-2".into(),
                    source: OutputSource::ProviderTurn {
                        scope: gents_protocol::rendered_request::CaptureScope {
                            kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                            seq: 0,
                        },
                        turn_index: 0,
                        attempt: 0,
                    },
                    writer: OutputWriter::RequestExecution {
                        execution_generation: "generation-1".into(),
                    },
                    ordinal: Some(0),
                    runs,
                    payload,
                    close: None,
                    created_at: "2026-04-21T12:00:00Z".into(),
                },
            });
        }
        let snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(rows),
            "session-1",
            Some("req-2"),
        )
        .unwrap_or_else(|| panic!("case {} should produce a snapshot", input.name));
        case.check(
            gents_lean_vocab::families::contract_cases_live_overlay::LiveOverlayOutput {
                expect_overlay: snapshot
                    .timeline_items
                    .iter()
                    .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })),
            },
        );
    }
}

#[test]
fn session_snapshot_transcript_rendering_consumes_generated_transcript_cases() {
    let cases = lean_transcript_cases();
    assert_eq!(cases.len(), 11);
    for case in cases {
        let mut rows = ClientStoreRows {
            sessions: vec![session(None)],
            ..ClientStoreRows::default()
        };
        for sequence in 0..case.post_message_count {
            push_canonical_text_message(
                &mut rows,
                &format!("{}-message-{sequence}", case.name),
                "session-1",
                Some("req-1"),
                u32::try_from(sequence + 1).expect("contract sequence"),
                if sequence == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                &format!("{} payload {sequence}", case.name),
            );
        }
        for index in 0..case.post_tool_call_count {
            rows.tool_calls.push(
                serde_json::from_value(serde_json::json!({
                    "_docID": format!("{}-tool-{index}", case.name),
                    "node_did": "did:test:amy",
                    "request_doc_id": "req-1",
                    "tool_call_key": format!("{}-tool-{index}", case.name),
                    "session_id": "session-1",
                    "request_id": "req-1",
                    "message_sequence": case.assistant_sequence,
                    "tool_name": "read",
                    "tool_call_id": format!("result-{}", case.logical_result_id + index),
                    "status": if case.expected_pair_closed { "completed" } else { "running" },
                    "lifecycle_state": if case.expected_pair_closed { "completed" } else { "running" }
                }))
                .expect("canonical tool-call envelope"),
            );
        }
        let snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(rows),
            "session-1",
            Some("req-1"),
        )
        .unwrap_or_else(|| panic!("case {} should produce a snapshot", case.name));
        assert_eq!(
            snapshot.messages.len(),
            case.post_message_count,
            "{}",
            case.name
        );
        assert_eq!(
            snapshot.tool_calls.len(),
            case.post_tool_call_count,
            "{}",
            case.name
        );
        assert_eq!(
            snapshot
                .timeline_items
                .iter()
                .filter_map(|item| match item {
                    RenderedTimelineItem::ToolGroup {
                        message_sequence,
                        tools,
                        ..
                    } => {
                        Some((*message_sequence, tools.len()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            if case.post_tool_call_count == 0 {
                Vec::new()
            } else {
                vec![(
                    Some(case.assistant_sequence as i64),
                    case.post_tool_call_count,
                )]
            },
            "{} tool pairing/group sequence drifted",
            case.name
        );
        assert!(
            case.expected_ordered,
            "{} must retain transcript ordering",
            case.name
        );
        if case.expected_duplicate_reused_sequence {
            assert_eq!(
                case.pre_message_count, case.post_message_count,
                "{} duplicate observation must not append a message",
                case.name
            );
            assert_eq!(
                case.result_sequence, case.post_message_count,
                "{} must retain the original tool-result sequence",
                case.name
            );
        }
    }
}

#[test]
fn session_snapshot_retains_modeled_terminal_provider_evidence() {
    for case in gents_lean_vocab::lean_request_transition_cases()
        .iter()
        .filter(|case| case.to == "interrupted" && case.classification == "legal")
    {
        for certificate in [None, Some(case.stopped_before_running)] {
            let mut row = request("req-2", RequestLifecycleState::Interrupted);
            row.stopped_before_running = certificate;
            let snapshot = build_session_snapshot_from_store(
                &ClientStore::from_rows(ClientStoreRows {
                    sessions: vec![session(None)],
                    requests: vec![row],
                    ..ClientStoreRows::default()
                }),
                "session-1",
                Some("req-2"),
            )
            .expect("snapshot");
            assert_eq!(
                snapshot
                    .latest_request_outcome
                    .unwrap()
                    .stopped_before_running,
                certificate,
                "{}",
                case.name
            );
        }
    }
}

#[test]
fn session_snapshot_derives_cancel_causes_from_request_and_tool() {
    let mut interrupted = request("req-2", RequestLifecycleState::Interrupted);
    interrupted.failure_reason = Some("completion cancelled".into());
    interrupted.interrupt_requested_at = Some("2026-04-21T12:03:00Z".into());
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![interrupted],
            tool_calls: vec![serde_json::from_value(serde_json::json!({
                "_docID": "cancelled-tool-doc",
                "tool_call_key": "cancelled-tool",
                "tool_call_id": "cancelled-call",
                "node_did": "did:test:amy",
                "session_id": "session-1",
                "request_id": "req-2",
                "request_doc_id": "req-2",
                "tool_name": "bash",
                "lifecycle_state": "cancelled",
                "completed_at": "2026-04-21T12:03:01Z"
            }))
            .expect("cancelled tool lifecycle row")],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-2"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("interrupted"));
    let outcome = snapshot.latest_request_outcome.expect("request outcome");
    assert_eq!(
        outcome.failure_reason.as_deref(),
        Some("completion cancelled")
    );
    let cause = outcome.cancel_cause.expect("cancel cause");
    assert_eq!(cause.cause, "userCancelled");
    assert_eq!(cause.source, "requestInterrupt");
    assert_eq!(snapshot.tool_calls.len(), 1);
    let tool_cause = snapshot.tool_calls[0]
        .cancel_cause
        .as_ref()
        .expect("cancelled tool retains request interrupt evidence");
    assert_eq!(tool_cause.cause, "userCancelled");
    assert_eq!(tool_cause.source, "requestInterrupt");
    assert_eq!(tool_cause.at.as_deref(), Some("2026-04-21T12:03:00Z"));
}

#[test]
fn tool_cancel_cause_uses_only_its_physical_request_owner() {
    for request_doc_id in [Some("req-1"), Some("missing-request"), None] {
        let mut latest = request("req-2", RequestLifecycleState::Interrupted);
        latest.interrupt_requested_at = Some("2026-04-21T12:03:00Z".into());
        let snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(ClientStoreRows {
                sessions: vec![session(None)],
                requests: vec![request("req-1", RequestLifecycleState::Completed), latest],
                tool_calls: vec![serde_json::from_value(serde_json::json!({
                    "_docID": "cancelled-tool-doc",
                    "tool_call_key": "cancelled-tool",
                    "tool_call_id": "cancelled-call",
                    "node_did": "did:test:amy",
                    "session_id": "session-1",
                    "request_id": "req-2",
                    "request_doc_id": request_doc_id,
                    "tool_name": "bash",
                    "lifecycle_state": "cancelled"
                }))
                .expect("cancelled tool lifecycle row")],
                ..ClientStoreRows::default()
            }),
            "session-1",
            Some("req-2"),
        )
        .expect("snapshot");
        assert_eq!(snapshot.tool_calls.len(), 1);
        let cause = snapshot.tool_calls[0].cancel_cause.as_ref().expect("cause");
        assert_eq!(cause.cause, "unknown", "owner={request_doc_id:?}");
        assert_eq!(cause.source, "unresolved", "owner={request_doc_id:?}");
    }
}

#[test]
fn accepted_steer_remains_in_the_transcript_before_intake() {
    use gents_protocol::request_input::{
        QueueDelivery, QueuePolicy, QueueSource, RequestInput, RequestQueue,
    };

    for delivery in [QueueDelivery::Queue, QueueDelivery::Steer] {
        let mut input = request("req-3", RequestLifecycleState::Pending);
        input.created_at = Some("2026-04-21T12:01:00Z".into());
        input.input = Some(RequestInput {
            queue: Some(RequestQueue {
                delivery,
                position: None,
                source: QueueSource::User,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: Some("req-2".into()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            }),
            ..Default::default()
        });
        let store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![request("req-2", RequestLifecycleState::Processing), input],
            ..Default::default()
        });
        let snapshot = build_session_snapshot_from_store(&store, "session-1", None)
            .expect("snapshot after reconnect");
        assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-2"));
        assert_eq!(snapshot.turn_state.as_deref(), Some("running"));
        assert_eq!(snapshot.queued_turns.len(), 1);
        assert_eq!(snapshot.queued_turns[0].request_id, "req-3");
        assert_eq!(
            snapshot.timeline_items.iter().any(|item| matches!(
                item, RenderedTimelineItem::PendingUserTurn { request_id, .. }
                    if request_id == "req-3"
            )),
            delivery == QueueDelivery::Steer,
            "accepted {delivery:?} input before provider intake"
        );
    }
}

#[test]
fn session_snapshot_projects_failed_request_reason_without_response_storage() {
    let mut failed = request("req-2", RequestLifecycleState::Failed);
    failed.failure_reason = Some("provider exploded".into());
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![failed],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-2"),
    )
    .expect("snapshot");
    let outcome = snapshot.latest_request_outcome.expect("request outcome");
    assert_eq!(outcome.failure_reason.as_deref(), Some("provider exploded"));
    assert!(outcome.cancel_cause.is_none());
    assert!(snapshot.retry_eligibility.eligible);
}

#[test]
fn refused_latest_request_observation_keeps_the_failed_turn_reason_retry_and_prompt_visible() {
    let refusal_reason =
        "request admission denied: request activates a skill outside its context allowlist";
    let mut refused_session = session(None);
    refused_session.observation = Some(SessionObservation {
        reply_preview: None,
        last_activity_at: "2026-04-21T12:02:00Z".into(),
        preview: Some("run the disallowed skill".into()),
        latest_request: Some(SessionRequestObservation {
            request_doc_id: "req-2".into(),
            request_id: "req-2".into(),
            lifecycle_state: RequestLifecycleState::Failed,
        }),
    });
    let mut refused = request("req-2", RequestLifecycleState::Failed);
    refused.content = Some("run the disallowed skill".into());
    refused.failure_reason = Some(refusal_reason.into());
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![refused_session],
            requests: vec![request("req-1", RequestLifecycleState::Completed), refused],
            ..ClientStoreRows::default()
        }),
        "session-1",
        None,
    )
    .expect("snapshot");
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-2"),);
    assert_eq!(snapshot.turn_state.as_deref(), Some("failed"));
    let outcome = snapshot.latest_request_outcome.expect("request outcome");
    assert_eq!(outcome.failure_reason.as_deref(), Some(refusal_reason));
    assert!(snapshot.retry_eligibility.eligible);
    let pending = snapshot.pending_turn.expect("refused prompt stays visible");
    assert_eq!(pending.request_id, "req-2");
    assert_eq!(pending.content, "run the disallowed skill");
    assert_eq!(pending.lifecycle_state, Some(RequestLifecycleState::Failed));
    assert!(
        snapshot
            .timeline_items
            .iter()
            .any(|item| matches!(item, RenderedTimelineItem::PendingUserTurn { request_id, .. } if request_id == "req-2"))
    );
}

#[test]
fn session_snapshot_derives_interrupted_cause_for_caused_request() {
    let mut child = request("req-2", RequestLifecycleState::Interrupted);
    child.caused_by_parent_request_id = Some("parent".into());
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![child],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-2"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("interrupted"));
    let cause = snapshot
        .latest_request_outcome
        .expect("request outcome")
        .cancel_cause
        .expect("cancel cause");
    assert_eq!(cause.cause, "interrupted");
    assert_eq!(cause.source, "requestLifecycle");
}
