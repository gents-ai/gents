use std::time::{Duration, Instant};

use gents::runtime::agent::p2p_reconcile::{
    enrollment_authority_channel, run_enrollment_reconciler, run_pairing_reconciler,
    EmbeddedRemoteP2pAdmin, RemoteP2pAdmin,
};
use gents::store::{config_client::ConfigAccess, graphql::escape_graphql_string};
use tokio_util::sync::CancellationToken;

use crate::support::{self, TestDb};

async fn display_rows(db: &TestDb) -> Vec<serde_json::Value> {
    ConfigAccess::local(db.node.clone())
        .execute("{ ProviderReductionDisplay { reduction_key requester_did node_did } }")
        .await
        .unwrap()["data"]["ProviderReductionDisplay"]
        .as_array()
        .unwrap()
        .clone()
}

async fn create_display(db: &TestDb, key: &str, requester: &str) {
    let mutation = format!(
        r#"mutation {{ create_ProviderReductionDisplay(input: {{ reduction_key: "{}", node_did: "{}", requester_did: "{}", session_id: "session", request_doc_id: "request", summary: "summary", messages_compacted: 1, original_tokens: 10, compacted_tokens: 2, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
        escape_graphql_string(key),
        escape_graphql_string(db.node_identity.did()),
        escape_graphql_string(requester)
    );
    ConfigAccess::local(db.node.clone())
        .write("test.reduction_display", &mutation)
        .await
        .unwrap();
}

async fn wait_display(db: &TestDb, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if display_rows(db)
            .await
            .iter()
            .any(|row| row["reduction_key"] == expected)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "display {expected} was not delivered"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn enrolled_clients_receive_only_their_retained_and_live_display_records() {
    let runtime = support::test_p2p_db("reduction-display-runtime").await;
    let first = support::test_p2p_db("reduction-display-first").await;
    let second = support::test_p2p_db("reduction-display-second").await;
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let (owner, authority) = enrollment_authority_channel();
    let enrollment = tokio::spawn(run_enrollment_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        owner,
        cancel.clone(),
    ));
    let pairing = tokio::spawn(run_pairing_reconciler(
        runtime.node.clone(),
        runtime.node_identity.clone(),
        authority.clone(),
        cancel.clone(),
    ));
    let owner_did = runtime.node_identity.did();
    gents::documents::ensure_node(&runtime.node, owner_did)
        .await
        .unwrap();
    create_display(&runtime, "first-retained", first.node_identity.did()).await;
    create_display(&runtime, "second-retained", second.node_identity.did()).await;
    let private = format!(
        r#"mutation {{ create_ProviderContextReduction(input: {{ reduction_key: "private", node_did: "{}", requester_did: "{}", session_id: "session", compacted_prefix_json: "private prompt" }}) {{ _docID }} }}"#,
        escape_graphql_string(owner_did),
        escape_graphql_string(first.node_identity.did())
    );
    ConfigAccess::local(runtime.node.clone())
        .write("test.private_reduction", &private)
        .await
        .unwrap();
    let (_, runtime_address) = support::enrollment::wait_for_peer_identity(&runtime.node).await;
    for client in [&first, &second] {
        let (peer, address) = support::enrollment::wait_for_peer_identity(&client.node).await;
        support::enrollment::authorize_enrollment_peer(
            runtime.node.clone(),
            "display-network",
            "display-network",
            runtime.node_identity.clone(),
            client.node_identity.clone(),
            &peer,
            &address,
        )
        .await;
        EmbeddedRemoteP2pAdmin::new(client.node.clone())
            .connect(std::slice::from_ref(&runtime_address))
            .await
            .unwrap();
    }
    wait_display(&first, "first-retained").await;
    wait_display(&second, "second-retained").await;
    create_display(&runtime, "first-live", first.node_identity.did()).await;
    create_display(&runtime, "second-live", second.node_identity.did()).await;
    wait_display(&first, "first-live").await;
    wait_display(&second, "second-live").await;
    // Check beyond the arrival of the expected records: another route may
    // deliver an incorrectly scoped record after those records have arrived.
    let isolation_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        for client in [&first, &second] {
            let rows = display_rows(client).await;
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().all(|row| {
                row["requester_did"] == client.node_identity.did() && row["node_did"] == owner_did
            }));
            let private = ConfigAccess::local(client.node.clone())
                .execute("{ ProviderContextReduction { reduction_key } }")
                .await
                .unwrap();
            assert!(private["data"]["ProviderContextReduction"]
                .as_array()
                .unwrap()
                .is_empty());
        }
        if Instant::now() >= isolation_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cancel.cancel();
    enrollment.await.unwrap().unwrap();
    pairing.await.unwrap().unwrap();
    first.node.shutdown().await;
    second.node.shutdown().await;
    runtime.node.shutdown().await;
}
