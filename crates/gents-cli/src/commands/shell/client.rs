use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, Stream, StreamExt};
use gents_codex_protocol as codex;
use rapidhash::{HashMapExt, RapidHashMap};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{timeout, timeout_at, Instant};
use tokio_tungstenite::tungstenite::{self, Message};

/// The upgrade and the `initialize` handshake together.
const CONNECT_WAIT: Duration = Duration::from_secs(10);
/// For each request's answer. The shell handles one intent at a time, so an unanswered
/// request would otherwise stop every tab.
pub(super) const ANSWER_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub(super) enum ClientError {
    #[error("could not reach the runtime at {url}: {source}")]
    Connect {
        url: String,
        #[source]
        source: Box<tungstenite::Error>,
    },
    #[error("the runtime closed the connection")]
    Closed,
    #[error("{method}: {message}")]
    Rpc {
        method: &'static str,
        code: i64,
        message: String,
    },
    #[error("{method} answered in an unexpected shape: {source}")]
    Decode {
        method: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("no answer from the runtime: check `gents status`, then reopen the shell ({method} waited {secs}s)")]
    Timeout { method: &'static str, secs: u64 },
}

#[derive(Debug)]
pub(super) enum ClientEvent {
    Notification(Box<codex::ServerNotification>),
    /// The read or write error, when the socket broke rather than closed.
    Closed(Option<String>),
}

type Reply = Result<Value, codex::JSONRPCErrorError>;
/// `None` once the socket has closed.
type Pending = Arc<Mutex<Option<RapidHashMap<i64, oneshot::Sender<Reply>>>>>;

/// One JSON-RPC connection to a Codex endpoint.
pub(super) struct Client {
    outgoing: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicI64,
    answer_wait: Duration,
}

impl Client {
    /// Sends `initialize`, awaits its answer, then sends `initialized`. Each later request
    /// waits `answer_wait` for its answer.
    pub(super) async fn connect(
        url: &str,
        agent: String,
        events: mpsc::UnboundedSender<(String, ClientEvent)>,
        runtime: &Handle,
        answer_wait: Duration,
    ) -> Result<Client, ClientError> {
        let deadline = Instant::now() + CONNECT_WAIT;
        let expired = |method| ClientError::Timeout {
            method,
            secs: CONNECT_WAIT.as_secs(),
        };
        let (socket, _) = timeout_at(deadline, tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| expired("websocket upgrade"))?
            .map_err(|source| ClientError::Connect {
                url: url.to_string(),
                source: Box::new(source),
            })?;
        let (mut sink, stream) = socket.split();
        let (outgoing, mut queued) = mpsc::unbounded_channel::<String>();
        let writer = agent.clone();
        let broken = events.clone();
        runtime.spawn(async move {
            while let Some(text) = queued.recv().await {
                if let Err(error) = sink.send(Message::Text(text.into())).await {
                    tracing::warn!(agent = %writer, %error, "could not write to the runtime");
                    let _ = broken.send((writer, ClientEvent::Closed(Some(error.to_string()))));
                    return;
                }
            }
            let _ = sink.close().await;
        });
        let pending: Pending = Arc::new(Mutex::new(Some(RapidHashMap::new())));
        runtime.spawn(read(stream, pending.clone(), agent, events));
        let client = Client {
            outgoing,
            pending,
            next_id: AtomicI64::new(1),
            answer_wait,
        };
        let initialize = codex::InitializeParams {
            client_info: codex::ClientInfo {
                name: "gents-shell".to_string(),
                title: None,
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            capabilities: None,
        };
        timeout_at(
            deadline,
            client.request::<_, IgnoredAny>("initialize", &initialize),
        )
        .await
        .map_err(|_| expired("initialize"))??;
        client.send(codex::JSONRPCMessage::Notification(
            codex::JSONRPCNotification {
                method: "initialized".to_string(),
                params: None,
            },
        ))?;
        Ok(client)
    }

    pub(super) async fn request<P, R>(
        &self,
        method: &'static str,
        params: &P,
    ) -> Result<R, ClientError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        self.pending
            .lock()
            .expect("no holder of the pending map panics")
            .as_mut()
            .ok_or(ClientError::Closed)?
            .insert(id, answer);
        self.send(codex::JSONRPCMessage::Request(codex::JSONRPCRequest {
            id: codex::RequestId::Integer(id),
            method: method.to_string(),
            params: Some(serde_json::to_value(params).expect("Codex params serialize to JSON")),
            trace: None,
        }))?;
        let Ok(answer) = timeout(self.answer_wait, answered).await else {
            if let Some(waiting) = self
                .pending
                .lock()
                .expect("no holder of the pending map panics")
                .as_mut()
            {
                waiting.remove(&id);
            }
            return Err(ClientError::Timeout {
                method,
                secs: self.answer_wait.as_secs(),
            });
        };
        match answer {
            Ok(Ok(result)) => serde_json::from_value(result)
                .map_err(|source| ClientError::Decode { method, source }),
            Ok(Err(error)) => Err(ClientError::Rpc {
                method,
                code: error.code,
                message: error.message,
            }),
            Err(_) => Err(ClientError::Closed),
        }
    }

    fn send(&self, message: codex::JSONRPCMessage) -> Result<(), ClientError> {
        let text = serde_json::to_string(&message).expect("a JSON-RPC message serializes");
        self.outgoing.send(text).map_err(|_| ClientError::Closed)
    }
}

/// On close, drops every pending answer, so each waiting request reads `Closed`.
async fn read<S>(
    mut stream: S,
    pending: Pending,
    agent: String,
    events: mpsc::UnboundedSender<(String, ClientEvent)>,
) where
    S: Stream<Item = Result<Message, tungstenite::Error>> + Unpin,
{
    let mut broke = None;
    loop {
        let message = match stream.next().await {
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                tracing::warn!(%agent, %error, "could not read from the runtime");
                broke = Some(error.to_string());
                break;
            }
            None => break,
        };
        let Message::Text(text) = message else {
            continue;
        };
        let (id, reply) = match serde_json::from_str::<codex::JSONRPCMessage>(&text) {
            Ok(codex::JSONRPCMessage::Response(response)) => (response.id, Ok(response.result)),
            Ok(codex::JSONRPCMessage::Error(error)) => (error.id, Err(error.error)),
            Ok(codex::JSONRPCMessage::Notification(notification)) => {
                match codex::ServerNotification::try_from(notification) {
                    Ok(notification) => {
                        let event = ClientEvent::Notification(Box::new(notification));
                        let _ = events.send((agent.clone(), event));
                    }
                    Err(error) => {
                        tracing::debug!(%agent, %error, "dropping a notification the shell does not read")
                    }
                }
                continue;
            }
            // The endpoint's approval policy is `never`, so it has nothing to ask.
            Ok(codex::JSONRPCMessage::Request(request)) => {
                tracing::warn!(%agent, method = %request.method, "dropping a request from the runtime: the shell answers none");
                continue;
            }
            Err(error) => {
                tracing::warn!(%agent, %error, "dropping a message that is not JSON-RPC");
                continue;
            }
        };
        let codex::RequestId::Integer(id) = id else {
            tracing::warn!(%agent, %id, "dropping an answer to a request the shell did not send");
            continue;
        };
        let answer = pending
            .lock()
            .expect("no holder of the pending map panics")
            .as_mut()
            .and_then(|waiting| waiting.remove(&id));
        if let Some(answer) = answer {
            let _ = answer.send(reply);
        }
    }
    pending
        .lock()
        .expect("no holder of the pending map panics")
        .take();
    let _ = events.send((agent, ClientEvent::Closed(broke)));
}
