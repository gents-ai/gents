//! The interleavings of TLA+ `HeldAudienceReads`, driven step by step on one
//! thread: a gated status read stays unanswered until the test opens it, and
//! the other readers run until they can make no progress.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use gents_protocol::delegation_audience::{
    CredentialAnswer, DelegationAudience, ReportedDelegationAudience,
};
use tokio::sync::Notify;

use super::ResolveAudience;
use crate::config_client::GraphqlEndpoint;

/// What one status read answers: the audience and provider's time the home
/// signs, once `gate` (when there is one) is opened.
struct Answer {
    audience: u64,
    provider_time: Option<u64>,
    gate: Option<Arc<Notify>>,
}

/// The home: each read takes the next scripted answer.
struct Scripted {
    node: crate::identity::KeyIdentity,
    answers: Mutex<VecDeque<Answer>>,
    entered: Notify,
    reads: AtomicUsize,
}

impl ResolveAudience for Scripted {
    fn resolve<'a>(
        &'a self,
        nonce: &'a gents_protocol::home_attestation::HomeNonce,
    ) -> BoxFuture<'a, Option<serde_json::Value>> {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted answer for every read");
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        Box::pin(async move {
            if let Some(gate) = &answer.gate {
                gate.notified().await;
            }
            Some(
                crate::test_support::signed_home_status(
                    &self.node,
                    &ReportedDelegationAudience::Named(audience(answer.audience)),
                    answer.provider_time,
                    nonce,
                )
                .await,
            )
        })
    }
}

fn audience(deployment: u64) -> DelegationAudience {
    DelegationAudience {
        worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
        deployment,
        genesis_id: None,
    }
}

async fn home(answers: Vec<Answer>) -> Arc<Scripted> {
    Arc::new(Scripted {
        node: tokio::task::spawn_blocking(|| crate::test_support::test_identity("ordered-home"))
            .await
            .unwrap(),
        answers: Mutex::new(answers.into()),
        entered: Notify::new(),
        reads: AtomicUsize::new(0),
    })
}

async fn endpoint(home: &Arc<Scripted>, held: u64) -> GraphqlEndpoint {
    let user = tokio::task::spawn_blocking(|| crate::test_support::test_identity("ordered-user"))
        .await
        .unwrap();
    let capability = crate::identity::NodeIdentity::client_signing_capability(&user).unwrap();
    GraphqlEndpoint::as_principal("http://127.0.0.1:9/api/v0/graphql", capability.did())
        .with_client_signing_capability(capability)
        .unwrap()
        .with_reported_delegation_audience(ReportedDelegationAudience::Named(audience(held)))
        .unwrap()
        .with_audience_resolver(crate::identity::NodeIdentity::did(&home.node), home.clone())
        .with_device_clock_for_test(Arc::new(|| 1_000))
}

/// Lets every other task on this thread run until none can go on.
async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

fn refused() -> CredentialAnswer {
    CredentialAnswer::of_status(403)
}

#[tokio::test(flavor = "current_thread")]
async fn a_read_answered_late_never_replaces_a_read_that_started_after_it() {
    let gate = Arc::new(Notify::new());
    let home = home(vec![
        Answer {
            audience: 2,
            provider_time: None,
            gate: Some(gate.clone()),
        },
        Answer {
            audience: 3,
            provider_time: None,
            gate: None,
        },
    ])
    .await;
    let endpoint = endpoint(&home, 1).await;
    let first = tokio::spawn({
        let endpoint = endpoint.clone();
        let presented = endpoint.delegation_audience.record();
        async move { endpoint.recovers(refused(), &presented).await }
    });
    home.entered.notified().await;
    let second = tokio::spawn({
        let endpoint = endpoint.clone();
        let presented = endpoint.delegation_audience.record();
        async move { endpoint.recovers(refused(), &presented).await }
    });
    settle().await;
    assert_eq!(
        home.reads.load(Ordering::SeqCst),
        1,
        "the second read waits for the first to be kept"
    );
    gate.notify_one();
    assert!(first.await.unwrap().unwrap(), "the first request resends");
    assert!(second.await.unwrap().unwrap(), "the second request resends");
    assert_eq!(home.reads.load(Ordering::SeqCst), 2);
    assert_eq!(
        endpoint.delegation_audience.get(),
        Some(audience(3)),
        "the newest status is the one kept, and what both resends mint for"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_clock_read_answered_late_never_replaces_a_later_one() {
    let gate = Arc::new(Notify::new());
    let home = home(vec![
        Answer {
            audience: 1,
            provider_time: Some(100),
            gate: Some(gate.clone()),
        },
        Answer {
            audience: 1,
            provider_time: Some(50_000),
            gate: None,
        },
    ])
    .await;
    let endpoint = endpoint(&home, 1).await;
    let dating = tokio::spawn({
        let endpoint = endpoint.clone();
        async move { endpoint.home_time().await }
    });
    home.entered.notified().await;
    let recovering = tokio::spawn({
        let endpoint = endpoint.clone();
        let presented = endpoint.delegation_audience.record();
        async move { endpoint.recovers(refused(), &presented).await }
    });
    settle().await;
    gate.notify_one();
    assert_eq!(dating.await.unwrap(), 100, "dated at the status it read");
    recovering.await.unwrap().unwrap();
    let (clock, _) = endpoint.delegation_audience.minting_clock();
    assert_eq!(
        clock.map(|clock| clock.provider_at_read),
        Some(50_000),
        "the clock of the later read is kept"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_dropped_read_keeps_nothing_and_frees_the_status_lock() {
    let gate = Arc::new(Notify::new());
    let home = home(vec![
        Answer {
            audience: 2,
            provider_time: None,
            gate: Some(gate.clone()),
        },
        Answer {
            audience: 3,
            provider_time: None,
            gate: None,
        },
    ])
    .await;
    let endpoint = endpoint(&home, 1).await;
    let dropped = tokio::spawn({
        let endpoint = endpoint.clone();
        let presented = endpoint.delegation_audience.record();
        async move { endpoint.recovers(refused(), &presented).await }
    });
    home.entered.notified().await;
    dropped.abort();
    assert!(dropped.await.unwrap_err().is_cancelled());
    let presented = endpoint.delegation_audience.record();
    assert!(endpoint.recovers(refused(), &presented).await.unwrap());
    assert_eq!(endpoint.delegation_audience.get(), Some(audience(3)));
    gate.notify_one();
    settle().await;
    assert_eq!(
        endpoint.delegation_audience.get(),
        Some(audience(3)),
        "the dropped read's late answer is kept by no one"
    );
}

/// TLA+ `HeldAudienceReads` `Queued`: two requests minted for the audience the
/// home left are refused; the second waits while the first reads and keeps
/// the new audience, so its own read changes nothing. It is sent again all
/// the same, since what it presented is not what is kept (gents#3963).
#[tokio::test(flavor = "current_thread")]
async fn a_refusal_queued_behind_another_is_sent_again_with_the_newest_audience() {
    let gate = Arc::new(Notify::new());
    let home = home(vec![
        Answer {
            audience: 2,
            provider_time: None,
            gate: Some(gate.clone()),
        },
        Answer {
            audience: 2,
            provider_time: None,
            gate: None,
        },
        Answer {
            audience: 2,
            provider_time: None,
            gate: None,
        },
    ])
    .await;
    let endpoint = endpoint(&home, 1).await;
    let first = tokio::spawn({
        let endpoint = endpoint.clone();
        let presented = endpoint.delegation_audience.record();
        async move { endpoint.recovers(refused(), &presented).await }
    });
    home.entered.notified().await;
    let second = tokio::spawn({
        let endpoint = endpoint.clone();
        let presented = endpoint.delegation_audience.record();
        async move { endpoint.recovers(refused(), &presented).await }
    });
    settle().await;
    gate.notify_one();
    assert!(first.await.unwrap().unwrap(), "the first request resends");
    assert!(
        second.await.unwrap().unwrap(),
        "the request queued behind it resends"
    );
    assert_eq!(home.reads.load(Ordering::SeqCst), 2);
    assert_eq!(endpoint.delegation_audience.get(), Some(audience(2)));
    let presented = endpoint.delegation_audience.record();
    assert!(
        !endpoint.recovers(refused(), &presented).await.unwrap(),
        "a request minted for the audience kept is told"
    );
}
