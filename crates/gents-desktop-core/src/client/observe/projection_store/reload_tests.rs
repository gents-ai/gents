use super::*;
use crate::client::store::ClientStoreRows;
use gents_protocol::row::AgentRequestRow;

const NODE: &str = "did:test:reload";

fn requests(ids: &[&str]) -> ClientStore {
    ClientStore::from_rows(ClientStoreRows {
        requests: ids
            .iter()
            .map(|id| AgentRequestRow {
                request_id: (*id).into(),
                node_did: Some(NODE.into()),
                session_id: Some("session".into()),
                purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    })
}

fn node_store(initial: ClientStore) -> (Arc<ObservedStores>, Arc<ObservedStore>) {
    let stores = ObservedStores::new();
    let store = stores.insert_for_test(NODE, initial);
    (stores, store)
}

#[test]
fn generated_notice_delivery_never_regresses_watchers() {
    let case = &gents_lean_vocab::lean_contract_snapshot()
        .client_data_publication_cases
        .notice_delivery;
    let decode = |version: u64, provenance: u64, request_event: u64| StoreUpdateNotice {
        revision: StoreProjectionRevision {
            store_version: version,
            provenance_version: provenance,
            request_event_version: request_event,
        },
    };
    let (_stores, store) = node_store(ClientStore::default());
    let versions = store.subscribe();
    let changes = store.change_tx.subscribe();
    store.publish_notice(decode(
        case.initial.version,
        case.initial.provenance,
        case.initial.request_event,
    ));
    for delivery in &case.deliveries {
        store.publish_notice(decode(
            delivery.incoming.version,
            delivery.incoming.provenance,
            delivery.incoming.request_event,
        ));
        let expected = decode(
            delivery.expected.version,
            delivery.expected.provenance,
            delivery.expected.request_event,
        );
        assert_eq!(*versions.borrow(), expected.revision.store_version);
        assert_eq!(*changes.borrow(), expected);
    }
}

#[test]
fn provenance_revision_ignores_transcript_and_lease_but_tracks_remote_completion() {
    use gents_protocol::request_lifecycle::RequestLifecycleState;

    let mut initial = requests(&["parent", "remote-child"]);
    initial.requests[0].doc_id = Some("parent-doc".into());
    initial.requests[1].caused_by_parent_request_doc_id = Some("parent-doc".into());
    initial.requests[1].lifecycle_state = Some(RequestLifecycleState::Processing);
    let (_stores, store) = node_store(initial);
    let original = store.projection_revision();
    for _ in 0..50 {
        store.invalidate_projection(false);
    }
    assert_eq!(
        store.projection_revision().provenance_version,
        original.provenance_version
    );
    assert_eq!(
        store.projection_revision().store_version,
        original.store_version + 50
    );

    let mut renewed = store.snapshot().as_ref().clone();
    renewed.requests[1].execution_lease_expires_at = Some("later".into());
    store.merge_chat_patch(renewed);
    assert_eq!(
        store.projection_revision().provenance_version,
        original.provenance_version
    );

    let mut completed = store.snapshot().as_ref().clone();
    completed.requests[1].lifecycle_state = Some(RequestLifecycleState::Completed);
    store.merge_chat_patch(completed);
    assert_eq!(
        store.projection_revision().provenance_version,
        original.provenance_version + 1
    );
    let settled = store.projection_revision();
    store.merge_chat_patch(store.snapshot().as_ref().clone());
    assert_eq!(
        store.projection_revision().provenance_version,
        settled.provenance_version
    );
}

#[test]
fn accepted_reload_refreshes_unretained_lineage_but_rejected_reload_does_not() {
    let (_stores, store) = node_store(requests(&["parent"]));
    let captured = store.capture_read();
    store.merge_chat_patch(requests(&["parent"]));
    let current = store.projection_revision();
    assert!(!store.replace_reloaded_snapshot_at_read(&captured, requests(&["parent"])));
    assert_eq!(store.projection_revision(), current);
    let current_snapshot = store.capture_read();
    assert!(store.replace_reloaded_snapshot_at_read(&current_snapshot, requests(&["parent"])));
    assert_eq!(
        store.projection_revision().provenance_version,
        current.provenance_version + 1
    );
}

#[test]
fn generated_data_publication_cases_match_observed_store() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../gents/tests/fixtures/client_data_publication.json"
    ))
    .expect("generated ClientDataPublication cases");
    let cases = fixture["publication"]
        .as_array()
        .expect("publication cases");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let expected = case["accepted"].as_bool().expect("model decision");
        let ids = |key: &str| -> Vec<String> {
            case[key]
                .as_array()
                .expect("modeled request IDs")
                .iter()
                .map(|id| id.as_str().expect("request ID").to_owned())
                .collect()
        };
        let initial_ids = ids("initial_request_ids");
        let publish_ids = ids("publish_request_ids");
        let initial_refs = initial_ids.iter().map(String::as_str).collect::<Vec<_>>();
        let publish_refs = publish_ids.iter().map(String::as_str).collect::<Vec<_>>();
        let (_stores, store) = node_store(requests(&initial_refs));
        let captured = store.capture_read();
        for action in case["actions"].as_array().expect("modeled actions") {
            match action.as_str().expect("publication action") {
                "request_event" => {
                    store.observe_request_event();
                }
                "transcript_invalidation" => {
                    store.invalidate_projection(false);
                }
                "replace_requests" => {
                    store.replace_snapshot(requests(&[]));
                }
                other => panic!("unrecognized modeled publication action {other}"),
            }
        }
        let published = store
            .merge_chat_patch_at_read(&captured, requests(&publish_refs))
            .is_some();
        assert_eq!(published, expected, "modeled case {name}");
    }
}

/// Two nodes, each in its own store. A read of node 1 publishes after the
/// modeled actions; nothing done to node 2's store moves node 1's.
#[test]
fn generated_node_publications_keep_each_node_in_its_own_store() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../gents/tests/fixtures/client_data_publication.json"
    ))
    .expect("generated ClientDataPublication cases");
    let did = |node: u64| format!("did:node:{node}");
    let sessions = |node: u64, title: &str| {
        ClientStore::from_rows(ClientStoreRows {
            sessions: vec![serde_json::from_value(serde_json::json!({
                "node_did": did(node),
                "session_id": format!("session-{node}"),
                "agent_did": "agent",
                "created_at": "2026-01-01T00:00:00Z",
                "title": {"text": title, "source": "user"}
            }))
            .expect("modeled session")],
            ..Default::default()
        })
    };
    let cases = fixture["node_publication"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let stores = ObservedStores::new();
        for node in [1, 2] {
            stores.insert_for_test(&did(node), sessions(node, "initial"));
        }
        let read_store = stores.store(&did(1)).unwrap();
        let read = read_store.capture_read();
        for action in case["actions"].as_array().unwrap() {
            let node = action["node"].as_u64().unwrap();
            let store = stores.store(&did(node)).unwrap();
            match action["op"].as_str().unwrap() {
                "merge" => {
                    store.merge_chat_patch(sessions(node, "concurrent"));
                }
                "request" => {
                    store.observe_request_event();
                }
                "transcript" => {
                    store.invalidate_projection(false);
                }
                other => panic!("unknown modeled publication action {other}"),
            }
        }
        let incoming = sessions(1, "incoming");
        let accepted = if case["merge"].as_bool().unwrap() {
            let read_accepted = read_store.accepts_chat_patch_read(&read);
            let published = read_store
                .merge_chat_patch_at_read(&read, incoming)
                .is_some();
            assert_eq!(read_accepted, published, "{name}");
            published
        } else {
            read_store.replace_reloaded_snapshot_at_read(&read, incoming)
        };
        assert_eq!(accepted, case["accepted"].as_bool().unwrap(), "{name}");
        for expected in case["expected"].as_array().unwrap() {
            let node = expected["node"].as_u64().unwrap();
            let shown = stores.snapshot(&did(node));
            let titles = shown
                .sessions
                .iter()
                .map(|row| {
                    (
                        row.node_did.clone(),
                        row.title.as_ref().unwrap().text.clone(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                titles,
                vec![(did(node), expected["value"].as_str().unwrap().to_owned())],
                "{name}: node {node} shows only its own rows"
            );
        }
    }
}

#[tokio::test]
async fn generated_recovery_cases_preserve_scopes_without_self_waking() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../gents/tests/fixtures/client_data_publication.json"
    ))
    .expect("generated ClientDataPublication cases");
    let cases = fixture["recovery"].as_array().expect("recovery cases");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let scope =
            |value: &serde_json::Value| format!("scope-{}", value.as_u64().expect("modeled scope"));
        let expected_scopes: Vec<String> = case["pending_scopes"]
            .as_array()
            .expect("modeled pending scopes")
            .iter()
            .map(scope)
            .collect();
        let expected_generation = case["generation"].as_u64().expect("model generation");
        let retry_scheduled = case["retry_scheduled"]
            .as_bool()
            .expect("modeled retry deadline state");
        assert!(case["initial"]["pending_scopes"]
            .as_array()
            .expect("modeled initial scopes")
            .is_empty());
        assert_eq!(case["initial"]["generation"].as_u64(), Some(0));
        assert_eq!(case["initial"]["retry_scheduled"].as_bool(), Some(false));
        let stores = ObservedStores::new();
        let mut wake = stores.subscribe_resync_requests();
        assert_eq!(
            *wake.borrow(),
            case["initial"]["generation"].as_u64().unwrap()
        );
        let mut in_flight = Vec::new();
        let mut observed_generation = 0;
        let mut batch_taken = false;
        let mut retry_not_before = None;
        let now = tokio::time::Instant::now();
        for action in case["actions"]
            .as_array()
            .expect("modeled recovery actions")
        {
            match action["op"].as_str().expect("recovery action op") {
                "request_scope" if batch_taken => {
                    stores.ensure(&scope(&action["scope"])).request_resync();
                    assert!(wake.has_changed().expect("watch remains open"));
                    observed_generation = *wake.borrow_and_update();
                }
                "request_scope" => {
                    stores.ensure(&scope(&action["scope"])).request_resync();
                }
                "take_batch" => {
                    wake.changed().await.expect("initial batch notification");
                    in_flight = stores.take_resync_requests();
                    observed_generation = *wake.borrow_and_update();
                    batch_taken = true;
                }
                "fail_batch" => {
                    for failed in action["scopes"].as_array().expect("modeled failed scopes") {
                        let failed = scope(failed);
                        assert!(
                            in_flight.contains(&failed),
                            "failed scope was not in flight"
                        );
                        stores.ensure(&failed).enqueue_resync_request();
                    }
                    assert!(!wake.has_changed().expect("watch remains open"));
                    assert_eq!(*wake.borrow(), observed_generation);
                    retry_not_before = Some(now + std::time::Duration::from_millis(1));
                }
                "retry_timer" => {
                    retry_not_before = None;
                }
                other => panic!("unrecognized modeled recovery action {other}"),
            }
        }
        assert_eq!(
            *wake.borrow(),
            expected_generation,
            "model generation {name}"
        );
        assert_eq!(
            super::super::resync_retry_ready(retry_not_before, now),
            !retry_scheduled,
            "observer retry gate for modeled recovery actions {name}"
        );
        if retry_scheduled {
            let deadline = retry_not_before.expect("failed batch has a retry deadline");
            assert!(super::super::resync_retry_ready(Some(deadline), deadline));
        }
        assert_eq!(
            stores.take_resync_requests(),
            expected_scopes,
            "modeled recovery case {name}"
        );
    }
}

#[test]
fn denied_resource_read_invalidates_stale_snapshot_and_allows_fresh_visible_rows() {
    let (stores, store) = node_store(requests(&["before"]));
    let read = store.capture_read();
    stores.observe_runtime_connection(NODE, RuntimeConnectionEvent::SnapshotSucceeded);
    store.clear_after_refused_read();
    assert_eq!(
        stores.runtime_connection(NODE),
        Some(RuntimeConnectionStatus::Connected)
    );
    assert!(store.snapshot().requests.is_empty());
    assert!(!store.replace_reloaded_snapshot_at_read(&read, requests(&["stale"])));
    assert!(store.snapshot().requests.is_empty());
    assert_eq!(stores.take_resync_requests(), vec![NODE.to_owned()]);
    let fresh = store.capture_read();
    assert!(store.replace_reloaded_snapshot_at_read(&fresh, requests(&["unrelated-visible"])));
    assert_eq!(store.snapshot().requests[0].request_id, "unrelated-visible");
    assert_eq!(
        stores.runtime_connection(NODE),
        Some(RuntimeConnectionStatus::Connected)
    );
}
