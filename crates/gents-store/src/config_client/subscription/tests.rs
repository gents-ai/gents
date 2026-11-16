use super::*;
use crate::config_client::GraphqlEndpoint;
use crate::identity::{KeyIdentity, NodeIdentity};
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
        ended: None,
    };
    assert_eq!(
        subscription.recv().await.unwrap(),
        Some(json!({"data":{"Parcel":[{"value":"café"}]}}))
    );
    assert_eq!(subscription.recv().await.unwrap(), None);
    assert_eq!(subscription.recv().await.unwrap(), None);
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Arrival {
    Data,
    Denial,
    ErrorsList,
    ErrorsObject,
    ErrorsText,
    Complete,
    Closed,
    UnknownEvent,
    Malformed,
    LateData,
    Silence,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Received {
    Data,
    Nothing,
    Denied,
    Rejected,
    UnknownEvent,
    StreamError,
    Pending,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum End {
    RenewalDue,
    Completed,
}

const DEADLINE: Duration = Duration::from_secs(1);

fn arriving(arrival: Arrival) -> Events {
    let data = "event: next\ndata: {\"data\":{\"Parcel\":[]}}\n\n";
    let errors =
        |errors: &str| format!("event: next\ndata: {{\"data\":null,\"errors\":{errors}}}\n\n");
    let wire = match arrival {
        Arrival::Data | Arrival::LateData => data.as_bytes().to_vec(),
        Arrival::Denial => {
            errors(r#"[{"message":"opaque","extensions":{"code":"PERMISSION_DENIED"}}]"#)
                .into_bytes()
        }
        Arrival::ErrorsList => errors(r#"[{"message":"opaque"}]"#).into_bytes(),
        Arrival::ErrorsObject => errors(r#"{"message":"not a list"}"#).into_bytes(),
        Arrival::ErrorsText => errors(r#""text""#).into_bytes(),
        Arrival::Complete => b"event: complete\ndata: {}\n\n".to_vec(),
        Arrival::UnknownEvent => b"event: ping\ndata: {}\n\n".to_vec(),
        Arrival::Malformed => b"event: next\ndata: \xff\xfe\n\n".to_vec(),
        Arrival::Closed | Arrival::Silence => Vec::new(),
    };
    let delay = match arrival {
        Arrival::LateData => DEADLINE * 2,
        _ => Duration::ZERO,
    };
    let chunk = futures::stream::once(async move {
        tokio::time::sleep(delay).await;
        Ok::<_, reqwest::Error>(wire)
    });
    match arrival {
        Arrival::Closed => Box::pin(
            futures::stream::empty::<std::result::Result<Vec<u8>, reqwest::Error>>().eventsource(),
        ),
        Arrival::Silence => Box::pin(
            futures::stream::pending::<std::result::Result<Vec<u8>, reqwest::Error>>()
                .eventsource(),
        ),
        Arrival::Malformed => Box::pin(chunk.eventsource()),
        _ => Box::pin(chunk.chain(futures::stream::pending()).eventsource()),
    }
}

async fn receive(subscription: &mut GraphqlSubscription) -> (Received, bool) {
    match tokio::time::timeout(DEADLINE * 10, subscription.recv()).await {
        Err(_) => (Received::Pending, false),
        Ok(Ok(Some(_))) => (Received::Data, false),
        Ok(Ok(None)) => (Received::Nothing, false),
        Ok(Err(error)) => {
            let retryable = super::super::retry::is_retryable_ambiguous_write(&error);
            let received = match &error {
                error if error.is_permission_denied() => Received::Denied,
                ConfigClientError::SubscriptionEvent(_) => Received::UnknownEvent,
                ConfigClientError::SubscriptionStream(_) => Received::StreamError,
                _ => Received::Rejected,
            };
            (received, retryable)
        }
    }
}

#[tokio::test(start_paused = true)]
async fn every_receive_matches_the_lean_model() {
    use gents_lean_vocab::families::subscription_stream_step as family;
    let cases = tokio::task::spawn_blocking(family::cases::<Arrival, End, Received>)
        .await
        .unwrap();
    for case in cases {
        let mut subscription = GraphqlSubscription {
            events: Some(arriving(case.input.arrival)),
            renew_at: case.input.minted.then(|| Instant::now() + DEADLINE),
            ended: None,
        };
        if case.input.taken {
            subscription.take_renewal();
        }
        let (received, retryable) = receive(&mut subscription).await;
        let ended = subscription.end().map(|end| match end {
            SubscriptionEnd::RenewalDue => End::RenewalDue,
            SubscriptionEnd::Completed => End::Completed,
        });
        let (again, _) = receive(&mut subscription).await;
        case.check(family::Step {
            received,
            again,
            ended,
            retryable,
        });
    }
}

#[tokio::test]
async fn renewal_offers_match_the_lean_model() {
    gents_lean_vocab::families::subscription_stream_take_renewal::replay(|i| {
        let mut subscription = GraphqlSubscription {
            events: Some(arriving(Arrival::Silence)),
            renew_at: i.minted.then(|| Instant::now() + DEADLINE),
            ended: None,
        };
        if i.already_taken {
            subscription.take_renewal();
        }
        subscription.take_renewal().map(|period| period.as_secs())
    });
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
        crate::graphql::escape_graphql_string(&policy),
    ))
        .await
        .unwrap();
    let created = access.write("test.subscription.seed", "mutation { create_SubscriptionParcel(input: [{value:\"visible\"}, {value:\"private\"}]) { _docID } }").await.unwrap();
    let rows = crate::graphql::mutation_documents(&created, "create_SubscriptionParcel")
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
    let reader_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(endpoint, reader.did())
            .with_client_signing_capability(reader.client_signing_capability().unwrap())
            .unwrap(),
    );
    let mut subscription = reader_access
        .subscribe(
            "subscription { SubscriptionParcel(showDeleted: true) { _docID _deleted value } }",
        )
        .await
        .unwrap();
    for (doc, value) in [(private, "private-update"), (visible, "visible-update")] {
        access.write("test.subscription.update", &format!(
            "mutation {{ update_SubscriptionParcel(docID: \"{}\", input: {{value:\"{}\"}}) {{ _docID }} }}",
            crate::graphql::escape_graphql_string(doc),
            crate::graphql::escape_graphql_string(value),
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
                crate::graphql::escape_graphql_string(visible),
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
