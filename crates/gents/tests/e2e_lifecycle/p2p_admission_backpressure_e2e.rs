use std::time::Duration;
use tokio::time::Instant;

use gents::defra_node::Node as EmbeddedNode;
use gents_store::graphql::escape_graphql_string;

use crate::support::test_p2p_db_with_admission;
use gents_p2p::test_support::transport::{wait_for_connected_peer, wait_for_listen_addr};
use gents_store::test_support::snapshot_reader::SnapshotReader;

const OWNER_DID: &str = "did:test:admission-p2p-owner";

async fn install_one_way_replicator(
    sender: &std::sync::Arc<EmbeddedNode>,
    receiver: &std::sync::Arc<EmbeddedNode>,
    collections: &[&str],
    subscribe: bool,
) {
    let sender_addr = wait_for_listen_addr(sender).await;
    let receiver_addr = wait_for_listen_addr(receiver).await;
    // Transport administration runs as each Node's own identity.
    let sender_p2p = gents_p2p::node_access::operations(sender).expect("sender p2p");
    let receiver_p2p = gents_p2p::node_access::operations(receiver).expect("receiver p2p");

    sender_p2p
        .connect_peer(&receiver_addr)
        .await
        .expect("connect sender to receiver");
    wait_for_connected_peer(sender).await;
    wait_for_connected_peer(receiver).await;

    let collection_names = collections
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    if subscribe {
        sender_p2p
            .add_collections(collection_names.clone())
            .await
            .expect("add sender p2p collections");
        receiver_p2p
            .add_collections(collection_names.clone())
            .await
            .expect("add receiver p2p collections");
    } else {
        assert!(!receiver_p2p
            .get_collections()
            .await
            .unwrap()
            .iter()
            .any(|name| collection_names.contains(name)));
        assert!(!sender_p2p
            .get_collections()
            .await
            .unwrap()
            .iter()
            .any(|name| collection_names.contains(name)));
    }
    receiver_p2p
        .add_replicator(
            collection_names.clone(),
            Some(&sender_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("authorize sender as receiver-side replicator");
    sender_p2p
        .add_replicator(
            collection_names,
            Some(&receiver_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install sender to receiver replicator");
}

/// Publishes one discovery record, the only document class pairing carries.
async fn publish_peer_record(node: &std::sync::Arc<EmbeddedNode>, peer_id: &str, address: &str) {
    let peer_id = escape_graphql_string(peer_id);
    let address = escape_graphql_string(address);
    let mutation = format!(
        r#"mutation {{
            create_PeerRegistry(input: {{
                peer_id: "{peer_id}",
                node_did: "{OWNER_DID}",
                addresses: ["{address}"],
                endpoint_address: "{address}",
                endpoint_updated_at: "2026-07-09T00:00:00Z",
                binding_sig: "sig-{address}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = gents_store::config_client::ConfigAccess::write_local_response(
        node,
        "test.p2p_fixture",
        &mutation,
    )
    .await
    .expect("P2P document access");
    assert!(
        !resp.has_errors(),
        "create PeerRegistry failed: {:?}",
        resp.errors
    );
}

async fn wait_for_peer_record(
    node: &std::sync::Arc<EmbeddedNode>,
    peer_id: &str,
    timeout: Duration,
    label: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    let query = format!(
        r#"{{ PeerRegistry(filter: {{ peer_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID peer_id node_did endpoint_address }} }}"#,
        escape_graphql_string(peer_id)
    );
    let mut changes = node.subscribe_changes("PeerRegistry").await;
    loop {
        let resp = gents_store::graphql::graphql_with_transaction_retry(
            node,
            &query,
            "test.p2p_observation",
        )
        .await
        .expect("P2P document access");
        if let Some(row) = resp
            .data
            .as_ref()
            .and_then(|data| data["PeerRegistry"].as_array())
            .and_then(|rows| rows.first())
        {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for PeerRegistry({peer_id}) on {label}: {:?}",
            resp.errors
        );
        tokio::time::timeout_at(deadline, changes.recv())
            .await
            .unwrap_or_else(|_| panic!("PeerRegistry({peer_id}) did not arrive on {label}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_push_worker_delivers_multi_wave_updates() {
    let admission = TestP2pAdmission::single_push_worker();
    let owner = test_p2p_db_with_admission("admission-p2p-multi-owner", admission.clone()).await;
    let peer = test_p2p_db_with_admission("admission-p2p-multi-peer", admission).await;

    install_one_way_replicator(&owner.node, &peer.node, &["PeerRegistry"], true).await;

    for idx in 0..3 {
        let peer_id = format!("admission-p2p-multi-wave-{idx}");
        let address = format!("/ip4/127.0.0.1/tcp/{}", 4100 + idx);
        publish_peer_record(&owner.node, &peer_id, &address).await;
        let on_peer = wait_for_peer_record(
            &peer.node,
            &peer_id,
            Duration::from_secs(45),
            &format!("peer wave {idx}"),
        )
        .await;
        assert_eq!(on_peer["node_did"], OWNER_DID);
        assert_eq!(on_peer["endpoint_address"], address.as_str());
    }
}

use gents_p2p::test_support::transport::{LoopbackEndpoint, TestP2pAdmission};

#[tokio::test]
async fn incoming_pushes_ack_only_a_registered_dag() {
    use gents_lean_vocab::tla::replay::{Red, Trace};
    let trace = Trace::load(
        include_str!("../../proofs/tla/generated/traces/MCP2PBackpressureBadAck.json"),
        "P2PBackpressure",
        &[],
    );
    let resident = replay_missing_pushes(&trace, false).await;
    let persisted = replay_missing_pushes(&trace, true).await;
    trace.check(Red::Defect, &[("SuccessAckBacked", resident && persisted)]);
}

async fn replay_missing_pushes(
    trace: &gents_lean_vocab::tla::replay::Trace,
    durable: bool,
) -> bool {
    use blockstore::DefraBlockstore;
    use defra_core::{Block, CompositeDeltaPayload, CrdtDelta, DAGLink};
    use gents_store::test_support::STALL_CEILING;
    use p2p::message::PushLogRequest;
    use p2p::sync::pending_store::{PendingDagStorage, PendingDagStore};
    use p2p::sync::{SyncConfig, SyncCoordinator};
    use p2p::transport::{P2PTransport, TransportEvent};
    use std::sync::Arc;
    let config = SyncConfig {
        max_pending_dags: 1,
        ..Default::default()
    };
    assert_eq!(trace.constants["MaxPending"], config.max_pending_dags);
    let engine = Arc::new(storage::RegolithStore::in_memory().unwrap());
    let blockstore = Arc::new(DefraBlockstore::new(engine.clone(), true));
    let mut receiver = LoopbackEndpoint::start().await;
    let (owner, _events) = SyncCoordinator::new(
        receiver.transport.clone(),
        blockstore,
        receiver.network.clone(),
        config,
    )
    .await
    .unwrap();
    let disk = Arc::new(PendingDagStore::new(engine));
    if durable {
        owner.install_pending_dag_store(disk.clone()).await;
    }
    let mut backed = true;
    for step in &trace.steps {
        let peer = step.actor.as_ref().unwrap()["$model"].as_str().unwrap();
        let sender = LoopbackEndpoint::start().await;
        let receiver_id = receiver.transport.local_peer_id();
        sender
            .transport
            .dial(
                receiver_id,
                receiver.transport.listen_addresses().await.unwrap(),
            )
            .await
            .unwrap();
        let composite = |links| {
            Block::new(
                CrdtDelta::Composite(CompositeDeltaPayload {
                    collection_version_id: format!("collection-{peer}"),
                    priority: 1,
                    status: 1,
                }),
                vec![],
                links,
            )
        };
        let missing = composite(vec![]).generate_cid().unwrap();
        let block = composite(vec![DAGLink::new("missing", missing)]);
        let root = block.generate_cid().unwrap();
        let request = PushLogRequest::new(
            format!("doc-{peer}"),
            root.to_bytes().into(),
            "collection".into(),
            peer.into(),
            block.to_dag_cbor().unwrap().into(),
        );
        let doc_id = request.doc_id.clone();
        let receiving = async {
            loop {
                let event = receiver.events.recv().await.expect("receiver stopped");
                if let TransportEvent::TwoStreamRequest { request, .. } = &event {
                    assert_eq!(request.doc_id, doc_id);
                    let message_id = request.message_id.clone();
                    break (message_id, owner.handle_transport_event(event).await);
                }
            }
        };
        let (reply, (message_id, handled)) = tokio::time::timeout(STALL_CEILING, async {
            tokio::join!(
                sender
                    .transport
                    .send_two_stream_request(receiver_id, request),
                receiving
            )
        })
        .await
        .expect("push did not settle");
        let reply = reply.unwrap();
        assert_eq!(reply.message_id, message_id);
        let resident = owner.manager().pending_dag_snapshot(&root).is_some();
        let persisted = disk
            .load_all()
            .await
            .unwrap()
            .iter()
            .any(|(cid, _)| cid == &root);
        let acknowledged = reply.err_message.is_none();
        assert_eq!(acknowledged, handled.is_ok());
        match step.action.as_str() {
            "ReceiveMissingAndRegister" => assert!(acknowledged && resident),
            "BadAckAtCapacity" if !acknowledged => {
                assert!(matches!(
                    handled,
                    Err(p2p::error::Error::PendingDagCapacity { .. })
                ));
                assert!(reply.retry_after().is_some());
                assert!(!resident && !persisted);
            }
            "BadAckAtCapacity" => assert!(durable && persisted),
            action => panic!("unmapped inbound push {action}"),
        }
        backed &= !acknowledged || if durable { persisted } else { resident };
        sender.shutdown().await;
    }
    receiver.shutdown().await;
    backed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_push_yields_the_only_worker_to_healthy_peers() {
    use gents_lean_vocab::tla::replay::Trace;
    for json in [
        include_str!("../../proofs/tla/generated/traces/MCP2PBackpressureTimeoutStall.json"),
        include_str!("../../proofs/tla/generated/traces/MCP2PBackpressureGreen.sample-1.json"),
    ] {
        let admission = TestP2pAdmission::single_push_worker();
        let trace = Trace::load(
            json,
            "P2PBackpressure",
            &[(
                "PushWorkers",
                admission.max_concurrent_push_tasks.try_into().unwrap(),
            )],
        );
        replay_pushes(&trace, admission).await;
    }
}

async fn replay_pushes(trace: &gents_lean_vocab::tla::replay::Trace, admission: TestP2pAdmission) {
    use gents_lean_vocab::tla::replay::Red;
    use gents_store::test_support::STALL_CEILING;
    use p2p::iroh::best_shareable_public_addr;
    use p2p::transport::{P2PTransport, TransportEvent};
    let owner = test_p2p_db_with_admission("trace-push-owner", admission.clone()).await;
    let admin = gents_p2p::node_access::operations(&owner.node).unwrap();
    let mut slow = LoopbackEndpoint::start().await;
    let address = best_shareable_public_addr(
        slow.transport.local_peer_id(),
        &slow.transport.listen_addresses().await.unwrap(),
    )
    .unwrap();
    admin.connect_peer(&address).await.unwrap();
    admin
        .add_replicator(
            vec!["PeerRegistry".into()],
            Some(&address),
            [(
                "PeerRegistry".into(),
                defra_p2p_adapter::ReplicationFilter::eq("peer_id", "slow-held".into()),
            )]
            .into(),
            vec![],
            None,
        )
        .await
        .unwrap();
    let mut healthy = rapidhash::RapidHashMap::default();
    let mut held = None;
    let mut delivered = 0usize;
    let mut merged = 0usize;
    for step in &trace.steps {
        let actor = || step.actor.as_ref().unwrap()["$model"].as_str().unwrap();
        match step.action.as_str() {
            "ReceiveComplete" => {
                let peer = actor();
                let receiver = test_p2p_db_with_admission(peer, admission.clone()).await;
                install_one_way_replicator(&owner.node, &receiver.node, &["PeerRegistry"], false)
                    .await;
                let inbound = format!("received-{peer}");
                publish_peer_record(&receiver.node, &inbound, peer).await;
                let row =
                    wait_for_peer_record(&owner.node, &inbound, STALL_CEILING, "inbound merge")
                        .await;
                merged += usize::from(row["endpoint_address"] == peer);
                assert!(healthy.insert(peer, receiver).is_none());
            }
            "StartPush" if actor() == "slow" => {
                publish_peer_record(&owner.node, "slow-held", "slow").await;
                let row =
                    wait_for_peer_record(&owner.node, "slow-held", STALL_CEILING, "held document")
                        .await;
                held = Some(
                    tokio::time::timeout(STALL_CEILING, async {
                        loop {
                            if let TransportEvent::TwoStreamRequest {
                                request,
                                token: Some(token),
                                ..
                            } = slow.events.recv().await.expect("slow endpoint closed")
                            {
                                assert_eq!(row["_docID"], request.doc_id);
                                break token;
                            }
                        }
                    })
                    .await
                    .expect("slow peer did not receive the push"),
                );
            }
            "StartPush" => {
                assert!(healthy.contains_key(actor()));
                publish_peer_record(&owner.node, &format!("outbound-{}", actor()), actor()).await;
            }
            "PushSucceeds" => {
                let peer = actor();
                let row = wait_for_peer_record(
                    &healthy[peer].node,
                    &format!("outbound-{peer}"),
                    STALL_CEILING,
                    "single worker delivery",
                )
                .await;
                delivered += usize::from(row["endpoint_address"] == peer);
            }
            "PushTimesOut" | "Stutter" => {
                assert!(held.is_some());
                publish_peer_record(&owner.node, "after-held-push", "healthy").await;
                for peer in healthy.values() {
                    let row = wait_for_peer_record(
                        &peer.node,
                        "after-held-push",
                        STALL_CEILING,
                        "delivery while the slow peer holds its response",
                    )
                    .await;
                    assert_eq!(row["endpoint_address"], "healthy");
                    if step.action == "Stutter" {
                        delivered += 1;
                    }
                }
            }
            action => panic!("unmapped outbound push {action}"),
        }
    }
    let responsive = trace.constants["ResponsivePeer"].as_array().unwrap().len();
    let inbound = trace.constants["InboundPeer"].as_array().unwrap().len();
    assert_eq!(healthy.len(), responsive);
    trace.check(
        Red::Defect,
        &[
            ("HealthyPeersDeliver", delivered == responsive),
            ("SuccessAckBacked", merged == inbound),
            ("InboundSettles", merged == inbound),
        ],
    );
    drop(held);
    slow.shutdown().await;
}
