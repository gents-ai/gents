use super::*;
use axum::{body::Bytes, routing::get};
use gents_desktop_core::client::{DesktopPaths, PrincipalIdentity};
use std::time::Duration;

/// Longer than an ended stream needs, shorter than DefraDB's 30-second
/// network drain, so a stream still open at shutdown fails the bound.
const BOUND: Duration = Duration::from_secs(10);

fn endless_event_stream() -> Response {
    let first = futures_util::stream::once(async {
        Ok::<_, std::io::Error>(Bytes::from_static(b"event: next\ndata: {}\n\n"))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(
            first.chain(futures_util::stream::pending()),
        ))
        .unwrap()
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await });
    format!("http://{address}")
}

#[tokio::test]
async fn event_streams_end_and_new_ones_are_refused_once_shutdown_begins() {
    let (shutdown, observed) = watch::channel(false);
    let streams = ShutdownStreams::new(observed);
    let endpoint = serve(
        streams.wrap(
            Router::new()
                .route("/events", get(|| async { endless_event_stream() }))
                .route("/plain", get(|| async { "plain" })),
        ),
    )
    .await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut open = client
        .get(format!("{endpoint}/events"))
        .header(header::ACCEPT, "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    assert!(open.chunk().await.unwrap().is_some(), "the stream delivers");

    shutdown.send_replace(true);
    let ended = tokio::time::timeout(BOUND, async {
        while open.chunk().await.unwrap().is_some() {}
    })
    .await;
    assert!(
        ended.is_ok(),
        "an open event stream ends when shutdown begins"
    );

    let refused = client
        .get(format!("{endpoint}/events"))
        .header(header::ACCEPT, "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    let plain = client
        .get(format!("{endpoint}/plain"))
        .send()
        .await
        .unwrap();
    assert_eq!(plain.status(), StatusCode::OK);
    assert_eq!(plain.text().await.unwrap(), "plain");
}

/// The served runtime's shutdown (journey: an enrolled client's remote
/// observer holds a GraphQL subscription per collection) completes while a
/// subscription is open, instead of waiting out the node's network drain.
#[tokio::test]
async fn node_shutdown_with_an_open_graphql_subscription_completes_within_the_bound() {
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path().join("operator"));
    paths.ensure_root_dirs().await.unwrap();
    let owner = PrincipalIdentity::load_or_create(&paths).await.unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let graphql = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let (shutdown, observed) = watch::channel(false);
    let streams = ShutdownStreams::new(observed);
    let node = gents::defra_node::Node::builder()
        .with_node_identity_did(owner.did())
        .with_node_acp_enabled()
        .with_http(
            gents::defra_node::HttpConfig::with_listener(listener)
                .unwrap()
                .with_router_transform(std::sync::Arc::new(move |router| streams.wrap(router))),
        )
        .build()
        .await
        .unwrap();
    node.add_collection("type ShutdownProbe { name: String }")
        .await
        .unwrap();
    let access = gents::store::config_client::ConfigAccess::Graphql(
        gents::store::config_client::GraphqlEndpoint::as_principal(graphql, owner.did())
            .with_client_signing_capability(
                gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                    owner.private_key_bytes(),
                )
                .unwrap(),
            )
            .unwrap(),
    );
    let mut subscription = access
        .subscribe("subscription { ShutdownProbe { _docID } }")
        .await
        .unwrap();

    shutdown.send_replace(true);
    let ended = tokio::time::timeout(BOUND, subscription.recv())
        .await
        .expect("the open subscription ends when shutdown begins");
    assert!(matches!(ended, Ok(None)), "{ended:?}");
    tokio::time::timeout(BOUND, node.shutdown())
        .await
        .expect("node shutdown completes while a client still holds the connection")
        .unwrap();
}
