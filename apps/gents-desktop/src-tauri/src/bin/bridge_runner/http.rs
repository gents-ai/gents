#[path = "http/protocol.rs"]
mod protocol;
#[path = "http/routes.rs"]
mod routes;

use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use self::protocol::{read_http_request, write_http_response, HttpResponse};
use self::routes::handle_request;
use crate::live_fixture::LiveBridgeFixture;

pub(crate) struct BridgeRunnerServer {
    port: u16,
    stop: oneshot::Sender<()>,
    accept: JoinHandle<()>,
}

impl BridgeRunnerServer {
    pub(crate) async fn start(fixture: Arc<LiveBridgeFixture>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let (stop, mut stopped) = oneshot::channel();
        let accept = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        connections.spawn(serve_connection(stream, Arc::clone(&fixture)));
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            while connections.join_next().await.is_some() {}
        });

        Ok(Self { port, stop, accept })
    }

    pub(crate) fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub(crate) async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.accept.await;
    }
}

async fn serve_connection(mut stream: TcpStream, fixture: Arc<LiveBridgeFixture>) {
    let response = match read_http_request(&mut stream).await {
        Ok(request) => handle_request(&fixture, request)
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    }
    .unwrap_or_else(|error| HttpResponse::json_error("500 Internal Server Error", &error));
    let _ = write_http_response(
        &mut stream,
        response.status,
        response.content_type,
        &response.body,
    )
    .await;
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
}
