use super::*;
use crate::runner::embedded::executor::tests::{assert_failure_class, seed_trigger};
use crate::runner::embedded::home::EmbeddedHome;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use std::time::Duration;

#[test]
fn provider_reason_from_failure_maps_the_table() {
    assert_eq!(
        provider_reason_from_failure("HTTP 503")
            .map(|stated| (stated.reason, stated.ended_by_backend)),
        Some((ProviderReason::Unavailable, false))
    );
    assert_eq!(
        provider_reason_from_failure("HTTP 429 rate limited")
            .map(|stated| (stated.reason, stated.ended_by_backend)),
        Some((ProviderReason::Unavailable, false))
    );
    assert_eq!(
        provider_reason_from_failure("HTTP 408 request timeout")
            .map(|stated| (stated.reason, stated.ended_by_backend)),
        Some((ProviderReason::Unavailable, false))
    );
    assert_eq!(
        provider_reason_from_failure("HTTP 400 context length")
            .map(|stated| (stated.reason, stated.ended_by_backend)),
        Some((ProviderReason::Rejected, false))
    );
    assert_eq!(
        provider_reason_from_failure("content policy")
            .map(|stated| (stated.reason, stated.ended_by_backend)),
        Some((ProviderReason::Rejected, false))
    );
    assert_eq!(
        provider_reason_from_failure("BackendGone")
            .map(|stated| (stated.reason, stated.ended_by_backend)),
        Some((ProviderReason::Unavailable, true))
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
        stage_outcome(
            false,
            Some(OutcomeKind::Infrastructure),
            stated_provider_reason(&evidence).map(|stated| stated.reason),
            Some(&[])
        ),
        Some((OutcomeKind::Infrastructure, None))
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
        "/../gents/tests/fixtures/eval_request_outcome.json"
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
        parked_for_backend: false,
        elapsed: Duration::ZERO,
    };
    let outcome = stage_failure_kind(Some(&terminal), true, false, &evidence);
    assert_eq!(outcome, Some(expected));
    assert_eq!(
        stage_outcome(
            false,
            outcome,
            stated_provider_reason(&evidence).map(|stated| stated.reason),
            Some(&[])
        ),
        Some((expected, None))
    );
    assert_failure_class(outcome, expected, crate::EvidenceClass::NotEvidence);
}

#[test]
fn a_deadline_without_terminal_evidence_keeps_its_deadline_outcome() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gents/tests/fixtures/eval_request_outcome.json"
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
            parked_for_backend: false,
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

async fn fires(
    home: &EmbeddedHome,
    node_did: &str,
    since: &str,
    source: Option<&str>,
) -> FailedFires {
    failed_fires(&home.node, node_did, since, source)
        .await
        .unwrap()
}

#[tokio::test]
async fn failed_fires_reads_this_nodes_failed_triggers_with_their_errors() {
    let home = EmbeddedHome::create_temp("trigger-error").await.unwrap();
    let since = "2026-06-01T00:00:00Z";
    assert_eq!(fires(&home, home.did(), since, None).await.failures, []);

    seed_trigger(&home, "healthy", "fired", None, since).await;
    let healthy = fires(&home, home.did(), since, None).await;
    assert_eq!((healthy.failures, healthy.detail), (vec![], None));

    seed_trigger(
        &home,
        "broken",
        "error",
        Some("template did not render"),
        since,
    )
    .await;
    let broken = fires(&home, home.did(), since, None).await;
    assert_eq!(broken.failures, [FireFailure::Error]);
    assert_eq!(
        broken.detail.as_deref(),
        Some("trigger broken error: template did not render"),
        "the verdict carries the trigger's own recorded error"
    );
    assert_eq!(
        fires(&home, "did:key:zSomeoneElse", since, None)
            .await
            .failures,
        [],
        "another node's trigger is not this trial's"
    );
    home.node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn a_seed_ignores_unrelated_and_stale_trigger_failures() {
    let home = EmbeddedHome::create_temp("seed-trigger-scope")
        .await
        .unwrap();
    let since = "2026-06-01T00:00:00Z";
    let source = "seed-\"current";
    let unacknowledged = gents_automation::triggers::UNACKNOWLEDGED_STATUS;
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
        gents_automation::test_support::update_trigger_runtime_fields(
            &home.node,
            home.did(),
            trigger_id,
            gents_automation::test_support::TriggerRuntimeUpdate {
                last_fired_source_doc_id: Some(doc_id.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(
        fires(&home, home.did(), since, Some(source)).await.failures,
        [],
        "another document's fire, and one attempted before the stage, is not this stage's"
    );
    gents_automation::test_support::update_trigger_runtime_fields(
        &home.node,
        home.did(),
        "stale",
        gents_automation::test_support::TriggerRuntimeUpdate {
            last_attempt_at: Some(since.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        fires(&home, home.did(), since, Some(source)).await.failures,
        [FireFailure::Error]
    );
    home.node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn an_unreadable_trigger_status_is_an_error() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    let read = failed_fires(&node, "did:key:trial", "2026-06-01T00:00:00Z", Some("seed")).await;
    node.shutdown().await.expect("shutdown node");
    assert!(read.is_err(), "{read:?}");
}

#[tokio::test]
async fn an_unacknowledged_fire_reads_as_the_runtime_losing_it() {
    let home = EmbeddedHome::create_temp("seed-trigger-unacknowledged")
        .await
        .unwrap();
    let since = "2026-06-01T00:00:00Z";
    let source = "seed-current";
    for (trigger_id, status) in [
        ("lost", gents_automation::triggers::UNACKNOWLEDGED_STATUS),
        ("spent", gents_documents::BUDGET_EXHAUSTED_STATUS),
    ] {
        seed_trigger(&home, trigger_id, status, Some("recorded"), since).await;
        gents_automation::test_support::update_trigger_runtime_fields(
            &home.node,
            home.did(),
            trigger_id,
            gents_automation::test_support::TriggerRuntimeUpdate {
                last_fired_source_doc_id: Some(source.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let mut failures = fires(&home, home.did(), since, Some(source)).await.failures;
    failures.sort_by_key(|failure| format!("{failure:?}"));
    assert_eq!(
        failures,
        [FireFailure::BudgetExhausted, FireFailure::Unacknowledged]
    );
    home.node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn a_trigger_error_from_before_the_stage_is_not_read() {
    let home = EmbeddedHome::create_temp("trigger-stale").await.unwrap();
    seed_trigger(&home, "stale", "error", Some("old"), "2026-01-01T00:00:00Z").await;
    assert_eq!(
        fires(&home, home.did(), "2026-06-01T00:00:00Z", None)
            .await
            .failures,
        []
    );
    home.node.shutdown().await.expect("shutdown node");
}

#[test]
fn stage_outcomes_match_the_lean_model() {
    gents_lean_vocab::families::eval_stage_outcome::replay::<
        FireFailure,
        OutcomeKind,
        ProviderReason,
    >(|input| {
        stage_outcome(
            input.ended_by_backend,
            input.request,
            input.stated,
            input.triggers_read.then_some(input.failures.as_slice()),
        )
        .map(|(kind, provider_reason)| {
            gents_lean_vocab::families::eval_stage_outcome::StageFailure {
                kind,
                provider_reason,
            }
        })
    });
}
