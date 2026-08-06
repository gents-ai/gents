use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::{
    resolve_template, scope_filter, EmbeddedRemoteP2pAdmin, RemoteP2pAdmin,
};
use serde_json::Value;

use crate::support::test_p2p_db;

#[tokio::test]
async fn signed_conversation_pairing_replays_agent_config_over_p2p() {
    let source = test_p2p_db("config-replication-source").await;
    let target = test_p2p_db("config-replication-target").await;
    let target_addr = wait_for_listen_addr(target.node.as_ref()).await;

    let response = source
        .node
        .execute(
            r#"mutation {
                create_AgentBehavior(input: {
                    behavior_id: "amy-default",
                    agent_did: "did:key:amy",
                    display_name: "Amy",
                    backend_id: "amy-backend",
                    inference_profile_id: "amy-profile",
                    enabled: true
                }) { _docID }
                create_InferenceBackend(input: {
                    backend_id: "amy-backend",
                    name: "Amy inference",
                    provider_kind: "openai",
                    enabled: true
                }) { _docID }
                create_InferenceProfile(input: {
                    profile_id: "amy-profile",
                    display_name: "Amy default"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "seed config: {:?}", response.errors);

    let template = resolve_template("conversation").expect("conversation template");
    let collections = template
        .collections
        .iter()
        .map(|collection| (*collection).to_string())
        .collect::<Vec<_>>();
    let filters = scope_filter(
        &template.scope,
        template.collections,
        "did:key:phone",
        "did:key:amy",
    );
    for collection in ["AgentBehavior", "InferenceBackend", "InferenceProfile"] {
        assert!(collections.iter().any(|value| value == collection));
        assert!(
            !filters.contains_key(collection),
            "config collection {collection} must cross the signed grant unfiltered"
        );
    }

    EmbeddedRemoteP2pAdmin::new(source.node.clone())
        .add_replicator(&[target_addr], &collections, &filters)
        .await
        .expect("install P2P config replicator");

    let data = wait_for_config(target.node.as_ref(), Duration::from_secs(30)).await;
    assert_eq!(row_count(&data, "AgentBehavior"), 1);
    assert_eq!(row_count(&data, "InferenceBackend"), 1);
    assert_eq!(row_count(&data, "InferenceProfile"), 1);
}

async fn wait_for_listen_addr(node: &gents::defra_node::EmbeddedNode) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addresses = node
            .p2p()
            .expect("P2P enabled")
            .listen_addresses()
            .await
            .expect("listen addresses");
        if let Some(address) = addresses.first() {
            return address.clone();
        }
        assert!(Instant::now() < deadline, "P2P listen address timeout");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_config(node: &gents::defra_node::EmbeddedNode, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let response = node
            .execute(
                r#"query {
                    AgentBehavior(filter: { behavior_id: { _eq: "amy-default" } }) { behavior_id }
                    InferenceBackend(filter: { backend_id: { _eq: "amy-backend" } }) { backend_id }
                    InferenceProfile(filter: { profile_id: { _eq: "amy-profile" } }) { profile_id }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "query replicated config: {:?}",
            response.errors
        );
        let data = response.data.unwrap_or(Value::Null);
        if row_count(&data, "AgentBehavior") == 1
            && row_count(&data, "InferenceBackend") == 1
            && row_count(&data, "InferenceProfile") == 1
        {
            return data;
        }
        assert!(
            Instant::now() < deadline,
            "replicated config timeout; last={data}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn row_count(data: &Value, collection: &str) -> usize {
    data.get(collection)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}
