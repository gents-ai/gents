use super::*;

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

#[test]
fn chat_write_fence_matches_lean() {
    use crate::client::peer_directory::tests::{chat_case_record, with_route_observations};
    for case in gents_lean_vocab::families::chat_readiness_cases_write_fence::cases::<
        crate::client::peer_directory::ChatRouteCause,
    >() {
        let records = match case.input.route.as_ref() {
            None => vec![None],
            Some(route) => with_route_observations(&chat_case_record(
                route.source.as_deref(),
                route.operator_endpoint,
                route.managed,
            ))
            .into_iter()
            .map(Some)
            .collect(),
        };
        let mut refusals = Vec::new();
        for record in records {
            let node_did = record
                .as_ref()
                .map_or("did:key:runtime", |record| record.node_did.as_str())
                .to_owned();
            let refusal =
                chat_route_refusal(&node_did, record.as_ref()).map(|refusal| refusal.cause());
            match ensure_peer_chat_ready(&node_did, record.as_ref()) {
                Ok(()) => assert_eq!(refusal, None, "{record:?}"),
                Err(error) => {
                    let cause = error
                        .downcast_ref::<ChatRouteRefusal>()
                        .map(ChatRouteRefusal::cause)
                        .expect("a typed refusal");
                    assert_eq!(Some(cause), refusal, "{record:?}");
                    assert!(
                        error.to_string().contains(cause.token())
                            && error
                                .to_string()
                                .contains(cause.next_action().description()),
                        "the refusal must name its cause and next action: {error}"
                    );
                }
            }
            let owning = peer_record_owning_node(record.as_slice(), &node_did);
            assert_eq!(owning.is_some(), refusal.is_none(), "{record:?}");
            refusals.push(refusal);
        }
        assert!(
            refusals.windows(2).all(|pair| pair[0] == pair[1]),
            "{refusals:?}"
        );
        case.check(
            gents_lean_vocab::families::chat_readiness_cases_write_fence::FenceOutput {
                refusal: refusals[0],
            },
        );
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
async fn submitted_request_survives_refresh_failure_without_duplicating_observer_resync() {
    let stores = crate::client::observe::ObservedStores::new();
    let store = stores.ensure("did:test:node");
    let wake = stores.subscribe_resync_requests();
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
    assert!(!wake.has_changed().unwrap());
    assert!(stores.take_resync_requests().is_empty());
    assert_eq!(store.projection_revision(), before);
    assert!(store.snapshot().requests.is_empty());
}

#[tokio::test]
async fn submitted_request_refresh_success_does_not_schedule_another_read() {
    let stores = crate::client::observe::ObservedStores::new();
    let store = stores.ensure("did:test:node");
    let wake = stores.subscribe_resync_requests();
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
    assert!(stores.take_resync_requests().is_empty());
}
