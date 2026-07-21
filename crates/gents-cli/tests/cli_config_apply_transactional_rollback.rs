mod support;
use support::*;

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

/// SIGKILL the CLI mid-apply and assert that transaction **atomicity** leaves
/// the database at the pre-apply snapshot — a transaction that never sees a
/// `commit` produces no externally-visible mutations. The orphaned handle on
/// the server is reclaimed via connection drop / per-request HTTP timeout
/// (not an active idle-GC sweep — see
/// `docs/superpowers/audits/2026-05-20-defradb-tx-idle-timeout-audit.md` (removed from the tree; see git history)).
/// This test exercises the operationally-meaningful failure mode (Ctrl-C,
/// OOM, container restart) against a real node, not the recorder.
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

    // Snapshot row counts for every collection in CONFIG_APPLY_ORDER so the
    // atomicity assertion covers the full apply surface, not just a subset.
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

    // No `spawn_cli` helper takes env vars today; spawn directly.
    let mut cli = std::process::Command::new(support::cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .env("GENTS_CONFIG_APPLY_SLEEP_MS", PER_COLLECTION_SLEEP_MS)
        .current_dir(&home_dir)
        .args(["config", "apply", "--root", root_str, "--graphql", &graphql])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawning gents config apply with apply-sleep env")?;

    // With per-collection sleep = 200 ms and CONFIG_APPLY_ORDER having 9
    // collections, full apply takes at least 1.8 s. Sleep 400 ms — past
    // begin and at least the first batched collection mutation, well before
    // commit. The sleep widens the kill window deterministically; without
    // it a fast local apply could complete in under our delay.
    thread::sleep(KILL_DELAY);
    cli.kill().context("SIGKILL CLI")?;
    cli.wait().context("reap CLI")?;

    // Allow the orphaned tx handle to be reclaimed via connection drop /
    // per-request HTTP timeout (not an active idle-GC sweep — see audit
    // doc 2026-05-20-defradb-tx-idle-timeout-audit.md). Atomicity already
    // guarantees the pre-apply snapshot is what readers see; this poll just
    // waits for that visibility to stabilize.
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
            // Build a diff line for collections whose counts changed.
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
