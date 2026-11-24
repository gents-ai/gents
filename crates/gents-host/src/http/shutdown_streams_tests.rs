use super::*;
use axum::{body::Bytes, routing::get};
use gents_store::client_journal::paths::DesktopPaths;
use gents_store::client_journal::principal_identity::PrincipalIdentity;
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
        .with_signing_key(gents::defra_node::SigningKey::Ed25519(
            owner.private_key_bytes().to_vec(),
        ))
        .with_node_acp_enabled()
        .with_http(
            gents::defra_node::HttpConfig::with_listener(listener)
                .unwrap()
                .with_extra_routes(gents_store::test_support::local_authentication_route(
                    std::sync::Arc::new(owner.clone()),
                ))
                .with_router_transform(std::sync::Arc::new(move |router| streams.wrap(router))),
        )
        .build()
        .await
        .unwrap();
    node.add_collection("type ShutdownProbe { name: String }")
        .await
        .unwrap();
    let access = gents_store::config_client::ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(graphql, owner.did())
            .with_client_signing_capability(
                gents_store::config_client::ClientSigningCapability::from_ed25519_key(
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

/// A JWT-shaped bearer whose claims expire `seconds` from now; the runtime
/// reads only the claims, since DefraDB verified the bearer at the handshake.
fn bearer(seconds: u64, issuer: &str) -> String {
    use base64::Engine as _;
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + seconds;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::json!({ "exp": exp, "iss": issuer }).to_string());
    format!("Bearer header.{claims}.signature")
}

/// An event stream that sends an event every 100 ms.
fn ticking_event_stream() -> Response {
    let events = futures_util::stream::unfold((), |()| async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Some((
            Ok::<_, std::io::Error>(Bytes::from_static(b"event: next\ndata: {}\n\n")),
            (),
        ))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(events))
        .unwrap()
}

/// Everything a stream delivers until it ends, or `BOUND` passes.
async fn drain(mut response: reqwest::Response) -> String {
    let mut text = String::new();
    let _ = tokio::time::timeout(BOUND, async {
        while let Ok(Some(chunk)) = response.chunk().await {
            text.push_str(&String::from_utf8_lossy(&chunk));
        }
    })
    .await;
    text
}

async fn subscribe(endpoint: &str, path: &str, authorization: &str) -> reqwest::Response {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{endpoint}{path}"))
        .header(header::ACCEPT, "text/event-stream")
        .header(header::AUTHORIZATION, authorization)
        .send()
        .await
        .unwrap()
}

fn subscription_router(streams: &ShutdownStreams) -> Router {
    streams.wrap(
        Router::new()
            .route(
                "/api/v0/graphql",
                axum::routing::post(|| async { ticking_event_stream() }),
            )
            .route(
                "/events",
                axum::routing::post(|| async { ticking_event_stream() }),
            ),
    )
}

#[tokio::test]
async fn a_subscription_ends_at_its_bearer_expiry_and_says_so() {
    let (_shutdown, observed) = watch::channel(false);
    let streams = ShutdownStreams::new(observed);
    let endpoint = serve(subscription_router(&streams)).await;
    let delivered =
        drain(subscribe(&endpoint, "/api/v0/graphql", &bearer(1, "did:key:z")).await).await;
    assert!(delivered.contains("event: next"), "{delivered}");
    assert!(
        delivered.ends_with("event: gents-end\ndata: {\"reason\":\"expired\"}\n\n"),
        "the stream ends at the bearer's expiry and names it: {delivered}"
    );
}

/// The subscriber holds node read permission for its first three events and
/// loses it before the fourth.
#[tokio::test]
async fn a_subscriber_that_lost_node_read_receives_nothing_more() {
    let (_shutdown, observed) = watch::channel(false);
    let streams = ShutdownStreams::new(observed);
    let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = checks.clone();
    streams.admit_members_with(std::sync::Arc::new(move |did: String| {
        let check = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let held = check < 3 && did == "did:key:z";
        Box::pin(async move { held })
    }));
    let endpoint = serve(subscription_router(&streams)).await;
    let delivered =
        drain(subscribe(&endpoint, "/api/v0/graphql", &bearer(60, "did:key:z")).await).await;
    assert_eq!(
        delivered,
        format!(
            "{}event: gents-end\ndata: {{\"reason\":\"revoked\"}}\n\n",
            "event: next\ndata: {}\n\n".repeat(3)
        ),
        "no event after the revocation, then the reason"
    );
    assert_eq!(checks.load(std::sync::atomic::Ordering::SeqCst), 4);
}

#[tokio::test]
async fn only_graphql_subscriptions_are_bounded_by_their_bearer() {
    let (_shutdown, observed) = watch::channel(false);
    let streams = ShutdownStreams::new(observed);
    streams.admit_members_with(std::sync::Arc::new(|_| Box::pin(async { false })));
    let endpoint = serve(subscription_router(&streams)).await;
    let mut open = subscribe(&endpoint, "/events", &bearer(0, "did:key:z")).await;
    let events = tokio::time::timeout(Duration::from_secs(2), async {
        let mut events = 0;
        while let Ok(Some(chunk)) = open.chunk().await {
            assert!(!chunk.starts_with(b"event: gents-end"));
            events += 1;
            if events == 5 {
                break;
            }
        }
        events
    })
    .await
    .unwrap();
    assert_eq!(events, 5, "another event stream keeps delivering");
}
