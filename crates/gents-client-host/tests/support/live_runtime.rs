use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::support::live_inference::{live_target, wait_for_request_terminal, InferenceTarget};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_store::identity::NodeIdentity;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::super::{initialization::Initialization, ports, stop, support};
use support::Attachment;

const WAIT: Duration = Duration::from_secs(30);

pub async fn exercise(root: &Path, goal: bool) -> Result<()> {
    run(root, goal, false).await
}

#[cfg(feature = "editor-e2e")]
pub async fn exercise_editor(root: &Path) -> Result<()> {
    run(root, false, true).await
}

async fn run(root: &Path, goal: bool, editor: bool) -> Result<()> {
    let target = live_target().await;
    let root_owned = root.to_owned();
    let (binary, homes) = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>((
            support::native_binary("gents-client-host")?,
            support::homes::ProcessHomes::under(&root_owned),
        ))
    })
    .await??;
    let home = root.join("runtime");
    let mut command = Command::new(&binary);
    command
        .current_dir(root)
        .envs(homes.env())
        .env_remove("GENTS_SYSTEM_LOG")
        .args(["init", "--store-key-custody", "file", "--home"])
        .arg(&home)
        .args(["--tool-package", "readonly", "--tool-root"])
        .arg(root)
        .args([
            "--node-name",
            "client-live",
            "--model-name",
            target.model(),
            "--inference-url",
            target.endpoint(),
        ]);
    let root_owned = root.to_owned();
    let initialized =
        tokio::task::spawn_blocking(move || Initialization::spawn(&mut command, &root_owned))
            .await??;
    let initialized: Value = serde_json::from_slice(&initialized.wait(WAIT).await?)?;
    let node = initialized["node_did"]
        .as_str()
        .context("initialized node DID")?;
    let port = ports::allocate_port()?;
    let stderr = tokio::fs::File::create(root.join("runtime.err"))
        .await?
        .into_std()
        .await;
    let mut runtime = Command::new(&binary)
        .current_dir(root)
        .envs(homes.env())
        .env_remove("GENTS_SYSTEM_LOG")
        .args(["server", "--home"])
        .arg(&home)
        .arg("--http-port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()?;
    let mut lines = BufReader::new(runtime.stdout.take().context("runtime stdout")?).lines();
    let mut log = tokio::fs::File::create(root.join("runtime.log")).await?;
    let ready = tokio::time::timeout(WAIT, async {
        while let Some(line) = lines.next_line().await? {
            log.write_all(line.as_bytes()).await?;
            log.write_all(b"\n").await?;
            if let Some(start) = line.find('{') {
                if let Some(Ok(value)) = serde_json::Deserializer::from_str(&line[start..])
                    .into_iter::<Value>()
                    .next()
                {
                    if value["status"] == "serving" {
                        return Ok::<_, anyhow::Error>(value);
                    }
                }
            }
        }
        anyhow::bail!("runtime output ended before readiness")
    })
    .await
    .context("runtime readiness notification deadline")??;
    ensure!(
        ready["node_did"] == node,
        "runtime readiness belongs to the initialized node: {ready}"
    );
    let drain = tokio::spawn(async move {
        while let Some(line) = lines.next_line().await? {
            log.write_all(line.as_bytes()).await?;
            log.write_all(b"\n").await?;
        }
        Ok::<_, anyhow::Error>(())
    });
    let paths = DesktopPaths::from_root(root.join("client"));
    let seed = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await?,
    );
    let client_did = seed.node_identity().did().to_owned();
    seed.shutdown().await?;
    let operator_paths = homes.cli_identity();
    let operator = tokio::task::spawn_blocking(move || {
        gents_desktop_core::client::PrincipalIdentity::load_existing(&operator_paths)
    })
    .await??;
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(
            ready["graphql"]
                .as_str()
                .context("runtime GraphQL endpoint")?,
            operator.did(),
        )
        .with_client_signing_capability(operator.client_signing_capability()?)?,
    );
    let mut client = Attachment::start_with_binary(&paths, &binary).await;
    let mut updates = Attachment::start_with_binary(&paths, &binary).await;
    let result = async {
        client.try_call(1, "client_connect_local", json!({"nodeHome": home})).await?;
        client.try_call(2, "client_enroll_local", json!({"nodeHome": home})).await?;
        let mut id = 3;
        let deployment = wait_client(&mut client, &mut updates, &mut id, "desktop_client_snapshot", json!({}), |snapshot| {
            snapshot["client"]["deployments"].as_array().is_some_and(|rows| rows.iter().any(|row|
                row["nodeDid"] == node && row["chatSafe"] == true && row["agents"].as_array().is_some_and(|agents| agents.iter().any(|agent| agent["isDefault"] == true))))
        }).await?;
        let agent = deployment["client"]["deployments"].as_array().unwrap().iter()
            .find(|row| row["nodeDid"] == node).unwrap()["agents"].as_array().unwrap().iter()
            .find(|agent| agent["isDefault"] == true).unwrap()["agentDid"].as_str().context("default Agent")?.to_owned();
        configure(&mut client, &mut id, node, &agent, &target, goal).await?;
        let receipt = client.try_call(id, "desktop_chat_send", json!({"request": {
            "nodeDid": node, "agentDid": agent,
            "content": if goal { "Acknowledge this predecessor briefly. Do not call tools." } else { "Write 120 short numbered lines about trees. Keep each line to one short sentence and continue until the list is finished. Do not call tools." },
            "delivery": "queue",
        }})).await?;
        id += 1;
        let session = receipt["sessionId"].as_str().context("submitted session")?;
        let request = receipt["requestId"].as_str().context("submitted request")?;
        if !goal {
        let live = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({
            "nodeDid":node,"sessionId":session,"timelineLimit":40,
        }), |projection| {
            projection["turnState"] != "completed" && projection["timelineItems"].as_array().is_some_and(|items| items.iter().any(|item|
                item["kind"] == "liveAssistant" && item["content"].as_str().is_some_and(|text| !text.trim().is_empty())))
        }).await?;
        ensure!(live["sessionId"] == session, "live output crossed the selected session");
        }
        ensure!(wait_for_request_terminal(&access, request, WAIT).await == "completed");
        let mut projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({
            "nodeDid":node,"sessionId":session,"timelineLimit":40,
        }), |projection| projection["turnState"] == "completed" && projection["timelineItems"].as_array().is_some_and(|items|
            items.iter().any(|item| item["kind"] == "assistantMessage" && item["content"].as_str().is_some_and(|text| !text.trim().is_empty())))).await?;
        let output = client.try_call(id, "desktop_request_output", json!({"nodeDid":node,"sessionId":session,"requestId":request})).await?;
        id += 1;
        ensure!(output["nodeDid"] == node && output["sessionId"] == session && output["requestId"] == request, "request output crossed its scope: {output}");
        ensure!(output["output"]["kind"] == "terminalMessage" && output["output"]["content"].as_str().is_some_and(|text| !text.trim().is_empty()), "completed request has no visible terminal answer: {output}");
        let query = format!("{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{{}}} }}", gents_store::graphql::escape_graphql_string(request), gents::SIGNED_REQUEST_FIELDS);
        let stored = access.execute(&query).await?;
        let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(stored["data"]["AgentRequest"][0].clone())?;
        gents_store::identity::request_receipt::verify_request_receipt_signature(&row)?;
        ensure!(row.admission_signer_did.as_deref() == Some(client_did.as_str()));
        let wrong = client.try_call(id, "desktop_request_output", json!({"nodeDid":node,"sessionId":"another-session","requestId":request})).await;
        id += 1;
        ensure!(wrong.is_err() || wrong.is_ok_and(|value| value["output"]["kind"] == "loading"), "request output escaped its session");
        if goal {
            let author_paths = paths.clone();
            let author = tokio::task::spawn_blocking(move || gents_desktop_core::client::PrincipalIdentity::load_existing(&author_paths)).await??;
            let author_access = ConfigAccess::Graphql(GraphqlEndpoint::as_principal(ready["graphql"].as_str().context("runtime GraphQL endpoint")?, author.did()).with_client_signing_capability(author.client_signing_capability()?)?);
            let seeded = gents_automation::goal::seed_goal_from_access(&author_access, node, session,
                Some("Give one short completion sentence, then call update_goal with status complete."),
                Some(gents_protocol::goal_status::GoalStatus::Paused), Some(Some(16000)), None).await?;
            let view = wait_goal(&mut client, &mut updates, &mut id, node, session, "paused").await?;
            ensure!(view["goal"]["goalId"] == seeded.goal_id);
            let resumed = client.try_call(id, "desktop_goal_control", json!({"request": {
                "nodeDid":node, "sessionId":session, "sessionDocId":view["sessionDocId"],
                "expected":view["goal"]["revision"], "action":{"kind":"resume"}
            }})).await?;
            id += 1;
            let resumed_request = resumed["requestId"].as_str().context("resumed goal request")?;
            ensure!(resumed["goalId"] == seeded.goal_id && resumed_request != request);
            ensure!(wait_for_request_terminal(&access, resumed_request, WAIT).await == "completed");
            wait_goal(&mut client, &mut updates, &mut id, node, session, "complete").await?;
            projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["latestRequestId"] == resumed_request && value["turnState"] == "completed").await?;
        }
        if editor {
            super::super::run_editor_journey(root, &paths, &binary, session, target.endpoint(), "live").await?;
            let evidence: Value = serde_json::from_slice(&tokio::fs::read(root.join("editor-live-canonical.json")).await?)?;
            ensure!(evidence["sessionId"] == session);
            let prompt = evidence["prompt"].as_str().context("editor submitted prompt")?;
            let query = format!("{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{{}}} }}", gents_store::graphql::escape_graphql_string(node), gents_store::graphql::escape_graphql_string(session), gents::SIGNED_REQUEST_FIELDS);
            let observed = access.execute(&query).await?;
            let rows: Vec<gents_protocol::row::AgentRequestRow> = serde_json::from_value(observed["data"]["AgentRequest"].clone())?;
            let submitted = rows.iter().find(|row| row.content.as_deref() == Some(prompt)).context("editor's signed submitted input")?;
            gents_store::identity::request_receipt::verify_request_receipt_signature(submitted)?;
            ensure!(submitted.admission_signer_did.as_deref() == Some(client_did.as_str()));
            ensure!(rows.iter().all(|row| row.content.as_deref() != evidence["draft"].as_str()), "the retained editor draft was submitted");
            ensure!(wait_for_request_terminal(&access, &submitted.request_id, WAIT).await == "completed");
            projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["latestRequestId"] == submitted.request_id && value["turnState"] == "completed").await?;
        }
        Ok::<_, anyhow::Error>((session.to_owned(), projection))
    }.await;
    updates.close().await;
    let retired = client.retire().await;
    let result = match result {
        Ok((session, projection)) => {
            retired?;
            let mut restarted = Attachment::start_with_binary(&paths, &binary).await;
            let mut restarted_updates = Attachment::start_with_binary(&paths, &binary).await;
            let mut id = 1;
            let retained = async {
                let retained = wait_client(
                    &mut restarted,
                    &mut restarted_updates,
                    &mut id,
                    "desktop_session_snapshot",
                    json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}),
                    |value| value["turnState"] == "completed",
                )
                .await?;
                ensure!(
                    retained["timelineItems"] == projection["timelineItems"],
                    "native client restart changed the durable transcript"
                );
                if goal {
                    wait_goal(
                        &mut restarted,
                        &mut restarted_updates,
                        &mut id,
                        node,
                        &session,
                        "complete",
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            restarted_updates.close().await;
            restarted.retire().await?;
            retained
        }
        Err(error) => {
            let _ = retired;
            Err(error)
        }
    };
    stop(&mut runtime).await?;
    drain.await??;
    result
}

async fn configure(
    client: &mut Attachment,
    id: &mut u32,
    node: &str,
    agent: &str,
    target: &InferenceTarget,
    goal: bool,
) -> Result<()> {
    let setup = client
        .try_call(*id, "client_inference_setup_state", json!({"nodeDid":node}))
        .await?;
    *id += 1;
    let mut selected = setup["agentConfigs"]
        .as_array()
        .and_then(|agents| agents.iter().find(|value| value["agent_did"] == agent))
        .context("selected Agent configuration")?
        .clone();
    if goal {
        selected["context_id"] = json!("native-live-goal-context");
    }
    let profile = target.profile(node);
    selected["inference_profile_id"] = json!(profile.profile_id);
    let mut document = json!({"node":{"node_did":node},"agents":[selected],"inference_backends":[target.backend(node)],"inference_profiles":[profile]});
    if goal {
        document["contexts"] = json!([{"node_did":node,"context_id":"native-live-goal-context","tools_id":"native-live-goal-tools","system_prompt":"Answer ordinary requests briefly. When the Goal is resumed, fulfill its objective and call update_goal with status complete. Do not create another Goal."}]);
        document["tools"] = json!([{"node_did":node,"tools_id":"native-live-goal-tools","built_ins":{"enable_goal_tools":true}}]);
    }
    if let Some(sampling) = target.sampling(node) {
        document["inference_sampling"] = json!([sampling]);
    }
    client
        .try_call(
            *id,
            "client_config_apply",
            json!({"request":{"document":document}}),
        )
        .await?;
    *id += 1;
    Ok(())
}

async fn wait_client(
    client: &mut Attachment,
    updates: &mut Attachment,
    id: &mut u32,
    command: &str,
    args: Value,
    ready: impl Fn(&Value) -> bool,
) -> Result<Value> {
    tokio::time::timeout(WAIT, async {
        loop {
            match client.try_call(*id, command, args.clone()).await {
                Ok(value) if ready(&value) => {
                    *id += 1;
                    return Ok(value);
                }
                Ok(_) => {}
                Err(error)
                    if error.code == gents_desktop_bridge::BridgeErrorCode::ProjectionPending => {}
                Err(error) => return Err(anyhow::Error::from(error)),
            }
            *id += 1;
            while updates.next().await["kind"] != "updated" {}
        }
    })
    .await
    .with_context(|| format!("native {command} observation deadline"))?
}

async fn wait_goal(
    client: &mut Attachment,
    updates: &mut Attachment,
    id: &mut u32,
    node: &str,
    session: &str,
    state: &str,
) -> Result<Value> {
    let value = wait_client(
        client,
        updates,
        id,
        "desktop_session_goal",
        json!({"nodeDid":node,"sessionId":session}),
        |value| value["kind"] == "observed" && value["observation"]["goal"]["status"] == state,
    )
    .await?;
    Ok(value["observation"].clone())
}
