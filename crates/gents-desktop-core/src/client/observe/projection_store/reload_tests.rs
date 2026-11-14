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
fn notice_delivery_matches_the_lean_model() {
    use gents_lean_vocab::families::client_data_publication_notice_delivery as notice_family;
    let decode = |notice: &notice_family::Notice| StoreUpdateNotice {
        revision: StoreProjectionRevision {
            store_version: notice.version,
            provenance_version: notice.provenance,
            request_event_version: notice.request_event,
        },
    };
    gents_lean_vocab::families::client_data_publication_notice_delivery::replay(|trace| {
        let (_stores, store) = node_store(ClientStore::default());
        let versions = store.subscribe();
        let changes = store.change_tx.subscribe();
        store.publish_notice(decode(&trace.initial));
        trace
            .deliveries
            .iter()
            .map(|incoming| {
                store.publish_notice(decode(incoming));
                let change = changes.borrow().revision;
                notice_family::Delivered {
                    change: notice_family::Notice {
                        version: change.store_version,
                        provenance: change.provenance_version,
                        request_event: change.request_event_version,
                    },
                    watched_version: *versions.borrow(),
                }
            })
            .collect()
    });
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
fn data_publications_match_the_lean_model() {
    use gents_lean_vocab::families::client_data_publication_publication as publication_family;
    gents_lean_vocab::families::client_data_publication_publication::replay::<String>(|case| {
        let initial = case
            .initial_request_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let publish = case
            .publish_request_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let (_stores, store) = node_store(requests(&initial));
        let captured = store.capture_read();
        for action in &case.actions {
            match action.as_str() {
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
        publication_family::PublicationOutput {
            accepted: store
                .merge_chat_patch_at_read(&captured, requests(&publish))
                .is_some(),
        }
    });
}

/// Two nodes, each in its own store. A read of node 1 publishes after the
/// modeled actions; nothing done to node 2's store moves node 1's.
#[test]
fn node_publications_match_the_lean_model() {
    use gents_lean_vocab::families::client_data_publication_node_publication as node_family;
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
    gents_lean_vocab::families::client_data_publication_node_publication::replay::<String>(
        |case| {
            let stores = ObservedStores::new();
            for node in [1, 2] {
                stores.insert_for_test(&did(node), sessions(node, "initial"));
            }
            let read_store = stores.store(&did(1)).unwrap();
            let read = read_store.capture_read();
            for action in &case.actions {
                let store = stores.store(&did(action.node)).unwrap();
                match action.op.as_str() {
                    "merge" => {
                        store.merge_chat_patch(sessions(action.node, "concurrent"));
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
            let accepted = if case.merge {
                let read_accepted = read_store.accepts_chat_patch_read(&read);
                let published = read_store
                    .merge_chat_patch_at_read(&read, incoming)
                    .is_some();
                assert_eq!(read_accepted, published, "{}", case.name);
                published
            } else {
                read_store.replace_reloaded_snapshot_at_read(&read, incoming)
            };
            let shown = [1, 2]
                .into_iter()
                .flat_map(|node| {
                    stores
                        .snapshot(&did(node))
                        .sessions
                        .iter()
                        .map(|row| {
                            assert_eq!(
                                row.node_did,
                                did(node),
                                "{}: node {node} shows only its own rows",
                                case.name
                            );
                            node_family::NodeRow {
                                node,
                                value: row.title.as_ref().unwrap().text.clone(),
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            node_family::NodePublicationOutput {
                accepted,
                expected: shown,
            }
        },
    );
}

#[tokio::test]
async fn recovery_matches_the_lean_model() {
    use gents_lean_vocab::families::client_data_publication_recovery as recovery_family;
    let scope = |value: u64| format!("scope-{value}");
    let scopes = |taken: Vec<String>| -> Vec<u64> {
        taken
            .iter()
            .map(|taken| {
                taken
                    .strip_prefix("scope-")
                    .and_then(|value| value.parse().ok())
                    .expect("modeled scope")
            })
            .collect()
    };
    for case in gents_lean_vocab::families::client_data_publication_recovery::cases::<String>() {
        let name = &case.input.name;
        let stores = ObservedStores::new();
        let mut wake = stores.subscribe_resync_requests();
        let now = tokio::time::Instant::now();
        let mut retry_not_before = None;
        let initial = recovery_family::Recovery {
            pending_scopes: scopes(stores.take_resync_requests()),
            generation: *wake.borrow(),
            retry_scheduled: !super::super::resync_retry_ready(retry_not_before, now),
        };
        let mut in_flight = Vec::new();
        let mut observed_generation = 0;
        let mut batch_taken = false;
        for action in &case.input.actions {
            match (action.op.as_str(), action.scope) {
                ("request_scope", Some(requested)) if batch_taken => {
                    stores.ensure(&scope(requested)).request_resync();
                    assert!(wake.has_changed().expect("watch remains open"));
                    observed_generation = *wake.borrow_and_update();
                }
                ("request_scope", Some(requested)) => {
                    stores.ensure(&scope(requested)).request_resync();
                }
                ("take_batch", _) => {
                    wake.changed().await.expect("initial batch notification");
                    in_flight = stores.take_resync_requests();
                    observed_generation = *wake.borrow_and_update();
                    batch_taken = true;
                }
                ("fail_batch", _) => {
                    for failed in &action.scopes {
                        let failed = scope(*failed);
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
                ("retry_timer", _) => {
                    retry_not_before = None;
                }
                other => panic!("unrecognized modeled recovery action {other:?} in {name}"),
            }
        }
        if let Some(deadline) = retry_not_before {
            assert!(super::super::resync_retry_ready(Some(deadline), deadline));
        }
        let retry_scheduled = !super::super::resync_retry_ready(retry_not_before, now);
        let generation = *wake.borrow();
        case.check(recovery_family::RecoveryOutput {
            initial,
            pending_scopes: scopes(stores.take_resync_requests()),
            generation,
            retry_scheduled,
        });
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
