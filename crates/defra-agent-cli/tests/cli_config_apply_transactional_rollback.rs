mod support;
use support::*;

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

/// SIGKILL the CLI mid-apply and assert that DefraDB's tx GC reclaims the
/// orphaned transaction and leaves the database at the pre-apply snapshot.
/// This exercises the operationally-meaningful failure mode (Ctrl-C, OOM,
/// container restart) against a real node, not the recorder.
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

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

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

    let pre_apply_backends = count_collection_rows(&graphql, "InferenceBackend").await?;
    let pre_apply_profiles = count_collection_rows(&graphql, "InferenceProfile").await?;
    let pre_apply_tools = count_collection_rows(&graphql, "ToolSelection").await?;
    let pre_apply_tasks = count_collection_rows(&graphql, "Task").await?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;

    // No `spawn_cli` helper takes env vars today; spawn directly.
    let mut cli = std::process::Command::new(support::cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .env("DEFRA_AGENT_CONFIG_APPLY_SLEEP_MS", "200")
        .current_dir(&home_dir)
        .args(["config", "apply", "--root", root_str, "--graphql", &graphql])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawning defra-agent config apply with apply-sleep env")?;

    // With per-collection sleep = 200 ms and CONFIG_APPLY_ORDER having 9
    // collections, full apply takes at least 1.8 s. Sleep 400 ms — past
    // begin and at least the first batched collection mutation, well before
    // commit. The sleep widens the kill window deterministically; without
    // it a fast local apply could complete in under our delay.
    thread::sleep(Duration::from_millis(400));
    cli.kill().context("SIGKILL CLI")?;
    cli.wait().context("reap CLI")?;

    // Allow DefraDB's tx GC to reclaim the orphaned handle. Poll for
    // stability rather than sleep blindly.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let backends = count_collection_rows(&graphql, "InferenceBackend").await?;
        let profiles = count_collection_rows(&graphql, "InferenceProfile").await?;
        let tools = count_collection_rows(&graphql, "ToolSelection").await?;
        let tasks = count_collection_rows(&graphql, "Task").await?;

        if backends == pre_apply_backends
            && profiles == pre_apply_profiles
            && tools == pre_apply_tools
            && tasks == pre_apply_tasks
        {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(anyhow!(
                "after SIGKILL, DB still shows post-apply state: backends={} (pre={}), \
                profiles={} (pre={}), tools={} (pre={}), tasks={} (pre={})",
                backends,
                pre_apply_backends,
                profiles,
                pre_apply_profiles,
                tools,
                pre_apply_tools,
                tasks,
                pre_apply_tasks,
            ));
        }
        thread::sleep(Duration::from_millis(250));
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
