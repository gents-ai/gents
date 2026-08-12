use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::managed_exec::ManagedProcess;

const MAX_CONTENT_LENGTH: usize = 8 * 1024 * 1024;

pub(crate) struct LspClient {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: AtomicI64,
    pub server_name: String,
    process: Arc<Mutex<ManagedProcess>>,
    reader: tokio::task::JoinHandle<()>,
}

impl LspClient {
    pub fn start(mut process: ManagedProcess, server_name: String) -> Result<Self, String> {
        let stdin = process.stdin.take().ok_or("process stdin missing")?;
        let stdout = process.stdout.take().ok_or("process stdout missing")?;
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
        Ok(Self {
            stdin,
            pending,
            next_id: AtomicI64::new(1),
            server_name,
            process: Arc::new(Mutex::new(process)),
            reader,
        })
    }

    pub async fn initialize(&self) -> Result<Value, String> {
        let params = json!({
            "processId": null,
            "capabilities": {
                "workspace": {
                    "applyEdit": false,
                    "workspaceEdit": {
                        "documentChanges": true,
                        "resourceOperations": ["create", "rename", "delete"]
                    }
                },
                "textDocument": {
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "definition": { "linkSupport": true }
                },
                "general": {
                    "positionEncodings": ["utf-8", "utf-16"]
                }
            },
            "clientInfo": { "name": "gents", "version": "0" }
        });
        let result = self.request("initialize", params).await?;
        let _ = self.notify("initialized", json!({})).await;
        Ok(result)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        write_message(&mut *self.stdin.lock().await, &payload).await?;
        tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| format!("LSP request {method} timed out"))?
            .map_err(|_| format!("LSP request {method} dropped"))?
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
        let _ = self
            .notify("$/cancelRequest", json!({ "id": id }))
            .await;
    }

    pub async fn shutdown_exit(&self) {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
        let mut process = self.process.lock().await;
        process.terminate().await;
        self.reader.abort();
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
    stdin.write_all(&body).await.map_err(|err| err.to_string())?;
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
                return Ok(());
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
