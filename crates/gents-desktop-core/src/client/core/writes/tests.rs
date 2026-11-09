use super::*;
use crate::client::store::ClientStore;

fn peer_record(source: Option<&str>) -> PeerRecord {
    let mut record = PeerRecord::new("Amy", "endpoint-amy", "did:key:amy");
    record.source = source.map(str::to_owned);
    if source == Some("enrollment") {
        record.pairing_network_id = Some("network-a".into());
        record.enrollment_request_digest = Some("digest-a".into());
        record.enrollment_authorization_sequence = Some(1);
        record.enrollment_authorization_expires_at = Some("2099-09-29T00:00:00Z".into());
    }
    record
}

#[test]
fn ux_data_requires_the_installing_local_runtime() {
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let mut record = peer_record(Some("local-standard"));
    record.graphql = Some("http://127.0.0.1:1234/api/v0/graphql".into());
    assert!(ux_local_runtime_record(&[], &record.node_did, home.path()).is_err());
    assert!(ux_local_runtime_record(&[record.clone()], &record.node_did, home.path()).is_err());
    record.local_node_home = Some(other.path().display().to_string());
    assert!(ux_local_runtime_record(&[record.clone()], &record.node_did, home.path()).is_err());
    record.local_node_home = Some(home.path().display().to_string());
    assert!(ux_local_runtime_record(&[record.clone()], "did:key:other", home.path()).is_err());
    assert!(ux_local_runtime_record(&[record.clone()], &record.node_did, home.path()).is_ok());
    record.graphql = None;
    assert!(ux_local_runtime_record(&[record.clone()], &record.node_did, home.path()).is_err());
}

/// `ClientChatReadiness.readiness_ignores_route_observations`: the runtime's
/// ACP, not the desktop's copy of the enrollment lease, decides whether a
/// signed HTTP write is admitted; an enrollment without an endpoint has no
/// write path whatever its lease says.
#[test]
fn enrollment_lease_does_not_gate_the_http_write_path() {
    let mut peer = peer_record(Some("enrollment"));
    peer.pairing_ready = true;
    peer.enrollment_authorization_expires_at = Some("2026-08-30T12:00:01Z".into());
    let refused = ensure_peer_chat_ready(&peer.node_did, Some(&peer)).unwrap_err();
    assert_eq!(
        refused
            .downcast_ref::<ChatRouteRefusal>()
            .map(ChatRouteRefusal::cause),
        Some(ChatRouteCause::EndpointNotAdvertised)
    );
    assert!(refused.to_string().contains("endpoint_not_advertised"));

    peer.graphql = Some("http://127.0.0.1:1234/api/v0/graphql".into());
    assert!(ensure_peer_chat_ready(&peer.node_did, Some(&peer)).is_ok());
    assert_eq!(
        peer_record_owning_node(std::slice::from_ref(&peer), &peer.node_did),
        Some(peer.clone())
    );
}

#[test]
fn chat_write_fence_matches_lean() {
    let cases = crate::client::peer_directory::tests::chat_readiness_cases();
    assert!(cases.len() > 1000);
    assert!(cases.iter().any(|(record, _, _)| record.is_none()));
    for (record, _, refusal) in cases {
        let node_did = record
            .as_ref()
            .map_or("did:key:runtime", |record| record.node_did.as_str())
            .to_owned();
        let observed = chat_route_refusal(&node_did, record.as_ref());
        assert_eq!(
            observed
                .as_ref()
                .map(|refusal| refusal.cause().token().to_owned()),
            refusal,
            "{record:?}"
        );
        let ensured = ensure_peer_chat_ready(&node_did, record.as_ref());
        match &refusal {
            None => assert!(ensured.is_ok(), "{record:?}"),
            Some(token) => {
                let error = ensured.unwrap_err();
                let cause = error
                    .downcast_ref::<ChatRouteRefusal>()
                    .map(ChatRouteRefusal::cause)
                    .expect("a typed refusal");
                assert_eq!(cause.token(), token.as_str());
                assert!(
                    error.to_string().contains(token.as_str())
                        && error
                            .to_string()
                            .contains(cause.next_action().description()),
                    "the refusal must name its cause and next action: {error}"
                );
            }
        }
        let owning = peer_record_owning_node(record.as_slice(), &node_did);
        assert_eq!(owning.is_some(), refusal.is_none(), "{record:?}");
    }
}

#[test]
fn machine_pairing_never_claims_an_unlisted_node() {
    let mut machine = peer_record(Some("enrollment"));
    machine.pairing_template = Some("machine".to_string());
    machine.pairing_ready = true;
    assert!(peer_record_owning_node(&[machine], "did:key:child").is_none());
}

#[tokio::test]
async fn submitted_request_survives_refresh_failure_and_requests_scoped_resync() {
    let (store, _version) = ObservedStore::new(ClientStore::default());
    let mut wake = store.subscribe_resync_requests();
    let receipt = SubmittedRequest {
        request_id: "accepted-request".into(),
        session_id: "accepted-session".into(),
        node_did: "did:test:node".into(),
        agent_did: Some("agent".into()),
    };
    let before = store.projection_revision();
    let accepted = observe_submitted_request(&store, receipt.clone(), async {
        anyhow::bail!("projection unavailable after commit")
    })
    .await;
    assert_eq!(accepted, receipt);
    assert!(wake.has_changed().unwrap());
    wake.borrow_and_update();
    assert_eq!(
        store.take_resync_requests(),
        (false, vec!["did:test:node".to_owned()])
    );
    assert_eq!(store.projection_revision(), before);
    assert!(store.snapshot().requests.is_empty());
}

#[tokio::test]
async fn submitted_request_refresh_success_does_not_schedule_another_read() {
    let (store, _version) = ObservedStore::new(ClientStore::default());
    let wake = store.subscribe_resync_requests();
    let receipt = SubmittedRequest {
        request_id: "accepted-request".into(),
        session_id: "accepted-session".into(),
        node_did: "did:test:node".into(),
        agent_did: None,
    };
    let accepted = observe_submitted_request(&store, receipt.clone(), async {
        Ok(crate::client::StoreRefresh::Published { version: 1 })
    })
    .await;
    assert_eq!(accepted, receipt);
    assert!(!wake.has_changed().unwrap());
    assert_eq!(store.take_resync_requests(), (false, Vec::new()));
}
