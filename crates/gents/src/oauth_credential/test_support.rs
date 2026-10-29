use std::sync::Arc;

use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) static TOKEN_URL_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn test_node() -> Arc<EmbeddedNode> {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("embedded node"),
    );
    crate::schema::ensure_runtime_schemas(&node)
        .await
        .expect("schemas");
    node
}

pub(crate) fn bound_bearer(
    credential_id: &str,
) -> Option<std::sync::Arc<super::DbCredentialBearer>> {
    super::bearer::bearer_registry()
        .lock()
        .expect("bearer registry mutex poisoned")
        .get(credential_id)
        .cloned()
}

pub(crate) async fn seed_credential(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    provider: &str,
    expires_at: DateTime<Utc>,
) {
    seed_credential_with_refresh_token(node, node_did, provider, expires_at, "refresh-TEST").await;
}

pub(crate) async fn seed_credential_with_refresh_token(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    provider: &str,
    expires_at: DateTime<Utc>,
    refresh_token: &str,
) {
    let credential = crate::oauth_credential::OAuthCredential {
        doc_id: None,
        credential_id: crate::oauth_credential::oauth_credential_id(node_did, provider),
        node_did: node_did.to_string(),
        provider: provider.to_string(),
        access_token: "access-TEST".into(),
        refresh_token: refresh_token.into(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: expires_at,
        last_refresh: Some(Utc::now()),
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
    };
    crate::oauth_credential::upsert_oauth_credential(node, &credential)
        .await
        .expect("seed credential");
}

pub(crate) fn unsigned_jwt(payload: serde_json::Value) -> String {
    use base64::Engine;
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap());
    format!("{header}.{payload}.sig")
}

pub(crate) async fn one_shot_token_server(
    status: u16,
    body: &'static str,
) -> (String, tokio::task::JoinHandle<String>) {
    gated_token_server(status, body, None).await
}

pub(crate) async fn gated_token_server(
    status: u16,
    body: &'static str,
    gate: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}/v1/oauth/token", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = socket.read(&mut chunk).await.expect("read");
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(idx) = text.find("\r\n\r\n") {
                let headers = &text[..idx];
                let content_length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length: ")
                            .or_else(|| line.strip_prefix("Content-Length: "))
                    })
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or(0);
                if buf.len() >= idx + 4 + content_length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf).into_owned();
        if let Some((received, release)) = gate {
            received.send(()).ok();
            release.await.ok();
        }
        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.expect("write");
        socket.shutdown().await.ok();
        request
    });
    (url, handle)
}
