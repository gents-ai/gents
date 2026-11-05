use super::*;
use crate::store::config_client::GraphqlEndpoint;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn fragmented_subscription_preserves_utf8_and_completion() {
    let wire = "event: next\r\ndata: {\"data\":{\"Parcel\":[{\"value\":\"café\"}]}}\r\n\r\nevent: complete\ndata: {}\n\n";
    let chunks = wire
        .as_bytes()
        .chunks(3)
        .map(|chunk| Ok::<_, reqwest::Error>(chunk.to_vec()))
        .collect::<Vec<_>>();
    let mut subscription = GraphqlSubscription {
        events: Some(Box::pin(futures::stream::iter(chunks).eventsource())),
        renew_at: None,
    };
    assert_eq!(
        subscription.recv().await.unwrap(),
        Some(json!({"data":{"Parcel":[{"value":"café"}]}}))
    );
    assert_eq!(subscription.recv().await.unwrap(), None);
    assert_eq!(subscription.recv().await.unwrap(), None);
}

#[tokio::test]
async fn subscription_returns_explicit_denial_without_retrying_or_falling_back() {
    let wire = b"event: next\ndata: {\"errors\":[{\"message\":\"opaque\",\"extensions\":{\"code\":\"PERMISSION_DENIED\"}}]}\n\n";
    let chunks = vec![Ok::<_, reqwest::Error>(wire.to_vec())];
    let mut subscription = GraphqlSubscription {
        events: Some(Box::pin(futures::stream::iter(chunks).eventsource())),
        renew_at: None,
    };
    let error = subscription.recv().await.unwrap_err();
    assert!(error.is_permission_denied());
    assert!(!super::super::retry::is_retryable_ambiguous_write(&error));
    assert!(subscription.recv().await.unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn subscription_closes_for_reauthentication_without_waiting_for_data() {
    let mut subscription = GraphqlSubscription {
        events: Some(Box::pin(futures::stream::pending())),
        renew_at: Some(Instant::now() + Duration::from_secs(1)),
    };
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(subscription.recv().await.unwrap(), None);
    assert!(subscription.events.is_none());
}

#[tokio::test]
async fn native_subscription_uses_caller_and_observes_nonbranchable_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let owner_path = directory.path().join("owner.key");
    let reader_path = directory.path().join("reader.key");
    let (owner, reader) = tokio::task::spawn_blocking(move || {
        (
            KeyIdentity::load_or_create(owner_path, None).unwrap(),
            KeyIdentity::load_or_create(reader_path, None).unwrap(),
        )
    })
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(owner.did())
            .with_http(defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap())
            .build()
            .await
            .unwrap(),
    );
    let policy = node
        .add_dac_policy_as(
            &defra_core::RequestContext::default().with_identity(Some(owner.did().to_owned())),
            "name: Subscription identity\nresources:\n  - name: parcels\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
        )
        .await
        .unwrap();
    let access = ConfigAccess::local_as(node.clone(), owner.did().parse().unwrap());
    access
        .add_collection(&format!(
        "type SubscriptionParcel @policy(id: \"{}\", resource: \"parcels\") {{ value: String }}",
        crate::store::graphql::escape_graphql_string(&policy),
    ))
        .await
        .unwrap();
    let created = access.write("test.subscription.seed", "mutation { create_SubscriptionParcel(input: [{value:\"visible\"}, {value:\"private\"}]) { _docID } }").await.unwrap();
    let rows = crate::store::graphql::mutation_documents(&created, "create_SubscriptionParcel")
        .unwrap()
        .expect("created parcels");
    assert_eq!(rows.len(), 2);
    let visible = rows[0]["_docID"].as_str().unwrap();
    let private = rows[1]["_docID"].as_str().unwrap();
    node.add_dac_actor_relationship(
        owner.did(),
        "SubscriptionParcel",
        visible,
        "reader",
        reader.did(),
    )
    .await
    .unwrap();
    let reader_access =
        ConfigAccess::Graphql(GraphqlEndpoint::as_principal(endpoint, reader.did()));
    let mut subscription = reader_access
        .subscribe(
            "subscription { SubscriptionParcel(showDeleted: true) { _docID _deleted value } }",
        )
        .await
        .unwrap();
    for (doc, value) in [(private, "private-update"), (visible, "visible-update")] {
        access.write("test.subscription.update", &format!(
            "mutation {{ update_SubscriptionParcel(docID: \"{}\", input: {{value:\"{}\"}}) {{ _docID }} }}",
            crate::store::graphql::escape_graphql_string(doc),
            crate::store::graphql::escape_graphql_string(value),
        )).await.unwrap();
    }
    let event = tokio::time::timeout(Duration::from_secs(10), subscription.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(event["data"]["SubscriptionParcel"][0]["_docID"], visible);
    assert_eq!(
        event["data"]["SubscriptionParcel"][0]["value"],
        "visible-update"
    );
    access
        .write(
            "test.subscription.delete",
            &format!(
                "mutation {{ delete_SubscriptionParcel(docID: \"{}\") {{ _docID }} }}",
                crate::store::graphql::escape_graphql_string(visible),
            ),
        )
        .await
        .unwrap();
    let deleted = tokio::time::timeout(Duration::from_secs(10), subscription.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(deleted["data"]["SubscriptionParcel"][0]["_docID"], visible);
    assert_eq!(deleted["data"]["SubscriptionParcel"][0]["_deleted"], true);
    drop(subscription);
    node.shutdown().await.expect("node shutdown");
}
