use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::managed_exec::ManagedProcess;

const MAX_CONTENT_LENGTH: usize = 8 * 1024 * 1024;

const MAX_PENDING: usize = 32;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HEADER_BYTES: usize = 8 * 1024;

pub(crate) struct LspClient {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: AtomicI64,
    pub server_name: String,
    process: Arc<Mutex<ManagedProcess>>,
    reader: tokio::task::JoinHandle<()>,
    stderr_task: tokio::task::JoinHandle<()>,
    encoding: Mutex<super::encoding::PositionEncoding>,
    capabilities: Mutex<Value>,
    workspace: std::path::PathBuf,
    init_options: Option<Value>,
    settings: Option<Value>,
    versions: Mutex<HashMap<String, i64>>,
    initialize_timeout: Duration,
}

impl LspClient {
    pub fn start(
        mut process: ManagedProcess,
        server_name: String,
        config: &super::LspToolConfig,
        server: &super::catalog::CatalogServer,
    ) -> Result<Self, String> {
        let stdin = process.stdin.take().ok_or("process stdin missing")?;
        let stdout = process.stdout.take().ok_or("process stdout missing")?;
        let stderr = process.stderr.take();
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        let stdin = Arc::new(Mutex::new(stdin));
        let stdin_reader = stdin.clone();
        let reader = tokio::spawn(async move {
            if let Err(error) = read_loop(stdout, pending_reader, stdin_reader).await {
                tracing::warn!(%error, "lsp reader exited");
            }
        });
        let stderr_task = tokio::spawn(async move {
            if let Some(mut stderr) = stderr {
                let mut buf = [0u8; 4096];
                loop {
                    match tokio::io::AsyncReadExt::read(&mut stderr, &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            }
        });
        Ok(Self {
            stdin,
            pending,
            next_id: AtomicI64::new(1),
            server_name,
            process: Arc::new(Mutex::new(process)),
            reader,
            stderr_task,
            encoding: Mutex::new(super::encoding::PositionEncoding::Utf8),
            capabilities: Mutex::new(Value::Null),
            workspace: config.workspace.clone(),
            init_options: server.init_options.clone(),
            settings: server.settings.clone(),
            versions: Mutex::new(HashMap::new()),
            initialize_timeout: server
                .warmup_timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(INITIALIZE_TIMEOUT)
                .max(INITIALIZE_TIMEOUT)
                .min(Duration::from_secs(60)),
        })
    }

    pub async fn initialize(&self) -> Result<Value, String> {
        let root_uri = format!("file://{}", self.workspace.display());
        let params = json!({
            "processId": null,
            "rootUri": root_uri,
            "rootPath": self.workspace,
            "initializationOptions": self.init_options.clone().unwrap_or(Value::Null),
            "capabilities": {
                "workspace": {
                    "applyEdit": false,
                    "workspaceEdit": {
                        "documentChanges": true,
                        "resourceOperations": ["create", "rename", "delete"]
                    },
                    "configuration": true
                },
                "textDocument": {
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "definition": { "linkSupport": true },
                    "synchronization": { "didSave": true }
                },
                "general": {
                    "positionEncodings": ["utf-8", "utf-16"]
                }
            },
            "clientInfo": { "name": "gents", "version": "0" }
        });
        let result = self
            .request_with_timeout("initialize", params, self.initialize_timeout)
            .await?;
        let encodings = result
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str)
            .map(|enc| vec![enc.to_string()])
            .unwrap_or_else(|| {
                result
                    .pointer("/capabilities/general/positionEncodings")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(ToOwned::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            });
        *self.encoding.lock().await = super::encoding::negotiate(&encodings);
        *self.capabilities.lock().await = result.clone();
        let _ = self.notify("initialized", json!({})).await;
        if let Some(settings) = &self.settings {
            let _ = self
                .notify(
                    "workspace/didChangeConfiguration",
                    json!({ "settings": settings }),
                )
                .await;
        }
        Ok(result)
    }

    pub async fn track_open(&self, uri: &str, version: i64) {
        self.versions.lock().await.insert(uri.to_string(), version);
    }

    pub async fn tracked_version(&self, uri: &str) -> Option<i64> {
        self.versions.lock().await.get(uri).copied()
    }

    pub async fn position_encoding(&self) -> super::encoding::PositionEncoding {
        *self.encoding.lock().await
    }

    pub async fn capabilities(&self) -> Value {
        self.capabilities.lock().await.clone()
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request_with_timeout(method, params, REQUEST_TIMEOUT)
            .await
    }

    pub async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.len() >= MAX_PENDING {
                return Err("pending LSP request cap reached".into());
            }
            pending.insert(id, tx);
        }
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        if let Err(error) = write_message(&mut *self.stdin.lock().await, &payload).await {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        let inflight = InflightRequest {
            stdin: self.stdin.clone(),
            pending: self.pending.clone(),
            id,
            completed: Arc::new(AtomicBool::new(false)),
        };
        let runtime = crate::tool_call_lifecycle::runtime::current_tool_runtime_context();
        let cancel = runtime.map(|scope| scope.cancellation_token);
        let outcome = tokio::select! {
            biased;
            _ = async {
                if let Some(token) = &cancel {
                    token.cancelled().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                self.cancel(id).await;
                self.pending.lock().await.remove(&id);
                Err(format!("LSP request {method} cancelled"))
            }
            result = tokio::time::timeout(timeout, rx) => {
                match result {
                    Ok(Ok(value)) => value,
                    Ok(Err(_)) => Err(format!("LSP request {method} dropped")),
                    Err(_) => {
                        self.cancel(id).await;
                        self.pending.lock().await.remove(&id);
                        Err(format!("LSP request {method} timed out"))
                    }
                }
            }
        };
        inflight.completed.store(true, Ordering::SeqCst);
        outcome
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        write_message(&mut *self.stdin.lock().await, &payload).await
    }

    pub async fn cancel(&self, id: i64) {
        let _ = self.notify("$/cancelRequest", json!({ "id": id })).await;
    }

    pub async fn shutdown_exit(&self) {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
        let mut process = self.process.lock().await;
        process.terminate().await;
        self.reader.abort();
        self.stderr_task.abort();
        let mut pending = self.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err("language server closed".into()));
        }
    }
}

struct InflightRequest {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    id: i64,
    completed: Arc<AtomicBool>,
}

impl Drop for InflightRequest {
    fn drop(&mut self) {
        if self.completed.swap(true, Ordering::SeqCst) {
            return;
        }
        let stdin = self.stdin.clone();
        let pending = self.pending.clone();
        let id = self.id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                pending.lock().await.remove(&id);
                let payload = json!({
                    "jsonrpc": "2.0",
                    "method": "$/cancelRequest",
                    "params": { "id": id }
                });
                let _ = write_message(&mut *stdin.lock().await, &payload).await;
            });
        }
    }
}

async fn write_message(stdin: &mut ChildStdin, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|err| err.to_string())?;
    if body.len() > MAX_CONTENT_LENGTH {
        return Err("JSON-RPC payload exceeds Content-Length cap".into());
    }
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin
        .write_all(header.as_bytes())
        .await
        .map_err(|err| err.to_string())?;
    stdin
        .write_all(&body)
        .await
        .map_err(|err| err.to_string())?;
    stdin.flush().await.map_err(|err| err.to_string())
}

async fn read_loop(
    stdout: ChildStdout,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    stdin: Arc<Mutex<ChildStdin>>,
) -> Result<(), String> {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|err| err.to_string())?;
            if n == 0 {
                let mut pending = pending.lock().await;
                for (_, tx) in pending.drain() {
                    let _ = tx.send(Err("language server stdout closed".into()));
                }
                return Ok(());
            }
            if headers.len() + line.len() > MAX_HEADER_BYTES {
                return Err("LSP header exceeded bound".into());
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            headers.push_str(&line);
        }
        let length = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .ok_or_else(|| "missing Content-Length".to_string())?;
        if length > MAX_CONTENT_LENGTH {
            return Err("incoming Content-Length exceeds cap".into());
        }
        let mut body = vec![0u8; length];
        reader
            .read_exact(&mut body)
            .await
            .map_err(|err| err.to_string())?;
        let value: Value = serde_json::from_slice(&body).map_err(|err| err.to_string())?;
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            if let Some(id) = value.get("id").cloned() {
                let result = if method == "workspace/applyEdit" {
                    json!({ "applied": false })
                } else {
                    json!(null)
                };
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                });
                write_message(&mut *stdin.lock().await, &reply).await?;
                continue;
            }
        }
        if let Some(id) = value.get("id").and_then(Value::as_i64) {
            let result = if let Some(error) = value.get("error") {
                Err(error.to_string())
            } else {
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            };
            if let Some(tx) = pending.lock().await.remove(&id) {
                let _ = tx.send(result);
            }
        }
    }
}
