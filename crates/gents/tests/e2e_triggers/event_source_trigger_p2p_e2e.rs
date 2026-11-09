//! Owns its own `ReplicatedEvent` source schema: the single-node
//! `event_trigger_e2e.rs` uses `WebhookEvent` in the same process-global
//! DefraDB schema registry.
//!
//! P2P carries only discovery and enrollment documents, so an application
//! event source never reaches a peer's trigger; the single-node trigger path
//! is covered by `event_source_trigger_e2e`.

use std::time::Duration;

use gents::defra_node::Node as EmbeddedNode;
use gents::runtime::agent::p2p_reconcile::{
    EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin, RemoteP2pAdminError,
};
use gents::store::graphql::escape_graphql_string;
use serde_json::Value;

use crate::support::p2p_waits::{wait_for_connected_peer, wait_for_listen_addr};
use crate::support::test_p2p_db;

const EXTERNAL_ID: &str = "wh-p2p-1";

async fn register_replicated_event_schema(node: &EmbeddedNode) {
    let sdl = r#"
        type ReplicatedEvent @branchable {
            external_id: String
            payload: String
            kind: String @index
        }
    "#;
    node.add_collection(sdl)
        .await
        .expect("add_collection for ReplicatedEvent");
}

async fn write_replicated_event(node: &EmbeddedNode, external_id: &str, kind: &str) -> String {
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
    let response = node.execute(&mutation).await;
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

async fn replicated_events(node: &EmbeddedNode) -> Vec<Value> {
    let response = node
        .execute(r#"{ ReplicatedEvent { _docID external_id } }"#)
        .await;
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

fn refuses_runtime_local(result: Result<(), RemoteP2pAdminError>) -> bool {
    matches!(
        result,
        Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == "ReplicatedEvent"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p2p_refuses_to_replicate_an_application_event_source() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    let db_writer = test_p2p_db("event-trigger-p2p-writer").await;
    register_replicated_event_schema(db_writer.node.as_ref()).await;
    let db_agent = test_p2p_db("event-trigger-p2p-agent").await;
    register_replicated_event_schema(db_agent.node.as_ref()).await;

    let writer = EmbeddedRemoteP2pAdmin::new(db_writer.node.clone());
    let agent_addr = wait_for_listen_addr(db_agent.node.as_ref()).await;
    writer
        .connect(std::slice::from_ref(&agent_addr))
        .await
        .expect("connect writer to agent");
    wait_for_connected_peer(db_writer.node.as_ref()).await;

    for node in [&db_writer.node, &db_agent.node] {
        assert!(
            !gents::store::identity::node_p2p(node)
                .expect("P2P transport")
                .get_collections()
                .await
                .expect("list P2P subscriptions")
                .iter()
                .any(|collection| collection == "ReplicatedEvent"),
            "an application collection must not be subscribed"
        );
    }
    assert!(
        refuses_runtime_local(
            writer
                .add_replicator(
                    std::slice::from_ref(&agent_addr),
                    &["ReplicatedEvent".to_owned()],
                    &PairingFilters::default(),
                )
                .await
        ),
        "an application collection must not be replicated to a peer"
    );

    let source_doc_id = write_replicated_event(&db_writer.node, EXTERNAL_ID, "signup").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        assert!(
            replicated_events(db_agent.node.as_ref()).await.is_empty(),
            "ReplicatedEvent {source_doc_id} reached the peer"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(replicated_events(db_writer.node.as_ref()).await.len(), 1);

    db_writer.node.shutdown().await.expect("node shutdown");
    db_agent.node.shutdown().await.expect("node shutdown");
}
