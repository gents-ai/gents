use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[path = "../../../../../crates/gents/src/lean_vocab_test/support.rs"]
mod lean_vocab_test;

use crate::types::TriggerView;
use lean_vocab_test::{lean_trigger_dispatch_case_count, lean_trigger_dispatch_cases};

fn canonical_session(requester: Option<&str>) -> gents_protocol::session::AgentSession {
    serde_json::from_value(serde_json::json!({
        "session_id":"session", "agent_did":"did:test:owner", "requester_did":requester,
        "behavior_id":"behavior", "created_at":"2026-09-01T00:00:00Z",
        "title":{"text":"Saved title","source":"user"}, "tags":["review"],
        "provenance":{"task_id":"task", "graph_run_id":"graph", "fork":{"source_session_id":"parent","at_user_turn":2}}
    })).unwrap()
}

fn indexed_session(requester: Option<&str>) -> gents_protocol::session::AgentSession {
    let mut session = canonical_session(requester);
    session.observation = Some(gents_protocol::session::SessionObservation {
        last_activity_at: "2026-09-02T00:00:00Z".into(),
        preview: Some("latest prompt".into()),
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: "physical-head".into(),
            request_id: "logical-head".into(),
            lifecycle_state: RequestLifecycleState::Processing,
        }),
    });
    session
}

fn indexed_request(requester: Option<&str>) -> AgentRequestRow {
    AgentRequestRow {
        doc_id: Some("physical-head".into()),
        request_id: "logical-head".into(),
        agent_did: Some("did:test:owner".into()),
        requester_did: requester.map(str::to_owned),
        session_id: Some("session".into()),
        behavior_id: "behavior".into(),
        lifecycle_state: Some(RequestLifecycleState::Processing),
        ..Default::default()
    }
}

#[test]
fn canonical_empty_sessions_preserve_title_fork_and_requester_groups() {
    let rows = [
        canonical_session(None),
        canonical_session(Some("did:test:requester")),
    ];
    let summaries = session_summaries(&rows, &[], &[], "did:test:owner", &[], &[]);
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].session_id, summaries[1].session_id);
    assert_ne!(summaries[0].requester_did, summaries[1].requester_did);
    for summary in summaries {
        assert_eq!(summary.title.as_deref(), Some("Saved title"));
        assert_eq!(summary.task_id.as_deref(), Some("task"));
        assert_eq!(summary.tags, ["review"]);
        assert_eq!(summary.provenance.unwrap().fork.unwrap().at_user_turn, 2);
        assert_eq!(summary.created_at.as_deref(), Some("2026-09-01T00:00:00Z"));
        assert!(summary.latest_request_doc_id.is_none());
    }
}

#[test]
fn missing_indexed_request_does_not_fall_back_to_older_or_foreign_rows() {
    let session = indexed_session(None);
    let mut older = indexed_request(None);
    older.doc_id = Some("older-physical".into());
    older.lifecycle_state = Some(RequestLifecycleState::Completed);
    let mut foreign = indexed_request(Some("did:test:foreign"));
    foreign.lifecycle_state = Some(RequestLifecycleState::Failed);
    let summaries = session_summaries(
        &[session],
        &[older, foreign],
        &[],
        "did:test:owner",
        &[],
        &[],
    );
    assert_eq!(
        summaries[0].latest_request_doc_id.as_deref(),
        Some("physical-head")
    );
    assert_eq!(summaries[0].status.as_deref(), Some("processing"));
    assert_eq!(summaries[0].preview_text.as_deref(), Some("latest prompt"));
    assert!(session_summaries(
        &[],
        &[indexed_request(None)],
        &[],
        "did:test:owner",
        &[],
        &[]
    )
    .is_empty());
}

#[test]
fn live_session_response_requires_physical_and_requester_identity() {
    let session = indexed_session(Some("did:test:requester"));
    let request = indexed_request(Some("did:test:requester"));
    let mut response = AgentResponseRow {
        response_key: "logical-head".into(),
        request_id: Some("logical-head".into()),
        request_doc_id: Some("other-physical".into()),
        agent_did: request.agent_did.clone(),
        requester_did: request.requester_did.clone(),
        session_id: request.session_id.clone(),
        status: Some("streaming".into()),
        ..Default::default()
    };
    let project = |response: AgentResponseRow| {
        session_summaries(
            &[session.clone()],
            &[request.clone()],
            &[response],
            "did:test:owner",
            &[],
            &[],
        )
    };
    assert_eq!(
        project(response.clone())[0].turn_state.as_deref(),
        Some("waitingForClaim")
    );
    response.request_doc_id = request.doc_id.clone();
    response.requester_did = None;
    assert_eq!(
        project(response.clone())[0].turn_state.as_deref(),
        Some("waitingForClaim")
    );
    response.requester_did = request.requester_did.clone();
    assert_eq!(
        project(response)[0].turn_state.as_deref(),
        Some("streaming")
    );
}

#[test]
fn task_run_history_is_agent_scoped_when_trigger_ids_match() {
    let store = ClientStore::from_rows(ClientStoreRows {
        requests: vec![
            AgentRequestRow {
                request_id: "req-mini-1".to_string(),
                agent_did: Some("did:test:mini-1".to_string()),
                requester_did: None,
                behavior_id: "default".to_string(),
                session_id: Some("session-mini-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("run task".to_string()),
                max_total_tokens: None,
                lifecycle_state: Some(RequestLifecycleState::Completed),
                backend_id: None,
                execution_origin: Some("scheduled".to_string()),
                failure_reason: None,
                terminalized_at: None,
                terminal_redrive_attempts: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: Some("shared-schedule".to_string()),
                caused_by_trigger_kind: Some("schedule".to_string()),
                caused_by_correlation: None,
                caused_by_trigger_context: None,
                caused_by_trigger_doc_id: None,
                caused_by_source_doc_id: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
                workspace_id: None,
                workspace_authority: None,
                workspace_seal_hash: None,
                ..Default::default()
            },
            AgentRequestRow {
                request_id: "req-mini-2".to_string(),
                agent_did: Some("did:test:mini-2".to_string()),
                requester_did: None,
                behavior_id: "default".to_string(),
                session_id: Some("session-mini-2".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("run task".to_string()),
                max_total_tokens: None,
                lifecycle_state: Some(RequestLifecycleState::Completed),
                backend_id: None,
                execution_origin: Some("scheduled".to_string()),
                failure_reason: None,
                terminalized_at: None,
                terminal_redrive_attempts: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: Some("shared-schedule".to_string()),
                caused_by_trigger_kind: Some("schedule".to_string()),
                caused_by_correlation: None,
                caused_by_trigger_context: None,
                caused_by_trigger_doc_id: None,
                caused_by_source_doc_id: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
                workspace_id: None,
                workspace_authority: None,
                workspace_seal_hash: None,
                ..Default::default()
            },
        ],
        ..ClientStoreRows::default()
    });
    let triggers = vec![TriggerView {
        config: serde_json::from_value(serde_json::json!({
            "agent_did":"did:test:mini-1","trigger_id":"shared-schedule","task_id":"task-1",
            "source":{"kind":"schedule","schedule_id":"schedule"}
        }))
        .unwrap(),
        next_run_at: None,
        last_attempt_at: None,
        last_fired_source_doc_id: None,
        last_status: None,
        last_error: None,
        fire_count: None,
    }];

    let runs = task_run_history(&store, "did:test:mini-1", "task-1", &triggers);

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].request_id, "req-mini-1");
}

#[test]
fn task_recent_runs_view_consumes_generated_trigger_dispatch_lineage_contract_cases() {
    let cases = lean_trigger_dispatch_cases();
    assert!(
        !cases.is_empty(),
        "Lean trigger dispatch contract should emit recent-runs cases"
    );
    assert_eq!(
        cases.len(),
        lean_trigger_dispatch_case_count(),
        "Lean trigger dispatch case-count sentinel drifted"
    );

    let fired_task_cases = cases
        .iter()
        .filter(|case| {
            case.expected_result == "fired"
                && matches!(
                    case.expected_request_caused_by_kind.as_deref(),
                    Some("schedule" | "event")
                )
        })
        .collect::<Vec<_>>();

    assert!(
        fired_task_cases
            .iter()
            .any(|case| case.expected_request_caused_by_kind.as_deref() == Some("schedule")),
        "Lean trigger dispatch contract must include a fired schedule lineage case"
    );
    assert!(
        fired_task_cases
            .iter()
            .any(|case| case.expected_request_caused_by_kind.as_deref() == Some("event")),
        "Lean trigger dispatch contract must include a fired event lineage case"
    );

    for (index, case) in fired_task_cases.into_iter().enumerate() {
        let task_id = format!("contract-task-{}", case.name);
        let request_id = format!("contract-req-{index}");
        let created_at = format!("2026-04-21T12:{index:02}:00Z");
        let trigger_id = case
            .expected_request_caused_by_id
            .as_deref()
            .expect("fired schedule/event cases carry a trigger id");
        let trigger_kind = case
            .expected_request_caused_by_kind
            .as_deref()
            .expect("fired schedule/event cases carry a trigger kind");

        assert_eq!(
            case.expected_materialize_trigger_id.as_deref(),
            Some(trigger_id),
            "case {} materialized trigger id should feed recent-runs lineage",
            case.name
        );
        assert_eq!(
            case.expected_materialize_trigger_kind.as_deref(),
            Some(trigger_kind),
            "case {} materialized trigger kind should feed recent-runs lineage",
            case.name
        );

        let source = if trigger_kind == "schedule" {
            serde_json::json!({"kind":"schedule","schedule_id":"schedule"})
        } else {
            serde_json::json!({"kind":"event","event_source_id":"event-source"})
        };
        let triggers = vec![TriggerView {
            config: serde_json::from_value(serde_json::json!({
                "agent_did":"did:test:contract-agent", "trigger_id": trigger_id,
                "task_id": task_id, "source":source, "concurrency":case.concurrency
            }))
            .unwrap(),
            next_run_at: None,
            last_attempt_at: Some(created_at.clone()),
            last_fired_source_doc_id: Some("source-doc-1".into()),
            last_status: Some("completed".into()),
            last_error: None,
            fire_count: Some(1),
        }];
        let store = ClientStore::from_rows(ClientStoreRows {
            requests: vec![AgentRequestRow {
                request_id: request_id.clone(),
                agent_did: Some("did:test:contract-agent".to_string()),
                requester_did: None,
                behavior_id: "contract-behavior".to_string(),
                session_id: Some(format!("contract-session-{index}")),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("contract prompt".to_string()),
                max_total_tokens: None,
                lifecycle_state: Some(RequestLifecycleState::Completed),
                backend_id: None,
                execution_origin: case.expected_execution_origin.clone(),
                failure_reason: None,
                terminalized_at: None,
                terminal_redrive_attempts: None,
                created_at: Some(created_at.clone()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: Some(trigger_id.to_string()),
                caused_by_trigger_kind: Some(trigger_kind.to_string()),
                caused_by_correlation: None,
                caused_by_trigger_context: None,
                caused_by_trigger_doc_id: None,
                caused_by_source_doc_id: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
                workspace_id: None,
                workspace_authority: None,
                workspace_seal_hash: None,
                ..Default::default()
            }],
            ..ClientStoreRows::default()
        });

        let recent_runs =
            recent_runs_for_task_views(&triggers, "did:test:contract-agent", &task_id);
        assert_eq!(
            recent_runs.total_fires, 1,
            "case {} should project one recent fire",
            case.name
        );
        assert_eq!(
            recent_runs.last_attempt_at.as_deref(),
            Some(created_at.as_str()),
            "case {} should surface the trigger bookkeeping timestamp",
            case.name
        );
        // The single observed attempt passes its status and error through.
        assert_eq!(
            recent_runs.last_status.as_deref(),
            Some("completed"),
            "case {} should surface the latest attempt's status",
            case.name
        );
        assert!(
            recent_runs.last_error.is_none(),
            "case {} must not invent an error for a clean latest attempt",
            case.name
        );
        assert_eq!(
            recent_runs.schedule_count,
            usize::from(trigger_kind == "schedule"),
            "case {} schedule count drifted",
            case.name
        );
        assert_eq!(
            recent_runs.event_trigger_count,
            usize::from(trigger_kind == "event"),
            "case {} event trigger count drifted",
            case.name
        );

        let run_history =
            task_run_history(&store, "did:test:contract-agent", true, &task_id, &triggers);
        assert_eq!(
            run_history.len(),
            1,
            "case {} should project one recent-runs history row",
            case.name
        );
        let run = &run_history[0];
        assert_eq!(run.request_id, request_id);
        assert_eq!(run.caused_by_trigger_id.as_deref(), Some(trigger_id));
        assert_eq!(run.caused_by_trigger_kind.as_deref(), Some(trigger_kind));
        assert_eq!(
            run.execution_origin.as_deref(),
            case.expected_execution_origin.as_deref(),
            "case {} should preserve Lean trigger execution origin",
            case.name
        );
    }
}
