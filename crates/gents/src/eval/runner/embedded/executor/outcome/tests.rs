use super::*;
use crate::eval::runner::embedded::executor::tests::{assert_failure_class, seed_trigger};
use crate::eval::runner::embedded::home::EmbeddedHome;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use std::time::Duration;

#[test]
fn provider_reason_from_failure_maps_the_table() {
    assert_eq!(
        provider_reason_from_failure("HTTP 503"),
        Some(ProviderReason::Unavailable)
    );
    assert_eq!(
        provider_reason_from_failure("HTTP 429 rate limited"),
        Some(ProviderReason::Unavailable)
    );
    assert_eq!(
        provider_reason_from_failure("HTTP 408 request timeout"),
        Some(ProviderReason::Unavailable)
    );
    assert_eq!(
        provider_reason_from_failure("HTTP 400 context length"),
        Some(ProviderReason::Rejected)
    );
    assert_eq!(
        provider_reason_from_failure("content policy"),
        Some(ProviderReason::Rejected)
    );
    assert_eq!(provider_reason_from_failure("weird"), None);
    assert_eq!(provider_reason_from_failure("model gpt-4o-500k"), None);
}

#[test]
fn an_observation_that_failed_is_infrastructure() {
    let evidence = RequestEvidence::default();

    assert_eq!(
        stage_failure_kind(None, true, false, &evidence),
        Some(OutcomeKind::Infrastructure),
        "an observation that never returned says nothing about the subject"
    );
    assert_eq!(
        provider_reason(Some(OutcomeKind::Infrastructure), &evidence),
        None
    );
    assert_eq!(
        stage_failure_kind(None, false, false, &evidence),
        Some(OutcomeKind::Infrastructure),
        "evidence that could not be collected is infrastructure too"
    );
    assert_eq!(
        stage_failure_kind(None, true, true, &evidence),
        Some(OutcomeKind::Runtime),
        "a cancelled stage is the request not finishing"
    );
}

#[test]
fn a_lease_failure_uses_the_existing_infrastructure_outcome() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/eval_request_outcome.json"
    )))
    .unwrap();
    let case = cases
        .iter()
        .find(|case| {
            case["state"] == "failed"
                && case["deadline"] == false
                && case["request_reason"] == "execution lease expired"
                && case["inference_reason"].is_null()
                && case["inference_failed"] == false
                && case["tool_failed"] == false
        })
        .unwrap();
    let expected = OutcomeKind::parse(case["outcome"].as_str().unwrap()).unwrap();
    let evidence = RequestEvidence {
        failure_reason: Some(case["request_reason"].as_str().unwrap().into()),
        ..Default::default()
    };
    let terminal = TerminalObservation {
        terminal_state: Some(RequestLifecycleState::Failed),
        session_id: Some("session".into()),
        interrupted_on_deadline: false,
        elapsed: Duration::ZERO,
    };
    let outcome = stage_failure_kind(Some(&terminal), true, false, &evidence);
    assert_eq!(outcome, Some(expected));
    assert_eq!(provider_reason(outcome, &evidence), None);
    assert_failure_class(outcome, expected, crate::eval::EvidenceClass::NotEvidence);
}

#[test]
fn a_deadline_without_terminal_evidence_keeps_its_deadline_outcome() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/eval_request_outcome.json"
    )))
    .unwrap();
    let cases: Vec<_> = cases
        .iter()
        .filter(|case| {
            case["state"].is_null()
                && case["request_reason"].is_null()
                && case["inference_reason"].is_null()
                && case["inference_failed"] == false
                && case["tool_failed"] == false
        })
        .collect();
    assert_eq!(cases.len(), 2);
    for case in cases {
        let observed = TerminalObservation {
            terminal_state: None,
            session_id: Some("session".into()),
            interrupted_on_deadline: case["deadline"].as_bool().unwrap(),
            elapsed: Duration::ZERO,
        };
        let expected = OutcomeKind::parse(case["outcome"].as_str().unwrap()).unwrap();
        assert_eq!(
            stage_failure_kind(Some(&observed), true, false, &RequestEvidence::default()),
            Some(expected),
            "{case}"
        );
    }
}

#[tokio::test]
async fn an_errored_trigger_fails_a_completed_stage_as_runtime() {
    let home = EmbeddedHome::create_temp("trigger-error").await.unwrap();
    let since = "2026-06-01T00:00:00Z";
    assert_eq!(
        trigger_failure(&home.node, home.did(), since, None, "t1", "only").await,
        None
    );

    seed_trigger(&home, "healthy", "fired", None, since).await;
    assert_eq!(
        trigger_failure(&home.node, home.did(), since, None, "t1", "only").await,
        None
    );

    seed_trigger(
        &home,
        "broken",
        "error",
        Some("template did not render"),
        since,
    )
    .await;
    assert_eq!(
        trigger_failure(&home.node, home.did(), since, None, "t1", "only").await,
        Some(OutcomeKind::Runtime)
    );
    assert_eq!(
        trigger_failure(
            &home.node,
            "did:key:zSomeoneElse",
            since,
            None,
            "t1",
            "only"
        )
        .await,
        None,
        "another node's trigger is not this trial's"
    );
    home.node.shutdown().await;
}

#[tokio::test]
async fn a_seed_ignores_unrelated_and_stale_trigger_failures() {
    let home = EmbeddedHome::create_temp("seed-trigger-scope")
        .await
        .unwrap();
    let since = "2026-06-01T00:00:00Z";
    let source = "seed-\"current";
    let unacknowledged = crate::triggers::engine::UNACKNOWLEDGED_STATUS;
    for (trigger_id, status, doc_id, attempted) in [
        ("unrelated", "error", "another-seed", since),
        ("stale", "error", source, "2026-01-01T00:00:00Z"),
        (
            "unrelated-unacknowledged",
            unacknowledged,
            "another-seed",
            since,
        ),
        (
            "stale-unacknowledged",
            unacknowledged,
            source,
            "2026-01-01T00:00:00Z",
        ),
    ] {
        seed_trigger(
            &home,
            trigger_id,
            status,
            Some("unrelated failure"),
            attempted,
        )
        .await;
        crate::documents::update_trigger_runtime_fields(
            &home.node,
            home.did(),
            trigger_id,
            crate::documents::TriggerRuntimeUpdate {
                last_fired_source_doc_id: Some(doc_id.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(
        trigger_failure(&home.node, home.did(), since, Some(source), "t1", "seed").await,
        None,
        "another document's fire, and one attempted before the stage, is not this stage's"
    );
    crate::documents::update_trigger_runtime_fields(
        &home.node,
        home.did(),
        "stale",
        crate::documents::TriggerRuntimeUpdate {
            last_attempt_at: Some(since.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_failure_class(
        trigger_failure(&home.node, home.did(), since, Some(source), "t1", "seed").await,
        OutcomeKind::Runtime,
        crate::eval::EvidenceClass::Fail,
    );
    home.node.shutdown().await;
}

#[tokio::test]
async fn an_unreadable_seed_trigger_status_is_infrastructure() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let failure = trigger_failure(
        &node,
        "did:key:trial",
        "2026-06-01T00:00:00Z",
        Some("seed"),
        "t1",
        "seed",
    )
    .await;
    node.shutdown().await;
    assert_failure_class(
        failure,
        OutcomeKind::Infrastructure,
        crate::eval::EvidenceClass::NotEvidence,
    );
}

#[tokio::test]
async fn a_seed_unacknowledged_trigger_is_infrastructure() {
    let home = EmbeddedHome::create_temp("seed-trigger-unacknowledged")
        .await
        .unwrap();
    let since = "2026-06-01T00:00:00Z";
    let source = "seed-current";
    seed_trigger(
        &home,
        "unacknowledged",
        crate::triggers::engine::UNACKNOWLEDGED_STATUS,
        Some("acknowledgment channel closed"),
        since,
    )
    .await;
    crate::documents::update_trigger_runtime_fields(
        &home.node,
        home.did(),
        "unacknowledged",
        crate::documents::TriggerRuntimeUpdate {
            last_fired_source_doc_id: Some(source.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_failure_class(
        trigger_failure(&home.node, home.did(), since, Some(source), "t1", "seed").await,
        OutcomeKind::Infrastructure,
        crate::eval::EvidenceClass::NotEvidence,
    );
    home.node.shutdown().await;
}

#[tokio::test]
async fn an_unacknowledged_seed_fire_does_not_downgrade_a_recorded_error() {
    let home = EmbeddedHome::create_temp("seed-trigger-precedence")
        .await
        .unwrap();
    let since = "2026-06-01T00:00:00Z";
    let source = "seed-current";
    for (trigger_id, status) in [
        ("lost", crate::triggers::engine::UNACKNOWLEDGED_STATUS),
        ("caused", "error"),
    ] {
        seed_trigger(
            &home,
            trigger_id,
            status,
            Some("recorded alongside the other"),
            since,
        )
        .await;
        crate::documents::update_trigger_runtime_fields(
            &home.node,
            home.did(),
            trigger_id,
            crate::documents::TriggerRuntimeUpdate {
                last_fired_source_doc_id: Some(source.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    assert_failure_class(
        trigger_failure(&home.node, home.did(), since, Some(source), "t1", "seed").await,
        OutcomeKind::Runtime,
        crate::eval::EvidenceClass::Fail,
    );
    home.node.shutdown().await;
}

#[tokio::test]
async fn a_trigger_error_from_before_the_stage_does_not_fail_it() {
    let home = EmbeddedHome::create_temp("trigger-stale").await.unwrap();
    seed_trigger(&home, "stale", "error", Some("old"), "2026-01-01T00:00:00Z").await;

    assert_eq!(
        trigger_failure(
            &home.node,
            home.did(),
            "2026-06-01T00:00:00Z",
            None,
            "t1",
            "only"
        )
        .await,
        None
    );
    home.node.shutdown().await;
}
