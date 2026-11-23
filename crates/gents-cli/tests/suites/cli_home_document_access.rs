use crate::support::*;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

const REPLY: &str = "HOME_DOCUMENT_ACCESS_REPLY";

/// The runtime as a person starts it, once it reports itself serving and has
/// published the endpoint its clients find.
async fn served(home_dir: &Path) -> Result<(ServeProcess, String)> {
    let (serve, port, readiness) =
        spawn_server_with_ready_json_recovering(home_dir, allocate_port()?, &[], &[])?;
    anyhow::ensure!(
        readiness.get("status").and_then(Value::as_str) == Some("serving"),
        "the runtime did not report itself serving: {readiness}"
    );
    let graphql = graphql_url(port);
    wait_for_runtime_state_graphql(home_dir, &graphql, gents_store::test_support::STALL_CEILING)
        .await?;
    Ok((serve, graphql))
}

/// Stops the runtime as its service manager does and waits for it to finish
/// draining, so the next start finds the home as a stopped runtime leaves it.
async fn stop(mut serve: ServeProcess) -> Result<()> {
    let pid = serve.child.id();
    let status = tokio::task::spawn_blocking(move || -> Result<std::process::ExitStatus> {
        let signal = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .context("sending SIGTERM to the served runtime")?;
        anyhow::ensure!(signal.success(), "kill -TERM {pid} failed: {signal}");
        Ok(serve.child.wait()?)
    })
    .await??;
    anyhow::ensure!(
        status.success(),
        "the runtime did not stop cleanly: {status}"
    );
    Ok(())
}

fn prompted(home_dir: &Path, prompt: &str) -> Result<()> {
    let output = run_cli_text(home_dir, &["-p", prompt])?;
    anyhow::ensure!(
        output.contains(REPLY),
        "the Agent's reply did not reach the client: {output}"
    );
    Ok(())
}

/// The home's sessions with their request counts, read through the running
/// runtime at `graphql`, or by the client opening the stopped home itself.
fn sessions(home_dir: &Path, graphql: Option<&str>) -> Result<Vec<(String, u64)>> {
    let mut args = vec!["sessions", "list", "--output", "json"];
    if let Some(graphql) = graphql {
        args.extend(["--graphql", graphql]);
    }
    let list = run_cli_json(home_dir, &args)?;
    list.get("items")
        .and_then(Value::as_array)
        .with_context(|| format!("session list has no items: {list}"))?
        .iter()
        .map(|row| {
            let id = row
                .get("session_id")
                .and_then(Value::as_str)
                .with_context(|| format!("session row has no id: {row}"))?;
            let requests = row
                .get("request_count")
                .and_then(Value::as_u64)
                .with_context(|| format!("session row has no request count: {row}"))?;
            Ok((id.to_owned(), requests))
        })
        .collect()
}

fn kept(before: &[(String, u64)], after: &[(String, u64)], when: &str) -> Result<()> {
    for (session, requests) in before {
        anyhow::ensure!(
            after
                .iter()
                .any(|(id, kept)| id == session && kept >= requests),
            "session {session} with {requests} requests was not kept {when}: {after:?}"
        );
    }
    Ok(())
}

async fn a_home_is_initialized_served_used_and_reopened(document_acp: &str) -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-home-access-{}", Uuid::new_v4().simple());
    let model = MockChatEndpoint::start(&model_name, REPLY)?;

    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &format!("cli-home-access-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model_name,
            "--inference-url",
            model.endpoint(),
            "--document-acp",
            document_acp,
        ],
    )?;
    node_did_from_init(&init)?;

    let (serve, graphql) = served(&home_dir).await?;
    prompted(&home_dir, "first start: answer the home's client")?;
    let first = sessions(&home_dir, Some(&graphql))?;
    anyhow::ensure!(
        first.iter().any(|(_, requests)| *requests >= 1),
        "the client's request is not in any session: {first:?}"
    );
    stop(serve).await?;

    let (serve, graphql) = served(&home_dir).await?;
    kept(
        &first,
        &sessions(&home_dir, Some(&graphql))?,
        "across the restart",
    )?;
    prompted(&home_dir, "second start: answer the home's client again")?;
    let second = sessions(&home_dir, Some(&graphql))?;
    anyhow::ensure!(
        second.len() > first.len(),
        "the second start's request did not open a session: {second:?}"
    );
    stop(serve).await?;

    kept(
        &second,
        &sessions(&home_dir, None)?,
        "for a client opening the stopped home",
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_home_on_the_node_store_is_initialized_served_used_and_reopened() -> Result<()> {
    a_home_is_initialized_served_used_and_reopened("local").await
}

#[cfg(feature = "vera")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_home_on_an_embedded_vera_node_is_initialized_served_used_and_reopened() -> Result<()> {
    a_home_is_initialized_served_used_and_reopened("vera").await
}

#[cfg(feature = "vera")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_vera_home_serves_with_its_client_owned_device_group() -> Result<()> {
    let home = tempfile::tempdir()?;
    let model = MockChatEndpoint::start("device-group", REPLY)?;
    let init = run_init_json(
        home.path(),
        &[
            "--model-name",
            "device-group",
            "--inference-url",
            model.endpoint(),
            "--document-acp",
            "vera",
            "--create-transport-group",
        ],
    )?;
    let group = &init["transport_group"];
    let account = group["account"]
        .as_str()
        .context("missing device group owner")?;
    anyhow::ensure!(
        !group["group"].as_str().unwrap_or_default().is_empty(),
        "missing device group ID"
    );
    let (serve, _, readiness) = spawn_server_with_ready_json_recovering(
        home.path(),
        allocate_port()?,
        &["--transport-account", account],
        &[],
    )?;
    anyhow::ensure!(readiness["status"] == "serving", "{readiness}");
    stop(serve).await
}

/// The follower reopens with its own node key and the host's provisioned trust.
#[cfg(feature = "vera")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paired_home_reopens_against_the_original_vera_deployment() -> Result<()> {
    let host = tempfile::tempdir()?;
    let follower = tempfile::tempdir()?;
    let model = MockChatEndpoint::start("paired-home", REPLY)?;
    let port = allocate_port()?.to_string();
    let init_args = [
        "--model-name",
        "paired-home",
        "--inference-url",
        model.endpoint(),
        "--document-acp",
        "vera",
        "--vera-rpc-port",
        &port,
    ];
    let provisioned = run_init_json(host.path(), &init_args)?;
    let home = provisioned["home"].as_str().context("host home path")?;
    let data = gents_store::local::home::default_data_dir(Path::new(home));
    let genesis: Value =
        serde_json::from_slice(&fs::read(data.join("vera/node/solo-genesis.json"))?)?;
    let account = provisioned["transport_group"]["account"]
        .as_str()
        .context("transport group owner")?;
    anyhow::ensure!(
        provisioned["vera_anchor"]["account"].as_str() == Some(account),
        "follower must pin the account that owns the host's device group"
    );
    let device = node_did_from_init(&provisioned)?;
    let mut operators = [account, device.as_str()];
    operators.sort_unstable();
    anyhow::ensure!(
        genesis["operators"] == serde_json::json!({"threshold": 2, "keys": operators}),
        "shared genesis must require the account and the device"
    );
    let anchor = host.path().join("follower-anchor.json");
    fs::write(
        &anchor,
        serde_json::to_vec(
            provisioned
                .get("vera_anchor")
                .context("host must export its verified deployment identity")?,
        )?,
    )?;
    let (active, host_graphql) = served(host.path()).await?;
    let mut paired = init_args.to_vec();
    paired.extend([
        "--vera-anchor",
        anchor.to_str().context("temporary file path")?,
    ]);
    let unadmitted = tempfile::tempdir()?;
    let mut denied_args = vec!["init", "--store-key-custody", "file"];
    denied_args.extend_from_slice(&paired);
    let denied = run_cli_failure_stderr(unadmitted.path(), &denied_args)?;
    anyhow::ensure!(
        denied.contains("no certified group assignment"),
        "expected the shared account to refuse an unadmitted device: {denied}"
    );

    use gents_store::identity::{KeyIdentity, NodeIdentity};
    let key_path = follower.path().join("follower.key");
    let follower_key = KeyIdentity::load_or_create(&key_path, None)?;
    let follower_did = follower_key.did().parse()?;
    let group = provisioned["transport_group"]["group"]
        .as_str()
        .context("transport group document")?;
    let owner = gents_store::config_client::ConfigAccess::Graphql(
        crate::support::graphql::served_endpoint(&host_graphql),
    );
    anyhow::ensure!(owner.actor_did().as_deref() == Some(account));
    owner
        .add_actor_relationship(
            gents_protocol::schemas::ACCOUNT_DEVICE_GROUP_NAME,
            group,
            "member",
            &follower_did,
        )
        .await?;
    paired.extend([
        "--key-path",
        key_path.to_str().context("follower key path")?,
    ]);
    let initialized = run_init_json(follower.path(), &paired)?;
    anyhow::ensure!(
        node_did_from_init(&initialized)? != node_did_from_init(&provisioned)?,
        "the follower must retain its own signing identity"
    );
    fs::remove_file(&anchor)?;
    let (serving, graphql) = served(follower.path()).await?;
    prompted(follower.path(), "record a session on the paired device")?;
    let first = sessions(follower.path(), Some(&graphql))?;
    anyhow::ensure!(!first.is_empty(), "the paired device has no sessions");
    stop(serving).await?;
    stop(active).await?;
    let (active, host_graphql) = served(host.path()).await?;
    let (serving, graphql) = served(follower.path()).await?;
    kept(
        &first,
        &sessions(follower.path(), Some(&graphql))?,
        "after both devices restarted",
    )?;
    stop(serving).await?;
    let owner = gents_store::config_client::ConfigAccess::Graphql(
        crate::support::graphql::served_endpoint(&host_graphql),
    );
    owner
        .delete_actor_relationship(
            gents_protocol::schemas::ACCOUNT_DEVICE_GROUP_NAME,
            group,
            "member",
            &follower_did,
        )
        .await?;
    let denied = run_cli_failure_stderr(follower.path(), &["runtime", "server"])?;
    anyhow::ensure!(
        denied.contains("no certified group assignment"),
        "a revoked follower must refuse startup: {denied}"
    );
    stop(active).await
}
