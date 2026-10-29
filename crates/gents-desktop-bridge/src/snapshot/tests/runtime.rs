use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::lean_vocab_test::{lean_trigger_dispatch_case_count, lean_trigger_dispatch_cases};
use crate::types::{SessionSummary, TriggerView};

fn canonical_session(requester: Option<&str>) -> AgentSession {
    serde_json::from_value(serde_json::json!({
        "session_id":"session", "node_did":"did:test:owner", "requester_did":requester,
        "agent_id":"agent", "created_at":"2026-09-01T00:00:00Z",
        "title":{"text":"Saved title","source":"user"}, "tags":["review"],
        "provenance":{"task_id":"task","fork":{"source_session_id":"parent","at_user_turn":2}}
    }))
    .expect("canonical session")
}

fn indexed_session(requester: Option<&str>) -> AgentSession {
    let mut session = canonical_session(requester);
    session.observation = Some(SessionObservation {
        reply_preview: None,
        last_activity_at: "2026-09-02T00:00:00Z".into(),
        preview: Some("latest prompt".into()),
        latest_request: Some(SessionRequestObservation {
            request_doc_id: "physical-head".into(),
            request_id: "logical-head".into(),
            lifecycle_state: RequestLifecycleState::Processing,
        }),
    });
    session
}

fn indexed_request(requester: Option<&str>) -> AgentRequestRow {
    AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some("physical-head".into()),
        request_id: "logical-head".into(),
        node_did: Some("did:test:owner".into()),
        requester_did: requester.map(str::to_owned),
        session_id: Some("session".into()),
        agent_id: Some("agent".into()),
        lifecycle_state: Some(RequestLifecycleState::Processing),
        ..Default::default()
    }
}

#[test]
fn canonical_empty_sessions_preserve_title_fork_and_requester_groups() {
    let summaries = session_summaries(
        &[
            canonical_session(None),
            canonical_session(Some("did:test:requester")),
        ],
        &[],
        "did:test:owner",
        &[],
        &[],
    );
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
    let summaries = session_summaries(&[session], &[older, foreign], "did:test:owner", &[], &[]);
    assert_eq!(
        summaries[0].latest_request_doc_id.as_deref(),
        Some("physical-head")
    );
    assert_eq!(summaries[0].status.as_deref(), Some("processing"));
    assert_eq!(summaries[0].preview_text.as_deref(), Some("latest prompt"));
    assert!(
        session_summaries(&[], &[indexed_request(None)], "did:test:owner", &[], &[]).is_empty()
    );
}

#[test]
fn session_summary_uses_request_lifecycle_not_retired_response_rows() {
    let summaries = session_summaries(
        &[indexed_session(Some("did:test:requester"))],
        &[indexed_request(Some("did:test:requester"))],
        "did:test:owner",
        &[],
        &[],
    );
    assert_eq!(summaries[0].turn_state.as_deref(), Some("running"));
}

#[test]
fn live_session_response_requires_physical_and_requester_identity() {
    let session = indexed_session(Some("did:test:requester"));
    let mut wrong_physical = indexed_request(Some("did:test:requester"));
    wrong_physical.doc_id = Some("wrong-physical".into());
    wrong_physical.lifecycle_state = Some(RequestLifecycleState::Failed);
    let mut wrong_requester = indexed_request(None);
    wrong_requester.lifecycle_state = Some(RequestLifecycleState::Completed);
    let adversarial = session_summaries(
        std::slice::from_ref(&session),
        &[wrong_physical, wrong_requester],
        "did:test:owner",
        &[],
        &[],
    );
    assert_eq!(adversarial[0].turn_state.as_deref(), Some("running"));
    assert_eq!(adversarial[0].status.as_deref(), Some("processing"));
    let exact = session_summaries(
        &[session],
        &[indexed_request(Some("did:test:requester"))],
        "did:test:owner",
        &[],
        &[],
    );
    assert_eq!(
        exact[0].latest_request_doc_id.as_deref(),
        Some("physical-head")
    );
    assert_eq!(exact[0].turn_state.as_deref(), Some("running"));
}

#[test]
fn task_run_history_is_node_scoped_when_trigger_ids_match() {
    let store = ClientStore::from_rows(ClientStoreRows {
        requests: vec![
            AgentRequestRow {
                purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                doc_id: Some("doc-mini-1".into()),
                request_id: "req-mini-1".into(),
                node_did: Some("did:test:mini-1".into()),
                session_id: Some("session-mini-1".into()),
                lifecycle_state: Some(RequestLifecycleState::Completed),
                caused_by_trigger_id: Some("shared-schedule".into()),
                caused_by_trigger_kind: Some("schedule".into()),
                ..Default::default()
            },
            AgentRequestRow {
                purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                doc_id: Some("doc-mini-2".into()),
                request_id: "req-mini-2".into(),
                node_did: Some("did:test:mini-2".into()),
                session_id: Some("session-mini-2".into()),
                lifecycle_state: Some(RequestLifecycleState::Completed),
                caused_by_trigger_id: Some("shared-schedule".into()),
                caused_by_trigger_kind: Some("schedule".into()),
                ..Default::default()
            },
        ],
        ..ClientStoreRows::default()
    });
    let triggers = vec![TriggerView {
        config: serde_json::from_value(serde_json::json!({
            "node_did":"did:test:mini-1", "trigger_id":"shared-schedule", "task_id":"task-1",
            "source":{"kind":"schedule","schedule_id":"schedule"}
        }))
        .expect("trigger"),
        next_run_at: None,
        last_attempt_at: None,
        last_fired_source_doc_id: None,
        last_status: None,
        last_error: None,
        fire_count: None,
    }];
    let histories = task_run_histories(&store, "did:test:mini-1", &triggers);
    let runs = histories.get("task-1").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].request_id, "req-mini-1");
}

#[test]
fn task_histories_keep_eight_latest_rows_per_task_without_foreign_or_duplicate_triggers() {
    let request = |index, owner: &str, trigger: &str| AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        request_id: format!("{trigger}-{index:02}"),
        node_did: Some(owner.into()),
        caused_by_trigger_id: Some(trigger.into()),
        created_at: Some(format!("2026-01-01T00:00:{index:02}Z")),
        ..Default::default()
    };
    let trigger = |id: &str, task: &str| TriggerView {
        config: serde_json::from_value(serde_json::json!({
            "node_did":"owner", "trigger_id":id, "task_id":task,
            "source":{"kind":"schedule","schedule_id":"schedule"}
        }))
        .unwrap(),
        next_run_at: None,
        last_attempt_at: None,
        last_fired_source_doc_id: None,
        last_status: None,
        last_error: None,
        fire_count: None,
    };
    let store = ClientStore::from_rows(ClientStoreRows {
        requests: (0..20)
            .map(|i| request(i, "owner", "a"))
            .chain((0..4).map(|i| request(i, "owner", "b")))
            .chain((30..40).map(|i| request(i, "foreign", "a")))
            .collect(),
        ..Default::default()
    });
    let histories = task_run_histories(
        &store,
        "owner",
        &[
            trigger("a", "task-a"),
            trigger("a", "task-a"),
            trigger("b", "task-b"),
        ],
    );
    assert_eq!(
        histories["task-a"]
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        ["a-19", "a-18", "a-17", "a-16", "a-15", "a-14", "a-13", "a-12"]
    );
    assert_eq!(histories["task-b"].len(), 4);
    assert_eq!(histories.len(), 2);
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
                "node_did":"did:test:contract-node", "trigger_id": trigger_id,
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
                purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                request_id: request_id.clone(),
                node_did: Some("did:test:contract-node".to_string()),
                requester_did: None,
                agent_id: Some("contract-agent".to_string()),
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

        let recent_runs = recent_runs_for_task_views(&triggers, "did:test:contract-node", &task_id);
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
            recent_runs.event_count,
            usize::from(trigger_kind == "event"),
            "case {} event trigger count drifted",
            case.name
        );

        let histories = task_run_histories(&store, "did:test:contract-node", &triggers);
        let run_history = histories.get(&task_id).unwrap();
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

fn starter_receipt(
    session: &AgentSession,
    session_doc_id: &str,
    source: &gents_protocol::session::SessionReference,
    parent: &str,
) -> gents_protocol::session::SessionCreationReceipt {
    gents_protocol::session::SessionCreationReceipt {
        node_did: session.node_did.clone(),
        session_id: session.session_id.clone(),
        session_doc_id: session_doc_id.into(),
        requester_did: session.requester_did.clone(),
        agent_id: session.agent_id.clone(),
        opening_request_doc_id: "opening".into(),
        cause: gents_protocol::session::SessionCreationCause::AgentNew,
        cause_binding: "tool".into(),
        starter: Some(gents_protocol::session::SessionCreationStarter {
            source: source.clone(),
            parent_request_doc_id: parent.into(),
        }),
        signature: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fleet_snapshot_verifies_indexed_starters_once_per_evidence() {
    use crate::tests::support::OPERATOR;
    use gents::NodeIdentity;
    let (core, tmp) = crate::tests::support::boot_core().await;
    let signer = gents::KeyIdentity::load_or_create(tmp.path().join("fleet.key"), None).unwrap();
    let node_did = signer.did().to_owned();
    let source = |session_id: &str| gents_protocol::session::SessionReference {
        node_did: "did:test:remote-node".into(),
        session_id: session_id.into(),
        requester_did: Some(OPERATOR.into()),
    };
    for (child, parent_session, parent) in [
        ("sess_fleet_child_shared_1", "sess_fleet_parent_a", "req-a"),
        ("sess_fleet_child_shared_2", "sess_fleet_parent_a", "req-a"),
        ("sess_fleet_child_b", "sess_fleet_parent_b", "req-b"),
        ("sess_fleet_child_forged", "sess_fleet_parent_a", "req-a"),
    ] {
        crate::tests::support::create_started_session(
            &core,
            &signer,
            crate::tests::support::fixture_session(&node_did, child),
            &source(parent_session),
            parent,
            "opening",
        )
        .await;
    }
    let access = gents::config_client::ConfigAccess::local(core.node_arc());
    let load = |session_id: &'static str| {
        let access = access.clone();
        async move {
            gents::session_origin::load_session(&access, session_id, Some(OPERATOR))
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("fixture session {session_id} must load"))
        }
    };
    let mut forged = load("sess_fleet_child_forged").await.provenance.unwrap();
    forged.creation_receipt.as_mut().unwrap().signature[0] ^= 1;
    crate::tests::support::write_provenance(&core, &node_did, "sess_fleet_child_forged", &forged)
        .await;
    crate::tests::support::create_session(&core, &node_did, "sess_fleet_legacy", Some("req-a"))
        .await;
    crate::tests::support::create_session(&core, &node_did, "sess_fleet_plain", None).await;

    let ids = [
        "sess_fleet_child_shared_1",
        "sess_fleet_child_b",
        "sess_fleet_legacy",
        "sess_fleet_child_shared_2",
        "sess_fleet_child_forged",
        "sess_fleet_plain",
    ];
    let mut sessions = Vec::new();
    for id in ids {
        sessions.push(load(id).await);
    }
    let reads = crate::tests::support::CountingRead::new(access);
    let starters = super::super::runtime_tasks::StarterCache::default();
    let build = |sessions: &[AgentSession]| session_summaries(sessions, &[], &node_did, &[], &[]);
    let (first, second) = sessions.split_at(3);
    let (mut first, mut second) = (build(first), build(second));
    super::super::runtime_tasks::resolve_summary_starters(
        &reads,
        &starters,
        first.iter_mut().chain(second.iter_mut()),
    )
    .await;
    assert_eq!(
        reads.queries(),
        1,
        "starters across two deployments are verified in one batched session read"
    );
    let summaries = first.into_iter().chain(second).collect::<Vec<_>>();
    let starter_of = |summaries: &[SessionSummary], session_id: &str| {
        summaries
            .iter()
            .find(|summary| summary.session_id == session_id)
            .unwrap()
            .started_by
            .clone()
    };
    for (child, parent_session, parent) in [
        ("sess_fleet_child_shared_1", "sess_fleet_parent_a", "req-a"),
        ("sess_fleet_child_shared_2", "sess_fleet_parent_a", "req-a"),
        ("sess_fleet_child_b", "sess_fleet_parent_b", "req-b"),
    ] {
        let starter = starter_of(&summaries, child).unwrap();
        assert_eq!(starter.session_id, parent_session, "{child}");
        assert_eq!(starter.node_did, "did:test:remote-node", "{child}");
        assert_eq!(starter.requester_did.as_deref(), Some(OPERATOR), "{child}");
        assert_eq!(starter.cause_request_doc_id, parent, "{child}");
    }
    for session_id in [
        "sess_fleet_child_forged",
        "sess_fleet_legacy",
        "sess_fleet_plain",
    ] {
        assert!(
            starter_of(&summaries, session_id).is_none(),
            "{session_id}: an unverified receipt or a parent pointer alone is no starter"
        );
    }

    let mut rebuilt = build(&sessions);
    super::super::runtime_tasks::resolve_summary_starters(&reads, &starters, &mut rebuilt).await;
    assert_eq!(
        reads.queries(),
        1,
        "a rebuild of sessions with unchanged evidence does not read"
    );
    assert_eq!(
        serde_json::to_value(starter_of(&rebuilt, "sess_fleet_child_b")).unwrap(),
        serde_json::to_value(starter_of(&summaries, "sess_fleet_child_b")).unwrap()
    );

    let child_b = sessions
        .iter_mut()
        .find(|session| session.session_id == "sess_fleet_child_b")
        .unwrap();
    let provenance = child_b.provenance.as_mut().unwrap();
    provenance.started_by = None;
    provenance.parent_request_doc_id = Some("req-a".into());
    let provenance = provenance.clone();
    crate::tests::support::write_provenance(&core, &node_did, "sess_fleet_child_b", &provenance)
        .await;
    let mut downgraded = build(&sessions);
    super::super::runtime_tasks::resolve_summary_starters(&reads, &starters, &mut downgraded).await;
    assert_eq!(
        reads.queries(),
        2,
        "changed evidence is read and verified again"
    );
    assert!(
        starter_of(&downgraded, "sess_fleet_child_b").is_none(),
        "removing the recorded starter never falls back to the parent pointer"
    );
    assert!(starter_of(&downgraded, "sess_fleet_child_shared_1").is_some());
}

#[tokio::test]
async fn recorded_summary_starters_follow_the_starter_verification_model() {
    use gents::NodeIdentity;
    struct SessionRead(serde_json::Value, std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl gents::config_client::ConfigRead for SessionRead {
        async fn execute_read(
            &self,
            query: &str,
        ) -> Result<serde_json::Value, gents::config_client::ConfigClientError> {
            assert!(
                query.contains("AgentSession") && !query.contains("AgentRequest"),
                "a starter is verified from its session row alone: {query}"
            );
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::json!({"data": {"AgentSession": [self.0.clone()]}}))
        }
    }
    let model: serde_json::Value = tokio::task::spawn_blocking(
        gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
    )
    .await
    .unwrap()
    .unwrap();
    let cases = model["session_document_cases"]["starter_verification"]
        .as_array()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let signer = gents::KeyIdentity::load_or_create(dir.path().join("node.key"), None).unwrap();
    for requester in [None, Some("did:test:source-requester")] {
        let mut session = canonical_session(None);
        session.node_did = signer.did().to_owned();
        let source = gents_protocol::session::SessionReference {
            node_did: "did:test:remote-node".into(),
            session_id: "source-session".into(),
            requester_did: requester.map(str::to_owned),
        };
        let mut receipt = starter_receipt(&session, "physical-session", &source, "missing-request");
        receipt.signature = signer.sign(&receipt.signing_payload()).await.unwrap();
        session.provenance = Some(gents_protocol::session::SessionProvenance {
            parent_request_doc_id: Some("missing-request".into()),
            opening_request_doc_id: Some("opening".into()),
            started_by: Some(source.clone()),
            creation_receipt: Some(receipt),
            ..Default::default()
        });
        let mut row = serde_json::to_value(&session).unwrap();
        row["_docID"] = "physical-session".into();
        row["started_by_node_did"] = source.node_did.clone().into();
        row["started_by_session_id"] = source.session_id.clone().into();
        let scope = gents::session_origin::SessionScope::from(&session);
        let candidate = |name: &str| {
            let mut row = row.clone();
            match name {
                "valid" => {}
                "source" => {
                    row["provenance"]["started_by"]["session_id"] = "forged".into();
                    row["started_by_session_id"] = "forged".into();
                }
                "physical" => row["_docID"] = "different-physical-session".into(),
                "index" => row["started_by_session_id"] = "other".into(),
                "parent" => row["provenance"]["parent_request_doc_id"] = "forged-parent".into(),
                "signature" => {
                    let signature = &mut row["provenance"]["creation_receipt"]["signature"][0];
                    *signature = (signature.as_u64().unwrap() ^ 1).into();
                }
                "starter_removed" => {
                    row["provenance"]
                        .as_object_mut()
                        .unwrap()
                        .remove("started_by");
                }
                "starter_removed_parent_rewritten" => {
                    row["provenance"]
                        .as_object_mut()
                        .unwrap()
                        .remove("started_by");
                    row["provenance"]["parent_request_doc_id"] = "other-request".into();
                }
                "unindexed" => {
                    row["started_by_node_did"] = serde_json::Value::Null;
                    row["started_by_session_id"] = serde_json::Value::Null;
                }
                other => panic!("unmapped starter verification case {other}"),
            }
            row
        };
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let expected = case["accepted"].as_bool().unwrap();
            let candidate = candidate(name);
            let read = SessionRead(candidate.clone(), Default::default());
            let verified = gents::session_origin::verified_recorded_starter(&read, &scope)
                .await
                .unwrap();
            assert_eq!(verified.is_some(), expected, "{name}");
            let mut stored = candidate.clone();
            for field in ["_docID", "started_by_node_did", "started_by_session_id"] {
                stored.as_object_mut().unwrap().remove(field);
            }
            let stored: AgentSession = serde_json::from_value(stored).unwrap();
            let mut summaries = session_summaries(&[stored], &[], &session.node_did, &[], &[]);
            let cache = Default::default();
            super::super::runtime_tasks::resolve_summary_starters(&read, &cache, &mut summaries)
                .await;
            let summarized = summaries[0].started_by.as_ref();
            assert_eq!(summarized.is_some(), expected, "{name}");
            if let (Some(link), Some(view)) = (&verified, summarized) {
                assert_eq!(view.node_did, source.node_did);
                assert_eq!(view.session_id, source.session_id);
                assert_eq!(view.requester_did, source.requester_did);
                assert_eq!(view.cause_request_doc_id, "missing-request");
                assert_eq!(link.cause_request_doc_id, "missing-request");
            }
            let before = read.1.load(std::sync::atomic::Ordering::SeqCst);
            super::super::runtime_tasks::resolve_summary_starters(&read, &cache, &mut summaries)
                .await;
            assert_eq!(
                read.1.load(std::sync::atomic::Ordering::SeqCst),
                before,
                "{name}: a rebuild with verified evidence does not read"
            );
        }
    }
}
