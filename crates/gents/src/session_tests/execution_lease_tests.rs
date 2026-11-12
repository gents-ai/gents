use chrono::{DateTime, Utc};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::lifecycle::execution_lease::*;
use gents_session::request::lifecycle::test_support::{
    agent_access, agent_identity, claimed_owner_caused_by, owner, request_row, test_node,
    PendingNativeOutcome,
};
use gents_session::request::lifecycle::TriggerLineage;
use std::{sync::Arc, time::Duration};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

struct PendingNativeObserver(Arc<tokio::sync::Notify>);

#[tokio::test]
async fn lean_periodic_renewal_waits_for_abandoned_native_operation_before_next_attempt() {
    let cases = tokio::task::spawn_blocking(|| {
        gents_lean_vocab::lean_contract_snapshot()
            .native_completion_cases
            .clone()
    })
    .await
    .expect("Lean snapshot load");
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

mod workspace_recovery;
