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

#[tokio::test(start_paused = true)]
async fn expired_enrollment_cannot_admit_a_request_without_waiting_for_the_sweep() {
    let mut peer = peer_record(Some("enrollment"));
    peer.pairing_ready = true;
    peer.enrollment_authorization_expires_at = Some("2026-08-30T12:00:01Z".into());
    let before = "2026-08-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let expired = "2026-08-30T12:00:01Z".parse::<DateTime<Utc>>().unwrap();

    assert!(ensure_peer_chat_ready_at(&peer.node_did, Some(&peer), before).is_ok());
    assert!(ensure_peer_chat_ready_at(&peer.node_did, Some(&peer), expired).is_err());
    assert!(
        peer_record_owning_node_at(std::slice::from_ref(&peer), &peer.node_did, expired,).is_none()
    );
}

#[test]
fn machine_pairing_never_claims_an_unlisted_node() {
    let mut machine = peer_record(Some("enrollment"));
    machine.pairing_template = Some("machine".to_string());
    machine.pairing_ready = true;
    assert!(peer_record_owning_node_at(&[machine], "did:key:child", Utc::now()).is_none());
}

#[tokio::test]
async fn submitted_request_survives_refresh_failure_and_requests_scoped_resync() {
    let (store, _version) = ObservedStore::new(ClientStore::default());
    let mut wake = store.subscribe_resync_requests();
    let receipt = SubmittedRequest {
        request_id: "accepted-request".into(),
        session_id: "accepted-session".into(),
        node_did: "did:test:node".into(),
        agent_id: Some("agent".into()),
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
        agent_id: None,
    };
    let accepted = observe_submitted_request(&store, receipt.clone(), async { Ok(1) }).await;
    assert_eq!(accepted, receipt);
    assert!(!wake.has_changed().unwrap());
    assert_eq!(store.take_resync_requests(), (false, Vec::new()));
}
