use std::sync::{Arc, Mutex};

use crate::support::TestResult;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};

pub struct SpeechService {
    pub port: u16,
    pub calls: Arc<Mutex<Vec<(String, Value)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl SpeechService {
    pub async fn start() -> TestResult<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let app = Router::new().route("/mcp", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                let result = match body["method"].as_str() {
                    Some("initialize") => json!({
                        "protocolVersion":"2025-06-18","capabilities":{"tools":{}},
                        "serverInfo":{"name":"speech-fixture","version":"1.0.0"}
                    }),
                    Some("notifications/initialized") => return StatusCode::ACCEPTED.into_response(),
                    Some("ping") => json!({}),
                    Some("tools/list") => json!({"tools":[
                        {"name":"transcribe","inputSchema":{"type":"object"}},
                        {"name":"synthesize","inputSchema":{"type":"object"}}
                    ]}),
                    Some("tools/call") => {
                        captured.lock().unwrap().push((
                            headers.get("x-node-did").and_then(|header| header.to_str().ok()).unwrap_or_default().to_owned(),
                            body["params"].clone(),
                        ));
                        let value = match body["params"]["name"].as_str() {
                            Some("transcribe") => json!({"text":"Return the fixture response."}),
                            Some("synthesize") => json!({"pcm":"AID/fw==","sampleRate":body["params"]["arguments"]["sampleRate"]}),
                            _ => return StatusCode::BAD_REQUEST.into_response(),
                        };
                        json!({"content":[],"structuredContent":value})
                    },
                    _ => return StatusCode::BAD_REQUEST.into_response(),
                };
                Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result})).into_response()
            }
        }));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Ok(Self { port, calls, task })
    }
}

impl Drop for SpeechService {
    fn drop(&mut self) {
        self.task.abort();
    }
}
