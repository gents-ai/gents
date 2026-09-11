use crate::support::*;

use std::fs;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde_json::Value;
use tokio::sync::Semaphore;
use uuid::Uuid;

const TRANSACTION_STAGE_DEADLINE: Duration = Duration::from_secs(10);
const TX_RECLAIM_DEADLINE: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone)]
struct StallingProxyState {
    target_origin: String,
    client: reqwest::Client,
    mutation_staged: std::sync::Arc<Semaphore>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_sigkill_mid_apply_leaves_db_unchanged() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-rb-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-rollback-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;

    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;
    let config_path = root.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    config["agent_principal"]["display_name"] = Value::String("Interrupted principal".to_string());
    config["agent_behaviors"][0]["display_name"] =
        Value::String("Interrupted behavior".to_string());
    config["contexts"][0]["description"] = Value::String("Interrupted context".to_string());
    config["tools"][0]["display_name"] = Value::String("Interrupted tools".to_string());
    config["inference_backends"][0]["name"] = Value::String("Interrupted backend".to_string());
    write_json_file(&config_path, &config)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let collections = [
        "InferenceBackend",
        "InferenceProfile",
        "ToolServiceRegistry",
        "Tools",
        "AgentContext",
        "AgentBehavior",
        "Task",
        "Schedule",
        "EventSource",
        "Trigger",
        "AgentPrincipal",
    ];
    let mut pre_apply = std::collections::BTreeMap::new();
    for c in &collections {
        pre_apply.insert(*c, count_collection_rows(&graphql, c).await?);
    }

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;

    // Hold the first successfully staged transactional mutation at the HTTP
    // boundary. This makes the SIGKILL point deterministic without a timing
    // sleep or a production-only test hook in config apply.
    let (proxy_graphql, mutation_staged, proxy) =
        start_stalling_transaction_proxy(&graphql).await?;
    let mut cli = std::process::Command::new(crate::support::cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .current_dir(&home_dir)
        .args([
            "config",
            "apply",
            "--root",
            root_str,
            "--graphql",
            &proxy_graphql,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawning gents config apply through transaction proxy")?;

    let staged_permit = tokio::time::timeout(TRANSACTION_STAGE_DEADLINE, mutation_staged.acquire())
        .await
        .context("config apply did not stage a transactional mutation")?
        .context("transaction proxy closed before staging a mutation")?;
    staged_permit.forget();
    if let Some(status) = cli
        .try_wait()
        .context("checking config apply before SIGKILL")?
    {
        let output = cli
            .wait_with_output()
            .context("capturing config apply that exited before SIGKILL")?;
        return Err(anyhow!(
            "config apply exited before the rollback probe could kill an active transaction ({status})\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    cli.kill().context("SIGKILL CLI")?;
    cli.wait().context("reap CLI")?;
    proxy.abort();

    let deadline = Instant::now() + TX_RECLAIM_DEADLINE;
    loop {
        let mut current = std::collections::BTreeMap::new();
        for c in &collections {
            current.insert(*c, count_collection_rows(&graphql, c).await?);
        }

        if current == pre_apply {
            return Ok(());
        }
        if Instant::now() > deadline {
            let drift: Vec<String> = collections
                .iter()
                .filter_map(|c| {
                    let pre = pre_apply[c];
                    let now = current[c];
                    (pre != now).then(|| format!("{c}: pre={pre} now={now}"))
                })
                .collect();
            return Err(anyhow!(
                "after SIGKILL, DB shows post-apply drift: {}",
                drift.join(", ")
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn start_stalling_transaction_proxy(
    graphql: &str,
) -> Result<(
    String,
    std::sync::Arc<Semaphore>,
    tokio::task::JoinHandle<()>,
)> {
    let mut origin = url::Url::parse(graphql).context("parsing GraphQL endpoint")?;
    origin.set_path("");
    origin.set_query(None);
    origin.set_fragment(None);
    let mutation_staged = std::sync::Arc::new(Semaphore::new(0));
    let state = StallingProxyState {
        target_origin: origin.as_str().trim_end_matches('/').to_owned(),
        client: reqwest::Client::new(),
        mutation_staged: mutation_staged.clone(),
    };
    let app = Router::new()
        .fallback(any(forward_and_stall_transactional_mutation))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding transaction proxy")?;
    let address = listener.local_addr().context("transaction proxy address")?;
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let path = url::Url::parse(graphql)
        .context("parsing GraphQL endpoint path")?
        .path()
        .to_owned();
    Ok((format!("http://{address}{path}"), mutation_staged, server))
}

async fn forward_and_stall_transactional_mutation(
    State(state): State<StallingProxyState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let is_transactional_mutation = headers.contains_key("x-defradb-tx")
        && serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("query")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|query| query.trim_start().starts_with("mutation"));
    let target = format!("{}{}", state.target_origin, uri);
    let upstream = match state
        .client
        .request(method, target)
        .headers(headers)
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("transaction proxy: {error}"),
            )
                .into_response();
        }
    };
    let status = upstream.status();
    let bytes = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("transaction proxy: {error}"),
            )
                .into_response();
        }
    };
    if is_transactional_mutation && status.is_success() {
        state.mutation_staged.add_permits(1);
        std::future::pending::<()>().await;
    }
    (status, bytes).into_response()
}

async fn count_collection_rows(graphql: &str, collection: &str) -> Result<usize> {
    let response = graphql_query(graphql, &format!("{{ {collection} {{ _docID }} }}")).await?;
    Ok(response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0))
}
