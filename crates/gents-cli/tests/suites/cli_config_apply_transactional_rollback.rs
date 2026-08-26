use crate::support::*;

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

const KILL_DELAY: Duration = Duration::from_millis(400);
const TX_RECLAIM_DEADLINE: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const PER_COLLECTION_SLEEP_MS: &str = "200";

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

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let collections = [
        "InferenceBackend",
        "InferenceProfile",
        "ToolServiceRegistry",
        "ToolSelection",
        "AgentBehavior",
        "Task",
        "Schedule",
        "EventTrigger",
        "AgentPrincipal",
    ];
    let mut pre_apply = std::collections::BTreeMap::new();
    for c in &collections {
        pre_apply.insert(*c, count_collection_rows(&graphql, c).await?);
    }

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;

    let mut cli = std::process::Command::new(crate::support::cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .env("GENTS_CONFIG_APPLY_SLEEP_MS", PER_COLLECTION_SLEEP_MS)
        .current_dir(&home_dir)
        .args(["config", "apply", "--root", root_str, "--graphql", &graphql])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawning gents config apply with apply-sleep env")?;

    thread::sleep(KILL_DELAY);
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

async fn count_collection_rows(graphql: &str, collection: &str) -> Result<usize> {
    let response = graphql_query(graphql, &format!("{{ {collection} {{ _docID }} }}")).await?;
    Ok(response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0))
}
