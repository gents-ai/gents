//! Owns its own `ReplicatedEvent` source schema: the single-node
//! `event_trigger_e2e.rs` uses `WebhookEvent` in the same process-global
//! DefraDB schema registry.
//!
//! An application collection reaches a peer only through an application
//! pairing (Lean `PairingReconcile.ApplicationRoute.no_overlay_is_base_only`);
//! the single-node trigger path is covered by `event_source_trigger_e2e`.

use std::time::Duration;

use gents::defra_node::Node as EmbeddedNode;
use gents_p2p::p2p_reconcile::{EmbeddedRemoteP2pAdmin, RemoteP2pAdmin};
use gents_store::graphql::escape_graphql_string;
use serde_json::Value;

use crate::support::snapshots::SnapshotReader;
use crate::support::test_p2p_db;
use gents_p2p::test_support::transport::{wait_for_connected_peer, wait_for_listen_addr};

const EXTERNAL_ID: &str = "wh-p2p-1";

async fn register_replicated_event_schema(node: &std::sync::Arc<EmbeddedNode>) {
    let sdl = r#"
        type ReplicatedEvent @branchable {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    gents_store::config_client::ConfigAccess::local(node.clone())
        .add_collection(sdl)
        .await
        .expect("add_collection for ReplicatedEvent");
}

async fn write_replicated_event(
    node: &std::sync::Arc<EmbeddedNode>,
    external_id: &str,
    kind: &str,
) -> String {
    let mutation = format!(
        r#"mutation {{
            add_ReplicatedEvent(input: {{
                external_id: "{}",
                payload: "{{}}",
                kind: "{}"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(external_id),
        escape_graphql_string(kind),
    );
    let response = gents_store::config_client::ConfigAccess::write_local_response(
        node,
        "test.p2p_fixture",
        &mutation,
    )
    .await
    .expect("P2P document access");
    assert!(
        !response.has_errors(),
        "add_ReplicatedEvent failed: {:?}",
        response.errors
    );
    let created = &response.data.as_ref().expect("mutation data")["add_ReplicatedEvent"];
    created
        .get("_docID")
        .or_else(|| created.get(0).and_then(|row| row.get("_docID")))
        .and_then(Value::as_str)
        .expect("ReplicatedEvent document ID")
        .to_owned()
}

async fn replicated_events(node: &std::sync::Arc<EmbeddedNode>) -> Vec<Value> {
    let response = gents_store::graphql::graphql_with_transaction_retry(
        node,
        r#"{ ReplicatedEvent { _docID external_id } }"#,
        "test.p2p_observation",
    )
    .await
    .expect("P2P document access");
    assert!(
        !response.has_errors(),
        "query ReplicatedEvent: {:?}",
        response.errors
    );
    response.data.as_ref().expect("query data")["ReplicatedEvent"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unpaired_application_event_source_stays_local() {
    let _p2p_guard = super::P2P_E2E_LOCK.lock().await;
    let db_writer = test_p2p_db("event-trigger-p2p-writer").await;
    register_replicated_event_schema(&db_writer.node).await;
    let db_agent = test_p2p_db("event-trigger-p2p-agent").await;
    register_replicated_event_schema(&db_agent.node).await;

    let writer = EmbeddedRemoteP2pAdmin::new(db_writer.node.clone());
    let agent_addr = wait_for_listen_addr(&db_agent.node).await;
    writer
        .connect(std::slice::from_ref(&agent_addr))
        .await
        .expect("connect writer to agent");
    wait_for_connected_peer(&db_writer.node).await;

    for node in [&db_writer.node, &db_agent.node] {
        assert!(
            !gents_store::identity::node_p2p(node)
                .expect("P2P transport")
                .get_collections()
                .await
                .expect("list P2P subscriptions")
                .iter()
                .any(|collection| collection == "ReplicatedEvent"),
            "an application collection must not be subscribed"
        );
    }
    let source_doc_id = write_replicated_event(&db_writer.node, EXTERNAL_ID, "signup").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let mut changes = db_agent.node.subscribe_changes("ReplicatedEvent").await;
    loop {
        assert!(
            replicated_events(&db_agent.node).await.is_empty(),
            "ReplicatedEvent {source_doc_id} reached the peer"
        );
        if tokio::time::timeout_at(deadline, changes.recv())
            .await
            .is_err()
        {
            break;
        }
    }
    assert!(
        replicated_events(&db_agent.node).await.is_empty(),
        "ReplicatedEvent reached the peer at the observation deadline"
    );
    assert_eq!(replicated_events(&db_writer.node).await.len(), 1);

    db_writer.node.shutdown().await.expect("node shutdown");
    db_agent.node.shutdown().await.expect("node shutdown");
}
