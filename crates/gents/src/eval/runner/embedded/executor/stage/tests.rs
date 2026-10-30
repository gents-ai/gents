use super::*;
use crate::eval::runner::embedded::executor::close;
use crate::eval::runner::embedded::executor::install::{install, install_schema};
use crate::eval::runner::embedded::executor::tests::{
    assert_failure_class, frozen_binding, materialized_digest, seed_request_with_state,
    write_slot_pack,
};
use crate::eval::runner::embedded::executor::EventSourcesReady;
use crate::eval::runner::embedded::home::boot_runtime;
use crate::eval::runner::embedded::observe::{
    classify_request_outcome, request_evidence_from_query_data,
};
use crate::eval::runner::executor::{CaptureResult, FixtureDocument};
use crate::graphql::graphql_with_transaction_retry;
use crate::lifecycle::{
    build_signed_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
};
use crate::{ConfigAccess, DocumentRuntimeOptions};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn a_cancelled_seed_stage_is_runtime_before_any_request_exists() {
    let home = EmbeddedHome::create_temp("seed-cancel").await.unwrap();
    let cancel = CancellationToken::new();
    let stage = StageSpec {
        stage_id: "fire".into(),
        prompt: String::new(),
        seed: Some(FixtureDocument {
            collection: "NoSuchItem".into(),
            document: json!({}),
        }),
        deadline_secs: 120,
        settle: false,
        continuation: None,
        review_previous: false,
        captures: Vec::new(),
    };
    let locator = TrialLocator {
        trial_node_did: home.did().to_string(),
        session_id: "s".into(),
        home_hint: None,
    };
    let (_ready_tx, never_ready) = watch::channel(None);
    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        canceller.cancel();
    });
    let observed = tokio::time::timeout(
        Duration::from_secs(5),
        submit_and_observe(
            &TrialSpec::empty_for_tests("t1"),
            &cancel,
            &home,
            &exited_runtime(),
            &locator,
            &stage,
            &never_ready,
        ),
    )
    .await
    .expect("the cancel ends the stage at once");
    assert_eq!(
        (observed.request_id, observed.failure_kind),
        (None, Some(OutcomeKind::Runtime))
    );
    assert_eq!(observed.terminal_state, None);
}

#[tokio::test]
async fn trial_teardown_settles_scheduled_inference_without_expiring_its_lease() {
    use crate::support::streaming_backend::{
        MockStreamingBackend, StreamPlan, StreamResponse, StreamScript,
    };

    for cancel_stage in [false, true] {
        let marker = "scheduled-trial-teardown";
        let backend = MockStreamingBackend::start_with_plans(
            "frozen-model",
            vec![StreamPlan::new(
                marker,
                vec![StreamResponse::Stream(StreamScript::paused(
                    marker,
                    ["received before shutdown"],
                ))],
            )],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pack_dir = dir.path().join("pack");
        write_slot_pack(&pack_dir, "gents:inference-slot:primary");
        let mut inference = frozen_binding(json!("frozen-profile"));
        inference.backend["endpoint"] = json!(backend.endpoint());
        let spec = TrialSpec {
            pack_digest: materialized_digest(&pack_dir),
            pack_dir,
            agent_id: "subject".into(),
            inference,
            ..TrialSpec::empty_for_tests("scheduled-teardown")
        };
        let home = EmbeddedHome::create_temp("scheduled-teardown")
            .await
            .unwrap();
        install(&spec, &home, &dir.path().join("workspace"))
            .await
            .unwrap();
        let (runtime, _agent) = boot_runtime(
            &home,
            home.identity.clone(),
            DocumentRuntimeOptions::default(),
        )
        .await
        .unwrap();
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = build_signed_request(
            RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                RequestIdentity {
                    request_id: request_id.clone(),
                    node_did: home.did().into(),
                    requester_did: None,
                    agent_id: "subject".into(),
                    session_id: uuid::Uuid::new_v4().to_string(),
                    content: marker.into(),
                    execution_origin: ExecutionOrigin::Scheduled,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                    home.did(),
                ),
            ),
            RequestSigner::RegisteredTarget,
        )
        .await
        .unwrap();
        ConfigAccess::write_local(
            &home.node,
            "test.eval_scheduled_shutdown",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            while backend.observed_chunks(marker) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("scheduled inference must reach its paused provider stream");
        let before = graphql_with_transaction_retry(
            &home.node,
            &crate::eval::runner::embedded::observe::evidence_query(&request_id),
            "test.eval_scheduled_inference",
        )
        .await
        .unwrap();
        let before = request_evidence_from_query_data(before.data.as_ref().unwrap());
        assert!(before
            .inference_calls
            .iter()
            .any(|call| call.call_state.as_deref() == Some("running")));

        if cancel_stage {
            let cancel = CancellationToken::new();
            cancel.cancel();
            let (terminal, stopped) = tokio::time::timeout(
                Duration::from_secs(30),
                observe_stage_terminal(
                    &home.node,
                    &cancel,
                    &runtime,
                    &request_id,
                    Duration::from_secs(120),
                ),
            )
            .await
            .expect("stage cancellation must settle active inference");
            assert!(stopped);
            assert_eq!(
                terminal.unwrap().terminal_state,
                Some(RequestLifecycleState::Interrupted)
            );
        }

        tokio::time::timeout(Duration::from_secs(30), runtime.shutdown())
            .await
            .expect("trial shutdown must settle active inference")
            .unwrap();
        let state = poll_request(&home.node, &request_id)
            .await
            .unwrap()
            .unwrap();
        let terminal_state =
            RequestLifecycleState::parse(state.lifecycle_state.as_deref().unwrap()).unwrap();
        assert!(terminal_state.is_terminal());
        let evidence = collect_request_evidence(&home.node, &request_id)
            .await
            .unwrap();
        assert!(!evidence.inference_calls.is_empty());
        assert!(evidence.inference_calls.iter().all(|call| {
            matches!(
                call.call_state.as_deref(),
                Some("completed" | "failed" | "cancelled")
            ) && call.ended_at.is_some()
        }));
        assert_ne!(
            classify_request_outcome(Some(terminal_state), false, &evidence),
            Some("infrastructure"),
            "{evidence:?}"
        );
        close(home).await;
    }
}

#[tokio::test]
async fn a_stage_that_could_not_be_submitted_still_runs_its_captures() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let home = EmbeddedHome::create_temp("submit-failure").await.unwrap();
    seed_request(&home, "seeded").await;

    let spec = TrialSpec::empty_for_tests("t1");
    let locator = TrialLocator {
        trial_node_did: "did:key:zUnregistered".to_string(),
        session_id: "s-1".to_string(),
        home_hint: None,
    };
    let stage = StageSpec {
        stage_id: "only".to_string(),
        prompt: "hello".to_string(),
        seed: None,
        deadline_secs: 1,
        settle: false,
        continuation: None,
        review_previous: false,
        captures: vec![
            Capture::Documents {
                name: "requests".to_string(),
                collection: "AgentRequest".to_string(),
                filter: json!({"request_id": {"_eq": "seeded"}}),
                fields: vec!["agent_id".to_string()],
            },
            Capture::Documents {
                name: "unreadable".to_string(),
                collection: "NoSuchCollection".to_string(),
                filter: json!({}),
                fields: Vec::new(),
            },
        ],
    };

    let runtime = exited_runtime();
    let evidence = run_stage(
        &spec,
        &StageEnv {
            cancel: &CancellationToken::new(),
            home: &home,
            runtime: &runtime,
            locator: &locator,
            workspace: &workspace,
            ready: &watch::channel(None).1,
        },
        &stage,
    )
    .await;

    assert_eq!(evidence.stage_id, "only");
    assert_eq!(evidence.request_id, None);
    assert_failure_class(
        evidence.failure_kind,
        OutcomeKind::Infrastructure,
        crate::eval::EvidenceClass::NotEvidence,
    );
    let CaptureResult::Documents { rows } = &evidence.captures["requests"] else {
        panic!("expected a document capture, got {:?}", evidence.captures);
    };
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["agent_id"], "seeded-agent");
    assert!(
        !evidence.captures.contains_key("unreadable"),
        "{:?}",
        evidence.captures
    );
    home.node.shutdown().await;
}

#[tokio::test]
async fn a_runtime_that_exits_mid_stage_fails_the_stage_as_runtime_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let home = EmbeddedHome::create_temp("runtime-exit").await.unwrap();
    let locator = TrialLocator {
        trial_node_did: home.did().to_string(),
        session_id: "s-1".to_string(),
        home_hint: None,
    };
    let stage = StageSpec {
        stage_id: "only".to_string(),
        prompt: "hello".to_string(),
        seed: None,
        deadline_secs: 600,
        settle: false,
        continuation: None,
        review_previous: false,
        captures: Vec::new(),
    };
    let runtime = exited_runtime();

    let evidence = tokio::time::timeout(
        Duration::from_secs(30),
        run_stage(
            &TrialSpec {
                agent_id: "subject".to_string(),
                ..TrialSpec::empty_for_tests("t1")
            },
            &StageEnv {
                cancel: &CancellationToken::new(),
                home: &home,
                runtime: &runtime,
                locator: &locator,
                workspace: &workspace,
                ready: &watch::channel(None).1,
            },
            &stage,
        ),
    )
    .await
    .expect("a dead runtime is not waited out to the stage deadline");

    assert!(evidence.request_id.is_some(), "the stage was submitted");
    assert_eq!(evidence.failure_kind, Some(OutcomeKind::Runtime));
    assert_eq!(evidence.provider_reason, None);
    home.node.shutdown().await;
}

#[tokio::test]
async fn a_seed_template_failure_ends_the_stage_before_its_deadline() {
    let backend = crate::support::streaming_backend::MockStreamingBackend::start_with_plans(
        "frozen-model",
        Vec::new(),
    )
    .unwrap();
    let mut inference = frozen_binding(json!("frozen-profile"));
    inference.backend["endpoint"] = json!(backend.endpoint());
    let dir = tempfile::tempdir().unwrap();
    let pack_dir = dir.path().join("pack");
    write_slot_pack(&pack_dir, "gents:inference-slot:primary");
    let path = pack_dir.join("pack_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["tasks"] = json!([{
        "task_id": "seed-task",
        "agent_id": "subject",
        "prompt_template": "{{ doc.payload.required }}"
    }]);
    config["event_sources"] = json!([{
        "event_source_id": "seed-source",
        "source_collection": "EvalSeed"
    }]);
    config["triggers"] = json!([{
        "trigger_id": "seed-trigger",
        "task_id": "seed-task",
        "source": {"kind": "event", "event_source_id": "seed-source"}
    }]);
    std::fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        agent_id: "subject".into(),
        inference,
        ..TrialSpec::empty_for_tests("seed-template-failure")
    };
    let home = EmbeddedHome::create_temp("seed-template-failure")
        .await
        .unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    install_schema(
        &ConfigAccess::local(home.node.clone()),
        "type EvalSeed { payload: JSON }",
    )
    .await
    .unwrap();
    install(&spec, &home, &workspace).await.unwrap();
    let (event_sources_ready, ready) = watch::channel(None);
    let options = DocumentRuntimeOptions {
        runtime_snapshot_observer: Some(Arc::new(EventSourcesReady(event_sources_ready))),
        ..Default::default()
    };
    let (runtime, _agent) = crate::eval::runner::embedded::home::boot_runtime_within(
        &home,
        home.identity.clone(),
        options,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    let locator = TrialLocator {
        trial_node_did: home.did().to_owned(),
        session_id: "seed-session".into(),
        home_hint: None,
    };
    let stage = StageSpec {
        stage_id: "seed".into(),
        prompt: String::new(),
        seed: Some(FixtureDocument {
            collection: "EvalSeed".into(),
            document: json!({"payload": {}}),
        }),
        deadline_secs: 600,
        settle: false,
        continuation: None,
        review_previous: false,
        captures: vec![Capture::Documents {
            name: "trigger".into(),
            collection: "Trigger".into(),
            filter: json!({"trigger_id": {"_eq": "seed-trigger"}}),
            fields: vec![
                "last_status".into(),
                "last_error".into(),
                "last_fired_source_doc_id".into(),
            ],
        }],
    };
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        run_stage(
            &spec,
            &StageEnv {
                cancel: &CancellationToken::new(),
                home: &home,
                runtime: &runtime,
                locator: &locator,
                workspace: &workspace,
                ready: &ready,
            },
            &stage,
        ),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), runtime.shutdown())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "fixture runtime did not stop; stage: {result:?}; event sources: {:?}",
                *ready.borrow()
            )
        })
        .unwrap();
    home.node.shutdown().await;

    let evidence = result.expect("the failed trigger must end the seed wait promptly");
    assert_failure_class(
        evidence.failure_kind,
        OutcomeKind::Runtime,
        crate::eval::EvidenceClass::Fail,
    );
    assert_eq!(evidence.request_id, None);
    assert!(evidence.inference_calls.is_empty());
    assert!(backend.observed_completion_bodies().is_empty());
    let CaptureResult::Documents { rows } = &evidence.captures["trigger"] else {
        panic!("the failed stage must retain its trigger evidence");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["last_status"], "error");
    assert!(
        rows[0]["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("template:")),
        "{rows:?}"
    );
    assert!(rows[0]["last_fired_source_doc_id"].as_str().is_some());
}

#[tokio::test]
async fn a_request_that_settled_before_its_runtime_exited_keeps_its_outcome() {
    let home = EmbeddedHome::create_temp("settled-then-exit")
        .await
        .unwrap();
    let request_id = "settled-request";
    seed_request(&home, request_id).await;
    let runtime = exited_runtime();
    while !runtime.handle.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(runtime.handle.is_finished());

    tokio::time::pause();
    let (observed, stopped) = observe_stage_terminal(
        &home.node,
        &CancellationToken::new(),
        &runtime,
        request_id,
        Duration::from_secs(600),
    )
    .await;
    tokio::time::resume();

    let observed = observed.expect("settled request should remain observable after exit");
    assert_eq!(
        observed.terminal_state,
        Some(RequestLifecycleState::Completed)
    );
    assert!(!stopped);

    let evidence = collect_request_evidence(&home.node, request_id)
        .await
        .expect("evidence should be gathered after virtual time resumes");
    assert_eq!(
        stage_failure_kind(Some(&observed), true, stopped, &evidence),
        None
    );
    home.node.shutdown().await;
}

fn exited_runtime() -> RunningRuntime {
    RunningRuntime {
        shutdown: tokio::sync::watch::channel(false).0,
        handle: tokio::spawn(async { Err(anyhow!("runtime killed")) }),
        node_did: String::new(),
    }
}

async fn seed_request(home: &EmbeddedHome, request_id: &str) {
    seed_request_with_state(home, request_id, "completed").await;
}
