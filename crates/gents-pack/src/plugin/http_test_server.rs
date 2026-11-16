use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::http_calls::MAX_RESPONSE_BYTES;

pub struct Server {
    pub port: u16,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub async fn start() -> Self {
        use axum::extract::{Path, State};
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::IntoResponse;
        use axum::routing::get;

        type Hits = Arc<Mutex<Vec<String>>>;
        async fn handle(
            State(hits): State<Hits>,
            Path(path): Path<String>,
            headers: HeaderMap,
        ) -> axum::response::Response {
            hits.lock().unwrap().push(path.clone());
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            };
            if let Some(to) = path.strip_prefix("redirect/") {
                let to = to.replacen('/', "://", 1);
                return (StatusCode::FOUND, [("location", to)], String::new()).into_response();
            }
            match path.as_str() {
                "hello" => (StatusCode::OK, [("x-served", "yes")], "hello").into_response(),
                "big" => "x".repeat(MAX_RESPONSE_BYTES + 1).into_response(),
                "slow" => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "late".into_response()
                }
                "cookie" => (StatusCode::OK, [("set-cookie", "session=1")], "set").into_response(),
                "headers" => json!({
                    "cookie": header("cookie"),
                    "authorization": header("authorization"),
                    "x-plugin": header("x-plugin"),
                })
                .to_string()
                .into_response(),
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
        let hits: Hits = Arc::default();
        let app = axum::Router::new()
            .route("/{*path}", get(handle).post(handle))
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { port, hits }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }

    pub fn literal(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn hits(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }
}
