use super::*;

#[tokio::test]
async fn pairing_retries_a_failed_attempt_within_its_bounds() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let cancel = tokio_util::sync::CancellationToken::new();
    let calls = AtomicU32::new(0);
    retry_managed_pairing(&cancel, 4, Duration::from_millis(1), || async {
        if calls.fetch_add(1, Ordering::SeqCst) < 2 {
            Err(PairingFailure::Transient("runtime_not_ready".to_string()))
        } else {
            Ok(())
        }
    })
    .await
    .expect("a later attempt pairs");
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let calls = AtomicU32::new(0);
    let error = retry_managed_pairing(&cancel, 3, Duration::from_millis(1), || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Err::<(), _>(PairingFailure::Transient("offer_mint_failed".to_string()))
    })
    .await
    .expect_err("attempts are bounded");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let error = error.to_string();
    assert!(
        error.contains("offer_mint_failed") && error.contains("3 attempts"),
        "{error}"
    );
}

fn managed_request(
    request_id: &str,
    server_peer: &str,
    state: &str,
    expires_at: &str,
) -> EnrollmentRequestResult {
    EnrollmentRequestResult {
        request_id: request_id.to_string(),
        network_id: "network".to_string(),
        admin_did: "did:key:admin".to_string(),
        server_peer: server_peer.to_string(),
        owner_node: "did:key:managed".to_string(),
        state: state.to_string(),
        expires_at: expires_at.to_string(),
        activation_error: None,
    }
}

#[tokio::test]
async fn a_failed_request_lookup_never_authors_another_request() {
    let authored = std::sync::atomic::AtomicU32::new(0);
    let cancel = tokio_util::sync::CancellationToken::new();
    let failure = resolve_managed_request(
        &cancel,
        || async { Err("decoding persisted enrollment request".to_string()) },
        |_| async { Ok(()) },
        || async {
            authored.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("enroll-new".to_string())
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(failure, PairingFailure::Transient(_)), "{failure}");
    assert_eq!(authored.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_stop_during_the_request_lookup_authors_and_pushes_nothing() {
    use std::sync::atomic::{AtomicU32, Ordering};

    for existing in [
        None,
        Some(managed_request(
            "enroll-1",
            "peer-a",
            "pending_approval",
            "2026-09-24T12:00:00Z",
        )),
    ] {
        let cancel = tokio_util::sync::CancellationToken::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let (authored, resent) = (AtomicU32::new(0), AtomicU32::new(0));
        let resolving = resolve_managed_request(
            &cancel,
            || async move {
                entered_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok(existing)
            },
            |_| {
                resent.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
            || async {
                authored.fetch_add(1, Ordering::SeqCst);
                Ok("enroll-new".to_string())
            },
        );
        let stop = async {
            entered_rx.await.unwrap();
            cancel.cancel();
            release_tx.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(resolving, stop);
        assert_eq!(result.unwrap_err(), PairingFailure::Cancelled);
        assert_eq!(authored.load(Ordering::SeqCst), 0);
        assert_eq!(resent.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn a_request_whose_push_failed_is_pushed_again_when_reused() {
    let store = std::sync::Mutex::new(None::<EnrollmentRequestResult>);
    let (resent, authored) = (
        std::sync::Mutex::new(Vec::<String>::new()),
        std::sync::atomic::AtomicU32::new(0),
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let attempt = || {
        resolve_managed_request(
            &cancel,
            || async { Ok(store.lock().unwrap().clone()) },
            |request_id| {
                resent.lock().unwrap().push(request_id);
                async { Ok(()) }
            },
            || async {
                authored.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                *store.lock().unwrap() = Some(managed_request(
                    "enroll-1",
                    "peer-a",
                    "pending_approval",
                    "2026-09-24T12:00:00Z",
                ));
                Err(PairingFailure::Transient(
                    "pushing enrollment request".to_string(),
                ))
            },
        )
    };
    assert!(attempt().await.is_err());
    assert_eq!(attempt().await.unwrap(), "enroll-1");
    assert_eq!(authored.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(*resent.lock().unwrap(), vec!["enroll-1".to_string()]);

    *store.lock().unwrap() = Some(managed_request(
        "enroll-1",
        "peer-a",
        "approved",
        "2026-09-24T12:00:00Z",
    ));
    assert_eq!(attempt().await.unwrap(), "enroll-1");
    assert_eq!(resent.lock().unwrap().len(), 1);
}

#[test]
fn the_newest_request_for_the_live_server_peer_is_reused() {
    let requests = vec![
        managed_request(
            "enroll-old",
            "peer-a",
            "pending_approval",
            "2026-09-24T10:00:00Z",
        ),
        managed_request(
            "enroll-new",
            "peer-a",
            "pending_approval",
            "2026-09-24T11:00:00Z",
        ),
        managed_request(
            "enroll-other",
            "peer-b",
            "pending_approval",
            "2026-09-24T12:00:00Z",
        ),
    ];
    let selected = select_managed_request(requests.clone(), "did:key:managed", Some("peer-a"));
    assert_eq!(selected.unwrap().request_id, "enroll-new");
    assert!(select_managed_request(requests.clone(), "did:key:managed", Some("peer-c")).is_none());
    assert!(select_managed_request(requests.clone(), "did:key:other", Some("peer-a")).is_none());
    assert!(
        select_managed_request(requests, "did:key:managed", None).is_none(),
        "without the live peer no request is reused"
    );
}

#[tokio::test]
async fn pairing_is_not_retried_after_a_permanent_failure() {
    use std::sync::atomic::{AtomicU32, Ordering};

    for message in [
        "requesting managed runtime enrollment: refusing to enroll with an incompatible runtime",
        "loading managed runtime enrollment offer: the running agent (v0.18.2) predates this app; restart it so it runs this version",
        "requesting managed runtime enrollment: enrollment server DID does not match the offer",
    ] {
        let cancel = tokio_util::sync::CancellationToken::new();
        let calls = AtomicU32::new(0);
        let failure = PairingFailure::classify(message.to_string());
        assert!(matches!(failure, PairingFailure::Permanent(_)), "{message}");
        let error = retry_managed_pairing(&cancel, 4, Duration::from_millis(1), || {
            calls.fetch_add(1, Ordering::SeqCst);
            let failure = failure.clone();
            async move { Err::<(), _>(failure) }
        })
        .await
        .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{message}");
        assert_eq!(error.to_string(), message);
    }
    assert!(matches!(
        PairingFailure::classify(
            "approving managed runtime enrollment: connection refused".to_string()
        ),
        PairingFailure::Transient(_)
    ));
}

#[tokio::test]
async fn pairing_retry_ends_when_cancelled_during_its_backoff() {
    let cancel = tokio_util::sync::CancellationToken::new();
    let retry = retry_managed_pairing(&cancel, 4, Duration::from_secs(60), || async {
        Err::<(), _>(PairingFailure::Transient("runtime_not_ready".to_string()))
    });
    let cancelled = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(retry, cancelled)
    })
    .await
    .expect("cancellation ends the backoff");
    assert_eq!(result.unwrap_err(), PairingFailure::Cancelled);
}
