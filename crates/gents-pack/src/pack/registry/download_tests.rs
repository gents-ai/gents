use super::*;
use futures::FutureExt;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

#[tokio::test]
async fn stalled_headers_release_the_turn_without_charging_the_next_download() {
    let reached = Arc::new(Notify::new());
    let entered = reached.clone();
    let app = axum::Router::new()
        .route(
            "/stall",
            axum::routing::get(move || {
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    std::future::pending::<axum::response::Response>().await
                }
            }),
        )
        .route("/healthy", axum::routing::get(|| async { "healthy pack" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = Arc::new(RegistryClient::new(base.clone()));
    let turn = Arc::new(Mutex::new(()));
    let stalled_client = client.clone();
    let stalled_turn = turn.clone();
    let mut stalled = tokio::spawn(async move {
        stalled_client
            .download_headers(format!("{base}/stall").parse().unwrap(), &stalled_turn)
            .await
    });
    reached.notified().await;
    let healthy = client.download_headers(
        format!("{}/healthy", client.base_url).parse().unwrap(),
        &turn,
    );
    tokio::pin!(healthy);
    assert!(healthy.as_mut().now_or_never().is_none());
    tokio::time::pause();
    tokio::time::advance(DOWNLOAD_HEADER_BOUND * 2).await;
    tokio::time::resume();
    let result = tokio::time::timeout(Duration::from_secs(5), &mut stalled).await;
    stalled.abort();
    let error = result
        .expect("stalled headers must release the turn")
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(
            &error,
            RegistryError::DownloadHeadersTimedOut { url, bound }
                if url.ends_with("/stall") && *bound == DOWNLOAD_HEADER_BOUND
        ),
        "{error}"
    );
    let response = healthy
        .await
        .expect("queue time is not the next download's header budget");
    assert_eq!(response.text().await.unwrap(), "healthy pack");
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn receiving_headers_releases_the_turn_before_the_body_finishes() {
    let body_ready = Arc::new(Notify::new());
    let body_gate = body_ready.clone();
    let app = axum::Router::new()
        .route(
            "/body",
            axum::routing::get(move || {
                let body_gate = body_gate.clone();
                async move {
                    axum::body::Body::from_stream(futures::stream::once(async move {
                        body_gate.notified().await;
                        Ok::<_, std::io::Error>("slow body")
                    }))
                }
            }),
        )
        .route("/healthy", axum::routing::get(|| async { "next pack" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = RegistryClient::new(base.clone());
    let turn = Mutex::new(());
    let response = client
        .download_headers(format!("{base}/body").parse().unwrap(), &turn)
        .await
        .unwrap();
    let next = client
        .download_headers(format!("{base}/healthy").parse().unwrap(), &turn)
        .await
        .unwrap();
    assert_eq!(next.text().await.unwrap(), "next pack");
    tokio::time::pause();
    tokio::time::advance(DOWNLOAD_HEADER_BOUND * 2).await;
    tokio::time::resume();
    body_ready.notify_one();
    assert_eq!(response.text().await.unwrap(), "slow body");
    server.abort();
    let _ = server.await;
}
