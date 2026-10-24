use gents::mcp_pool::McpPool;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::types::ToolServiceToolView;

#[derive(Debug, thiserror::Error)]
pub(super) enum SpeechTransportError {
    #[error("The speech worker could not start. Restart the Gents client.")]
    Spawn(#[source] std::io::Error),
    #[error("The speech worker is busy or unavailable. Stop voice and try again.")]
    Unavailable,
    #[error("The speech worker disconnected.")]
    Disconnected(#[source] oneshot::error::RecvError),
    #[error("The speech worker is busy. Stop voice and try again.")]
    Busy,
    #[error("The speech service timed out. Check its connection before trying again.")]
    TimedOut,
    #[error(
        "The speech service could not list its tools. Check its connection and service settings."
    )]
    ListFailed,
    #[error("The speech service returned too many tools to inspect.")]
    TooManyTools,
    #[error("The speech tool call failed. Check the selected service and tool settings.")]
    CallFailed,
    #[error("The speech tool reported a failure. Check its service settings.")]
    ToolFailure,
    #[error("The speech tool must return one structured result.")]
    UnstructuredResult,
    #[error("The speech tool must return a JSON result. Check its speech adapter configuration.")]
    NotJson,
}

type Result<T, E = SpeechTransportError> = std::result::Result<T, E>;

pub(super) enum Operation {
    List,
    Call { tool: String, arguments: Value },
}

pub(super) struct Request {
    pub service: String,
    pub endpoint: String,
    pub actor: String,
    pub send_node_did: bool,
    pub operation: Operation,
}

pub(super) enum Response {
    Tools(Vec<ToolServiceToolView>),
    Value(Value),
}

struct Work {
    request: Request,
    cancelled: CancellationToken,
    reply: oneshot::Sender<Result<Response>>,
}

pub(super) struct SpeechTransport {
    sender: mpsc::Sender<Work>,
}

impl SpeechTransport {
    pub fn new() -> Result<Self> {
        let (sender, receiver) = mpsc::channel(4);
        std::thread::Builder::new()
            .name("gents-speech".into())
            .spawn(move || run_worker(receiver))
            .map_err(SpeechTransportError::Spawn)?;
        Ok(Self { sender })
    }

    pub async fn request(&self, request: Request) -> Result<Response> {
        let (reply, result) = oneshot::channel();
        let cancelled = CancellationToken::new();
        let _cancel_on_drop = cancelled.clone().drop_guard();
        self.sender
            .try_send(Work {
                request,
                cancelled,
                reply,
            })
            .map_err(|_| SpeechTransportError::Unavailable)?;
        result.await.map_err(SpeechTransportError::Disconnected)?
    }
}

/// rmcp 1.3 emits complete messages from its independently spawned service
/// tasks, including audio and provider error bodies. A future-local span or
/// dispatcher does not protect those tasks. This current-thread runtime, the
/// desktop's one runtime besides its entry runtime, keeps the pool and all of
/// its transport tasks under a silent thread dispatcher.
/// The worker receives no database handles, writes no journal and opens no
/// audio devices; callers retain cancellation and product error presentation.
fn run_worker(mut receiver: mpsc::Receiver<Work>) {
    let _silence = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(async move {
        let pool = McpPool::new();
        let mut calls = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                work = receiver.recv() => {
                    let Some(work) = work else { break; };
                    if work.cancelled.is_cancelled() { continue; }
                    while calls.try_join_next().is_some() {}
                    if calls.len() >= 4 {
                        let _ = work.reply.send(Err(SpeechTransportError::Busy));
                        continue;
                    }
                    let pool = pool.clone();
                    calls.spawn(async move {
                        tokio::select! {
                            biased;
                            _ = work.cancelled.cancelled() => {},
                            result = tokio::time::timeout(std::time::Duration::from_secs(90), execute(&pool, work.request)) => {
                                let result = result.unwrap_or(Err(SpeechTransportError::TimedOut));
                                let _ = work.reply.send(result);
                            }
                        }
                    });
                }
                _ = calls.join_next(), if !calls.is_empty() => {},
            }
        }
        calls.abort_all();
        while calls.join_next().await.is_some() {}
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
}

async fn execute(pool: &McpPool, request: Request) -> Result<Response> {
    let pool = pool.for_agent(&request.actor);
    let actor = request.send_node_did.then_some(request.actor.as_str());
    match request.operation {
        Operation::List => {
            let result = pool
                .list_tools_with_node_did(&request.service, &request.endpoint, actor)
                .await
                .map_err(|_| SpeechTransportError::ListFailed)?;
            if result.tools.len() > 512 {
                return Err(SpeechTransportError::TooManyTools);
            }
            Ok(Response::Tools(
                result
                    .tools
                    .into_iter()
                    .map(|tool| ToolServiceToolView {
                        name: tool.name.to_string(),
                        description: tool.description.map(|description| description.to_string()),
                    })
                    .collect(),
            ))
        }
        Operation::Call { tool, arguments } => {
            let result = pool
                .call_tool_with_node_did(
                    &request.service,
                    &request.endpoint,
                    &tool,
                    arguments,
                    actor,
                )
                .await
                .map_err(|_| SpeechTransportError::CallFailed)?;
            if result.is_error.unwrap_or(false) {
                return Err(SpeechTransportError::ToolFailure);
            }
            if let Some(value) = result.structured_content {
                return Ok(Response::Value(value));
            }
            let texts: Vec<_> = result
                .content
                .iter()
                .filter_map(|block| block.raw.as_text().map(|text| text.text.as_str()))
                .collect();
            if texts.len() != 1 {
                return Err(SpeechTransportError::UnstructuredResult);
            }
            let value =
                serde_json::from_str(texts[0]).map_err(|_| SpeechTransportError::NotJson)?;
            Ok(Response::Value(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::error_chain::ErrorChain;
    use serde_json::json;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn dropping_a_request_cancels_its_queued_or_running_transport_work() {
        let (sender, mut receiver) = mpsc::channel(4);
        let transport = SpeechTransport { sender };
        let caller = tokio::spawn(async move {
            transport
                .request(Request {
                    service: "speech".into(),
                    endpoint: "http://127.0.0.1/mcp".into(),
                    actor: "did:key:client".into(),
                    send_node_did: false,
                    operation: Operation::List,
                })
                .await
        });
        let work = receiver.recv().await.unwrap();
        assert!(!work.cancelled.is_cancelled());
        caller.abort();
        assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            work.cancelled.cancelled(),
        )
        .await
        .expect("dropping a client call must retire its transport work");
    }

    async fn service(call_result: Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(move |request: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let result = match body["method"].as_str().unwrap() {
                    "initialize" => json!({
                        "protocolVersion":"2025-06-18", "capabilities":{"tools":{}},
                        "serverInfo":{"name":"speech-fixture","version":"1.0.0"}
                    }),
                    "notifications/initialized" => return ResponseTemplate::new(202),
                    "tools/list" => json!({"tools":[{
                        "name":"speech", "description":"Configured speech adapter",
                        "inputSchema":{"type":"object"}
                    }]}),
                    "tools/call" => call_result.clone(),
                    _ => return ResponseTemplate::new(400),
                };
                ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc":"2.0", "id":body["id"], "result":result
                }))
            })
            .mount(&server)
            .await;
        server
    }

    fn request(server: &MockServer, operation: Operation, send_node_did: bool) -> Request {
        Request {
            service: "configured-speech".into(),
            endpoint: format!("{}/mcp", server.uri()),
            actor: "did:key:actual-client".into(),
            send_node_did,
            operation,
        }
    }

    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn transport_tasks_cannot_emit_payloads_into_a_verbose_parent_logger() {
        let server =
            service(json!({"content":[],"structuredContent":{"text":"private-audio-marker"}}))
                .await;
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(output.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        let (sender, receiver) = mpsc::channel(4);
        let worker = std::thread::spawn(move || {
            tracing::subscriber::with_default(subscriber, || {
                tracing::info!("logger-control-marker");
                run_worker(receiver);
            })
        });
        let transport = SpeechTransport { sender };
        let result = transport
            .request(request(
                &server,
                Operation::Call {
                    tool: "speech".into(),
                    arguments: json!({"audio":"private-audio-marker"}),
                },
                true,
            ))
            .await
            .unwrap();
        let Response::Value(value) = result else {
            panic!("expected speech result")
        };
        assert_eq!(value["text"], "private-audio-marker");
        let requests = server.received_requests().await.unwrap();
        let call = requests
            .iter()
            .find(|request| {
                serde_json::from_slice::<Value>(&request.body)
                    .is_ok_and(|body| body["method"] == "tools/call")
            })
            .unwrap();
        assert_eq!(call.headers["x-node-did"], "did:key:actual-client");
        let body: Value = serde_json::from_slice(&call.body).unwrap();
        assert_eq!(body["params"]["name"], "speech");
        assert_eq!(body["params"]["arguments"]["audio"], "private-audio-marker");
        drop(transport);
        worker.join().unwrap();
        let text = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(text.contains("logger-control-marker"));
        assert!(!text.contains("private-audio-marker"), "{text}");
    }

    #[tokio::test]
    async fn configured_transport_lists_tools_and_decodes_json_text_without_an_actor_header() {
        let server =
            service(json!({"content":[{"type":"text","text":"{\"text\":\"transcript\"}"}]})).await;
        let transport = SpeechTransport::new().unwrap();
        let Response::Tools(tools) = transport
            .request(request(&server, Operation::List, false))
            .await
            .unwrap()
        else {
            panic!("expected tools");
        };
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "speech");
        for _ in 0..64 {
            assert!(matches!(
                transport
                    .request(request(&server, Operation::List, false))
                    .await
                    .unwrap(),
                Response::Tools(_)
            ));
        }
        let Response::Value(value) = transport
            .request(request(
                &server,
                Operation::Call {
                    tool: "speech".into(),
                    arguments: json!({"text":"draft"}),
                },
                false,
            ))
            .await
            .unwrap()
        else {
            panic!("expected transcription")
        };
        assert_eq!(value, json!({"text":"transcript"}));
        for request in server.received_requests().await.unwrap() {
            assert!(!request.headers.contains_key("x-node-did"));
        }
    }

    #[tokio::test]
    async fn provider_errors_and_malformed_results_never_echo_their_payload() {
        for result in [
            json!({"isError":true,"content":[{"type":"text","text":"private-audio-marker"}]}),
            json!({"content":[{"type":"text","text":"private-audio-marker"}]}),
            json!({"content":[]}),
        ] {
            let server = service(result).await;
            let transport = SpeechTransport::new().unwrap();
            let error = match transport
                .request(request(
                    &server,
                    Operation::Call {
                        tool: "speech".into(),
                        arguments: json!({}),
                    },
                    false,
                ))
                .await
            {
                Err(error) => error,
                Ok(_) => panic!("invalid speech result accepted"),
            };
            assert!(!ErrorChain(&error)
                .to_string()
                .contains("private-audio-marker"));
        }
    }
}
