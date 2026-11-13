use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const LIMIT: Duration = Duration::from_secs(120);

struct GateState {
    started: watch::Sender<bool>,
    released: watch::Sender<bool>,
}

pub struct HeldTool {
    root: PathBuf,
    executable: PathBuf,
    endpoint: String,
    state: Arc<GateState>,
    shutdown: CancellationToken,
}

async fn run(State(state): State<Arc<GateState>>) -> Result<&'static str, StatusCode> {
    state.started.send_replace(true);
    let mut released = state.released.subscribe();
    tokio::time::timeout(LIMIT, released.wait_for(|released| *released))
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)?
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok("The held tool completed after its owner released it.")
}

async fn started(State(state): State<Arc<GateState>>) -> Result<&'static str, StatusCode> {
    let mut started = state.started.subscribe();
    tokio::time::timeout(LIMIT, started.wait_for(|started| *started))
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)?
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok("The held tool is running.")
}

async fn release(State(state): State<Arc<GateState>>) -> &'static str {
    state.released.send_replace(true);
    "The held tool was released."
}

impl HeldTool {
    pub async fn start(root: &Path) -> Result<Self> {
        let root = tokio::fs::canonicalize(root)
            .await
            .context("resolve the held tool workspace")?;
        let interpreter = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("python3")
                .args(["-c", "import sys; sys.stdout.write(sys.executable)"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("resolve the held tool Python interpreter before its deadline")?
        .context("python3 is required for the native held tool fixture")?;
        ensure!(
            interpreter.status.success(),
            "python3 could not resolve its executable"
        );
        let executable = tokio::fs::canonicalize(
            String::from_utf8(interpreter.stdout).context("Python executable is UTF-8")?,
        )
        .await
        .context("resolve the native Python executable")?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let state = Arc::new(GateState {
            started: watch::channel(false).0,
            released: watch::channel(false).0,
        });
        let shutdown = CancellationToken::new();
        let router = Router::new()
            .route("/run", get(run))
            .route("/started", get(started))
            .route("/release", post(release))
            .with_state(state.clone());
        let stopped = shutdown.clone();
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
            {
                tracing::error!(%error, "held tool listener failed");
            }
        });
        Ok(Self {
            root,
            executable,
            endpoint,
            state,
            shutdown,
        })
    }

    pub fn configure(&self, document: &mut Value, node: &str) {
        let context_id = document["agents"]
            .as_array_mut()
            .expect("the selected Agent configuration")
            .iter_mut()
            .filter(|agent| agent["node_did"] == node)
            .map(|agent| {
                let context = agent["context_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .unwrap_or("native-live-held-context")
                    .to_owned();
                agent["context_id"] = json!(context);
                context
            })
            .next()
            .expect("a selected Agent belongs to the node");
        if !document["contexts"].is_array() {
            document["contexts"] = json!([]);
        }
        let contexts = document["contexts"].as_array_mut().unwrap();
        let index = contexts
            .iter()
            .position(|context| context["node_did"] == node && context["context_id"] == context_id)
            .unwrap_or_else(|| {
                contexts.push(json!({"node_did":node,"context_id":context_id}));
                contexts.len() - 1
            });
        let context = &mut contexts[index];
        let tools_id = context["tools_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .unwrap_or("native-live-held-tools")
            .to_owned();
        context["tools_id"] = json!(tools_id);
        let previous = context["system_prompt"].as_str().unwrap_or_default();
        context["system_prompt"] = json!(format!("{previous}\nWhen the current request supplies a held tool command, call it once in the foreground and wait for its result. Its owner releases it after observing the running tool. Do not replace it with a background call. Follow the current request after the tool completes."));
        if !document["tools"].is_array() {
            document["tools"] = json!([]);
        }
        let tools = document["tools"].as_array_mut().unwrap();
        let index = tools
            .iter()
            .position(|tools| tools["node_did"] == node && tools["tools_id"] == tools_id)
            .unwrap_or_else(|| {
                tools.push(json!({"node_did":node,"tools_id":tools_id}));
                tools.len() - 1
            });
        let tools = &mut tools[index];
        if !tools["host"].is_object() {
            tools["host"] = json!({});
        }
        tools["host"]["root"] = json!(self.root);
        tools["host"]["bash"] = json!({
            "mode":"ReadOnly", "network_mode":"enabled",
            "read_only_commands":[self.executable],
            "allowed_argv_prefixes":[[self.executable,"-c"]],
            "timeout_secs":LIMIT.as_secs(), "max_timeout_secs":LIMIT.as_secs(),
        });
        if !tools["built_ins"].is_object() {
            tools["built_ins"] = json!({});
        }
        tools["built_ins"]["enable_goal_tools"] = json!(true);
    }

    pub fn prompt(&self) -> String {
        let script = format!(
            "import urllib.request\nwith urllib.request.build_opener(urllib.request.ProxyHandler({{}})).open({:?}, timeout={}) as response:\n print(response.read().decode())",
            format!("{}/run", self.endpoint), LIMIT.as_secs(),
        );
        format!(
            "Call bash once in the foreground with exactly these arguments: {}. Wait for this held tool to return, then acknowledge completion briefly.",
            json!({"command":self.executable,"args":["-c",script],"timeout_secs":LIMIT.as_secs()}),
        )
    }

    pub fn descriptor(&self) -> Value {
        json!({
            "startedUrl":format!("{}/started",self.endpoint),
            "releaseUrl":format!("{}/release",self.endpoint),
            "prompt":self.prompt(),
        })
    }

    pub async fn wait_started(&mut self, deadline: Duration) -> Result<()> {
        let mut started = self.state.started.subscribe();
        tokio::time::timeout(deadline, started.wait_for(|started| *started))
            .await
            .context("the live model did not start its held host tool before the deadline")?
            .context("the held tool readiness owner closed")?;
        Ok(())
    }

    pub async fn release(&mut self) -> Result<()> {
        self.state.released.send_replace(true);
        Ok(())
    }
}

impl Drop for HeldTool {
    fn drop(&mut self) {
        self.state.released.send_replace(true);
        self.shutdown.cancel();
    }
}
