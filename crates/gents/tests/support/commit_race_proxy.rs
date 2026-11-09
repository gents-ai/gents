//! An HTTP proxy in front of the test home's DefraDB endpoint that runs a
//! hook before forwarding each request. A test uses it to land a concurrent
//! write between a client transaction's reads and its commit, so DefraDB
//! itself aborts that commit with a genuine transaction conflict. A
//! client-signed creation conversation (a WebSocket) passes through
//! unobserved; its transaction still commits over HTTP.

use std::sync::Arc;

use futures::future::BoxFuture;

/// Runs before a request is forwarded, with its method and path.
pub type ProxyHook = Arc<dyn Fn(String, String) -> BoxFuture<'static, ()> + Send + Sync>;

pub struct CommitRaceProxy {
    /// The proxy's GraphQL URL, in the upstream's `/api/v0/graphql` form.
    pub graphql: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for CommitRaceProxy {
    fn drop(&mut self) {
        self.server.abort();
    }
}

struct ProxyState {
    upstream: String,
    hook: ProxyHook,
    client: reqwest::Client,
}

impl CommitRaceProxy {
    pub async fn spawn(upstream_graphql: &str, hook: ProxyHook) -> Self {
        let upstream = upstream_graphql
            .strip_suffix("/api/v0/graphql")
            .expect("upstream GraphQL endpoint")
            .to_owned();
        let state = Arc::new(ProxyState {
            upstream,
            hook,
            client: reqwest::Client::new(),
        });
        let router = axum::Router::new().fallback(forward).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind commit race proxy");
        let graphql = format!(
            "http://{}/api/v0/graphql",
            listener.local_addr().expect("proxy address")
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self { graphql, server }
    }

    /// The User's client-signed access through this proxy.
    pub fn client_access(&self, identity: &dyn gents::NodeIdentity) -> gents::ConfigAccess {
        gents::ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(
                &self.graphql,
                identity.did(),
            )
            .with_client_signing_capability(identity.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
        )
    }
}

/// A DefraDB HTTP transaction commit: `POST /api/v0/tx/{id}`.
pub fn is_commit(method: &str, path: &str) -> bool {
    method == "POST"
        && path
            .strip_prefix("/api/v0/tx/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

async fn forward(
    axum::extract::State(state): axum::extract::State<Arc<ProxyState>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;

    let (mut parts, body) = request.into_parts();
    if parts
        .headers
        .get(axum::http::header::UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
    {
        use axum::extract::FromRequestParts as _;
        let upgrade = axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &())
            .await
            .expect("websocket upgrade");
        let upstream = state.upstream.replacen("http://", "ws://", 1);
        return upgrade.on_upgrade(move |socket| pipe_websocket(socket, upstream, parts));
    }
    let body = axum::body::to_bytes(body, usize::MAX)
        .await
        .expect("proxied request body");
    let path = parts
        .uri
        .path_and_query()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_default();
    (state.hook)(parts.method.as_str().to_owned(), path.clone()).await;
    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
        .expect("proxied request method");
    let mut outbound = state
        .client
        .request(method, format!("{}{path}", state.upstream));
    for (name, value) in &parts.headers {
        // The signed authorization binds the request's authority, so the
        // client's Host travels with it.
        if name != axum::http::header::CONTENT_LENGTH {
            outbound = outbound.header(name.as_str(), value.as_bytes());
        }
    }
    let response = match outbound.body(body).send().await {
        Ok(response) => response,
        Err(error) => {
            return (axum::http::StatusCode::BAD_GATEWAY, error.to_string()).into_response();
        }
    };
    let status =
        axum::http::StatusCode::from_u16(response.status().as_u16()).expect("upstream status");
    let mut headers = axum::http::HeaderMap::new();
    for (name, value) in response.headers() {
        if name != reqwest::header::CONTENT_LENGTH && name != reqwest::header::TRANSFER_ENCODING {
            if let (Ok(name), Ok(value)) = (
                axum::http::HeaderName::from_bytes(name.as_str().as_bytes()),
                axum::http::HeaderValue::from_bytes(value.as_bytes()),
            ) {
                headers.insert(name, value);
            }
        }
    }
    let bytes = response.bytes().await.unwrap_or_default();
    (status, headers, bytes).into_response()
}

async fn pipe_websocket(
    client: axum::extract::ws::WebSocket,
    upstream: String,
    parts: axum::http::request::Parts,
) {
    use axum::extract::ws::Message as Client;
    use futures::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest as _, Message as Node};

    let path = parts
        .uri
        .path_and_query()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_default();
    let mut request = format!("{upstream}{path}")
        .into_client_request()
        .expect("upstream websocket request");
    for (name, value) in &parts.headers {
        let name = name.as_str();
        if !name.starts_with("sec-websocket")
            && !matches!(name, "connection" | "upgrade" | "content-length")
        {
            request.headers_mut().insert(
                tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_bytes())
                    .expect("header name"),
                tokio_tungstenite::tungstenite::http::HeaderValue::from_bytes(value.as_bytes())
                    .expect("header value"),
            );
        }
    }
    let config = defra_http::client_signed_create::websocket_config();
    let Ok((node, _)) =
        tokio_tungstenite::connect_async_with_config(request, Some(config), false).await
    else {
        return;
    };
    let (mut node_tx, mut node_rx) = node.split();
    let (mut client_tx, mut client_rx) = client.split();
    // The client ends the conversation once it holds the response; the node
    // drops its side, so only the client's own close ends the pipe.
    let to_node = async {
        while let Some(Ok(message)) = client_rx.next().await {
            let message = match message {
                Client::Text(text) => Node::text(text.as_str()),
                Client::Binary(bytes) => Node::binary(bytes),
                Client::Ping(bytes) => Node::Ping(bytes),
                Client::Pong(bytes) => Node::Pong(bytes),
                Client::Close(_) => break,
            };
            if node_tx.send(message).await.is_err() {
                break;
            }
        }
        let _ = node_tx.close().await;
    };
    let to_client = async {
        while let Some(Ok(message)) = node_rx.next().await {
            let message = match message {
                Node::Text(text) => Client::text(text.as_str()),
                Node::Binary(bytes) => Client::Binary(bytes),
                Node::Ping(bytes) => Client::Ping(bytes),
                Node::Pong(bytes) => Client::Pong(bytes),
                Node::Close(_) | Node::Frame(_) => break,
            };
            if client_tx.send(message).await.is_err() {
                break;
            }
        }
    };
    tokio::join!(to_node, to_client);
}
