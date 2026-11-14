use std::path::Path;
#[cfg(unix)]
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::support::live_inference::{live_target, wait_for_request_terminal, InferenceTarget};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_store::identity::NodeIdentity;
use serde_json::{json, Value};
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::{initialization::Initialization, ports, stop, support};
use support::Attachment;

#[path = "live_delivery.rs"]
mod live_delivery;
#[path = "live_faults.rs"]
mod live_faults;
#[path = "live_file_actions.rs"]
mod live_file_actions;
#[path = "live_gate.rs"]
mod live_gate;
#[path = "live_goals.rs"]
mod live_goals;
#[path = "live_inventory.rs"]
mod live_inventory;
#[path = "live_questions.rs"]
mod live_questions;
#[path = "live_related_work.rs"]
mod live_related_work;
#[path = "live_steering.rs"]
mod live_steering;
#[path = "live_undo.rs"]
mod live_undo;

const WAIT: Duration = Duration::from_secs(30);

impl Attachment {
    /// Retirement is acknowledged before the serving installation has closed
    /// its store, so the next installation opens only after the store owner
    /// releases it.
    async fn retire(mut self, paths: &DesktopPaths) -> Result<()> {
        let build = self
            .installation_build
            .as_ref()
            .context("native installation does not support retirement")?;
        self.send(json!({"kind":"retire", "id":u32::MAX, "expectedBuild":build}))
            .await;
        loop {
            let response = self.next().await;
            if response["kind"] == "response" {
                ensure!(
                    response["id"] == u32::MAX && response["error"].is_null(),
                    "native retirement failed: {response}"
                );
                break;
            }
        }
        self.wait_for_output_eof().await;
        ensure!(
            self.wait_for_exit().await.success(),
            "retired attachment failed"
        );
        tokio::time::timeout(
            WAIT,
            gents_store::local::home::wait_for_store_release(paths.root(), paths.node_data_dir()),
        )
        .await
        .context("the retired installation did not release its store")??;
        Ok(())
    }
}

pub async fn exercise(root: &Path, goal: bool) -> Result<()> {
    run(root, goal, None, false).await
}

#[cfg(feature = "editor-e2e")]
pub async fn exercise_editor(root: &Path, scenario: &str) -> Result<()> {
    run(root, false, Some(scenario), false).await
}

pub async fn exercise_replacement(root: &Path) -> Result<()> {
    run(root, false, None, true).await
}

async fn run(root: &Path, goal: bool, editor: Option<&str>, replacement: bool) -> Result<()> {
    let target = live_target().await;
    let fault = if editor == Some("retry") {
        Some(live_faults::FaultRelay::start(&target, super::editor_retry::PROMPT.to_owned(),
            axum::http::StatusCode::BAD_REQUEST,
            json!({"error":{"message":"Retry fixture provider refusal","type":"invalid_request_error"}})).await?)
    } else {
        None
    };
    let mut gates = live_goals::gates(root, editor).await?;
    if replacement {
        gates.push(live_gate::HeldTool::start(root).await?);
    }
    let steering = matches!(editor, Some("live-steering" | "live-goal-steering"));
    let goal_steering = editor == Some("live-goal-steering");
    let root_owned = root.to_owned();
    let (binary, homes) = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>((
            support::native_binary("gents-client-host")?,
            support::homes::ProcessHomes::under(&root_owned),
        ))
    })
    .await??;
    let runtime_binary = if replacement {
        tokio::task::spawn_blocking(|| support::native_binary("gents")).await??
    } else {
        binary.clone()
    };
    let home = root.join("runtime");
    let mut command = Command::new(&runtime_binary);
    command
        .current_dir(root)
        .envs(homes.env())
        .env_remove("GENTS_SYSTEM_LOG")
        .args(["init", "--store-key-custody", "file", "--home"])
        .arg(&home)
        .args([
            "--tool-package",
            if matches!(
                editor,
                Some("live-review" | "live-review-inventory" | "live-undo")
            ) {
                "write"
            } else {
                "readonly"
            },
            "--tool-root",
        ])
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
    let mut server = Command::new(&runtime_binary);
    server
        .current_dir(root)
        .envs(homes.env())
        .env_remove("GENTS_SYSTEM_LOG")
        .args(["server", "--home"])
        .arg(&home)
        .arg("--http-port")
        .arg(port.to_string());
    #[cfg(unix)]
    let (mut runtime, ready, drain) = {
        let stderr = tokio::fs::File::create(root.join("runtime.err"))
            .await?
            .into_std()
            .await;
        let mut runtime = server
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()?;
        let mut lines = BufReader::new(runtime.stdout.take().context("runtime stdout")?).lines();
        let mut log = tokio::fs::File::create(root.join("runtime.log")).await?;
        let ready = tokio::time::timeout(WAIT, async {
            let mut output = String::new();
            while let Some(line) = lines.next_line().await? {
                log.write_all(line.as_bytes()).await?;
                log.write_all(b"\n").await?;
                output.push_str(&line);
                output.push('\n');
                if let Some(start) = output.find('{') {
                    if let Some(Ok(value)) = serde_json::Deserializer::from_str(&output[start..])
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
        let drain = tokio::spawn(async move {
            while let Some(line) = lines.next_line().await? {
                log.write_all(line.as_bytes()).await?;
                log.write_all(b"\n").await?;
            }
            Ok::<_, anyhow::Error>(())
        });
        (runtime, ready, drain)
    };
    // Ctrl-C reaches a Windows runtime only through its own console, which
    // owns and drains the output; readiness is read from the served status.
    #[cfg(windows)]
    let (mut runtime, ready, drain) = {
        let mut runtime =
            super::console::ConsoleChild::spawn(server.as_std(), &root.join("runtime.log"))?;
        let ready = super::wait_for_status(&mut runtime, &runtime_binary, &home, port).await?;
        (
            runtime,
            ready,
            tokio::spawn(async { Ok::<_, anyhow::Error>(()) }),
        )
    };
    ensure!(
        ready["node_did"] == node,
        "runtime readiness belongs to the initialized node: {ready}"
    );
    let paths = DesktopPaths::from_root(root.join("client"));
    let seed = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await?,
    );
    let client_did = seed.node_identity().did().to_owned();
    seed.shutdown().await?;
    drop(seed);
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
    let questions = if editor == Some("live-questions") {
        let ConfigAccess::Graphql(endpoint) = &access else {
            anyhow::bail!("native questions require a served runtime");
        };
        Some(live_questions::Questions::prepare(root, endpoint, node, gates[0].descriptor()).await?)
    } else {
        None
    };
    let related_work = if editor == Some("live-related-work") {
        let ConfigAccess::Graphql(endpoint) = &access else {
            anyhow::bail!("native related work requires a served runtime");
        };
        Some(
            live_related_work::RelatedWork::prepare(
                root,
                endpoint,
                node,
                [gates[0].descriptor(), gates[1].descriptor()],
            )
            .await?,
        )
    } else {
        None
    };
    let inventory = if editor == Some("live-review-inventory") {
        Some(live_inventory::Inventory::prepare(root).await?)
    } else {
        None
    };
    let undo = if editor == Some("live-undo") {
        Some(live_undo::Undo::prepare(root, gates[0].descriptor()).await?)
    } else {
        None
    };
    let mut client = Attachment::start_with_binary(&paths, &binary).await;
    let mut updates = Attachment::start_with_binary(&paths, &binary).await;
    let mut replaced = None;
    let result = async {
        client.try_call(1, "client_connect_local", json!({"nodeHome": home})).await?;
        client.try_call(2, "client_enroll_local", json!({"nodeHome": home})).await?;
        let author_paths = paths.clone();
        let author = tokio::task::spawn_blocking(move || gents_desktop_core::client::PrincipalIdentity::load_existing(&author_paths)).await??;
        ensure!(author.did() == client_did);
        let access = ConfigAccess::Graphql(GraphqlEndpoint::as_principal(ready["graphql"].as_str().context("runtime GraphQL endpoint")?, author.did()).with_client_signing_capability(author.client_signing_capability()?)?);
        let mut id = 3;
        let deployment = wait_client(&mut client, &mut updates, &mut id, "desktop_client_snapshot", json!({}), |snapshot| {
            snapshot["client"]["deployments"].as_array().is_some_and(|rows| rows.iter().any(|row|
                row["nodeDid"] == node && row["chatSafe"] == true && row["agents"].as_array().is_some_and(|agents| agents.iter().any(|agent| agent["isDefault"] == true))))
        }).await?;
        let agent = deployment["client"]["deployments"].as_array().unwrap().iter()
            .find(|row| row["nodeDid"] == node).unwrap()["agents"].as_array().unwrap().iter()
            .find(|agent| agent["isDefault"] == true).unwrap()["agentDid"].as_str().context("default Agent")?.to_owned();
        let configured_from = active_generation(&deployment, node).context("the serving runtime reports its active generation")?;
        configure(&mut client, &mut id, node, &agent, &target, goal, &gates, editor, questions.as_ref(), related_work.as_ref(), inventory.as_ref(), undo.as_ref(), fault.as_ref(), replacement).await?;
        if goal || !gates.is_empty() {
            wait_configuration_active(&mut client, &mut updates, &mut id, node, configured_from).await?;
        }
        let opening = if steering || replacement { gates[0].prompt() } else if goal || editor.is_some() { "Acknowledge this session briefly. Do not call tools.".to_owned() } else { "Write 120 short numbered lines about trees. Keep each line to one short sentence and continue until the list is finished. Do not call tools.".to_owned() };
        let receipt = if goal_steering {
            client.try_call(id, "desktop_goal_control", json!({"request":{"nodeDid":node,"agentDid":agent,"action":{"kind":"create","objective":opening,"tokenBudget":"16000"}}})).await?
        } else {
            client.try_call(id, "desktop_chat_send", json!({"request": {
                "nodeDid": node, "agentDid": agent, "content": opening, "delivery": "queue",
            }})).await?
        };
        id += 1;
        let session = receipt["sessionId"].as_str().context("submitted session")?;
        let request = receipt["requestId"].as_str().context("submitted request")?;
        if replacement {
            gates[0].wait_started(WAIT).await?;
            super::runtime_replacement::refuse_busy(&binary, &home).await?;
            gates[0].release().await?;
        }
        if steering {
            gates[0].wait_started(WAIT).await?;
            let mut fixture = json!({"gate":gates[0].descriptor()});
            if goal_steering {
                fixture["continuationGate"] = gates[1].descriptor();
            }
            tokio::fs::write(root.join("editor-steering-fixture.json"), serde_json::to_vec(&fixture)?).await?;
            super::run_editor_journey(root, &paths, &binary, session, target.endpoint(), editor.unwrap()).await?;
        }
        if !goal && editor.is_none() && !replacement {
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
        if replacement {
            super::runtime_replacement::Fixture {
                root, home:&home, binary:&binary, node_did:node, agent_did:&agent,
                session, client_did:&client_did, original_doc_id:row.doc_id.as_deref().context("original request document")?, access:&access,
            }.replace_and_verify(&mut runtime, &mut client, &mut updates, &mut id).await.map(|serving| replaced = Some(serving))?;
            projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed").await?;
        }
        if goal {
            let seeded = gents_automation::goal::seed_goal_from_access(&access, node, session,
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
        if steering {
            live_steering::verify(&access, root, node, session, request, &client_did, receipt["goalId"].as_str()).await?;
        }
        if let Some(scenario) = editor.filter(|_| !steering) {
            live_goals::prepare(root, scenario, &gates).await?;
            if scenario == "retry" {
                tokio::fs::write(root.join("editor-retry-fixture.json"), serde_json::to_vec(&json!({"prompt":super::editor_retry::PROMPT,"captured":super::editor_retry::CAPTURED}))?).await?;
            }
            if let Some(inventory) = &inventory {
                for prompt in &inventory.seed_prompts {
                    let sent = client.try_call(id, "desktop_chat_send", json!({"request":{"nodeDid":node,"agentDid":agent,"sessionId":session,"content":prompt,"delivery":"queue"}})).await?;
                    id += 1;
                    let request = sent["requestId"].as_str().context("inventory request receipt")?;
                    ensure!(wait_for_request_terminal(&access, request, WAIT).await == "completed");
                    projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["latestRequestId"] == request && value["turnState"] == "completed").await?;
                }
            }
            if scenario == "live-review" {
                live_file_actions::prepare_review(root).await?;
            }
            if scenario == "live-history" {
                let prompts = (1..=45).map(|turn| format!("History turn {turn}: give one short sentence about a leaf. Do not call tools.")).collect::<Vec<_>>();
                for prompt in &prompts {
                    let sent = client.try_call(id, "desktop_chat_send", json!({"request":{"nodeDid":node,"agentDid":agent,"sessionId":session,"content":prompt,"delivery":"queue"}})).await?;
                    id += 1;
                    ensure!(sent["sessionId"] == session);
                    let request = sent["requestId"].as_str().context("history request receipt")?;
                    ensure!(wait_for_request_terminal(&access, request, WAIT).await == "completed");
                    projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["latestRequestId"] == request && value["turnState"] == "completed").await?;
                }
                tokio::fs::write(root.join("editor-live-history-fixture.json"), serde_json::to_vec(&json!({"initialPrompt":opening,"latestPrompt":prompts.last(),"turnPrompts":prompts}))?).await?;
            }
            super::run_editor_journey(root, &paths, &binary, session, target.endpoint(), scenario).await?;
            if let Some(fault) = &fault {
                fault.wait_injected(WAIT).await?;
                let evidence = super::editor_retry::verify(&mut client, &mut id, &access, root, node, session, &client_did).await?;
                tokio::fs::write(root.join("editor-retry-canonical.json"), serde_json::to_vec(&evidence)?).await?;
                projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed").await?;
            } else if let Some(undo) = &undo {
                let evidence = undo.verify(&mut client, &mut id, &access, node, &client_did).await?;
                tokio::fs::write(root.join("editor-undo-canonical.json"), serde_json::to_vec(&evidence)?).await?;
            } else if let Some(inventory) = &inventory {
                let evidence = inventory.verify(&mut client, &mut id, &access, node, session, &client_did).await?;
                tokio::fs::write(root.join("editor-inventory-canonical.json"), serde_json::to_vec(&evidence)?).await?;
                projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed").await?;
            } else if let Some(related_work) = &related_work {
                let evidence = related_work.verify(&access, root, node, session, &client_did).await?;
                tokio::fs::write(root.join("editor-related-work-canonical.json"), serde_json::to_vec(&evidence)?).await?;
                projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed").await?;
            } else if let Some(questions) = &questions {
                let evidence = questions.verify(&access, root, node, session, &client_did).await?;
                tokio::fs::write(root.join("editor-questions-canonical.json"), serde_json::to_vec(&evidence)?).await?;
                projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed" || value["turnState"] == "interrupted").await?;
            } else if matches!(scenario, "live-goal-removal" | "live-goal-replacement" | "live-goal-delivery") {
                live_goals::verify(&access, root, node, &client_did, scenario).await?;
                projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed").await?;
            } else if scenario == "live-review" {
                let evidence = live_file_actions::verify_review(&mut client, &mut id, &access, root, node, session, &client_did).await?;
                tokio::fs::write(root.join("editor-review-canonical.json"), serde_json::to_vec(&evidence)?).await?;
                projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed").await?;
            } else if scenario == "live-delivery" {
                live_delivery::verify(&access, root, node, session, &client_did, &opening).await?;
                projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed").await?;
            } else if scenario == "live-workspace" {
                let evidence = super::editor_workspace::verify(&access, root, node, session, &client_did).await?;
                tokio::fs::write(root.join("editor-workspace-canonical.json"), serde_json::to_vec(&evidence)?).await?;
                projection = wait_client(&mut client, &mut updates, &mut id, "desktop_session_snapshot", json!({"nodeDid":node,"sessionId":session,"timelineLimit":40}), |value| value["turnState"] == "completed").await?;
            } else if scenario == "live-history" {
                let ui: Value = serde_json::from_slice(&tokio::fs::read(root.join("editor-history-ui.json")).await?)?;
                let archive: Value = serde_json::from_slice(&tokio::fs::read(root.join("editor-archive-ui.json")).await?)?;
                ensure!(ui["sessionId"] == session && ui["allInputs"] == 45 && ui["draftPreservedAfterReload"] == true);
                tokio::fs::write(root.join("editor-history-canonical.json"), serde_json::to_vec(&json!({"ui":ui,"archive":archive,"sessionId":session,"completedLiveTurns":45}))?).await?;
            } else {
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
        }
        Ok::<_, anyhow::Error>((session.to_owned(), projection))
    }.await;
    updates.close().await;
    let retired = client.retire(&paths).await;
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
                    |value| value["turnState"] == projection["turnState"],
                )
                .await?;
                ensure!(
                    retained["timelineItems"] == projection["timelineItems"],
                    "native client restart changed the durable transcript: before={}; after={}",
                    projection["timelineItems"],
                    retained["timelineItems"]
                );
                if goal || goal_steering {
                    wait_goal(
                        &mut restarted,
                        &mut restarted_updates,
                        &mut id,
                        node,
                        &session,
                        if goal_steering { "paused" } else { "complete" },
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            restarted_updates.close().await;
            restarted.retire(&paths).await?;
            retained
        }
        Err(error) => {
            let _ = retired;
            Err(error)
        }
    };
    // Cleanup runs on every path; the journey's own error is reported first.
    let mut released = Ok(());
    for gate in &mut gates {
        let outcome = gate.release().await;
        if released.is_ok() {
            released = outcome;
        }
    }
    let stopped = match runtime.try_wait() {
        Ok(None) => stop(&mut runtime).await,
        Ok(Some(_)) => Ok(()),
        Err(error) => Err(error.into()),
    };
    let stopped = match replaced {
        Some(replacement) => stopped.and(replacement.retire().await),
        None => stopped,
    };
    let drained = tokio::time::timeout(WAIT, drain)
        .await
        .context("runtime output cleanup deadline")
        .and_then(|joined| joined?);
    result.and(released).and(stopped).and(drained)
}

#[expect(
    clippy::too_many_arguments,
    reason = "each live journey adds its own optional configuration fixture"
)]
async fn configure(
    client: &mut Attachment,
    id: &mut u32,
    node: &str,
    agent: &str,
    target: &InferenceTarget,
    goal: bool,
    gates: &[live_gate::HeldTool],
    editor: Option<&str>,
    questions: Option<&live_questions::Questions>,
    related_work: Option<&live_related_work::RelatedWork>,
    inventory: Option<&live_inventory::Inventory>,
    undo: Option<&live_undo::Undo>,
    fault: Option<&live_faults::FaultRelay>,
    replacement: bool,
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
    let mut profile = target.profile(node);
    if replacement {
        profile.execution_id = Some("native-live-replacement-budget".to_owned());
    }
    selected["inference_profile_id"] = json!(profile.profile_id);
    let mut backend = target.backend(node);
    if let Some(fault) = fault {
        backend.endpoint = fault.endpoint().to_owned();
    }
    let mut document = json!({"node":{"node_did":node},"agents":[selected],"inference_backends":[backend],"inference_profiles":[profile]});
    if replacement {
        document["inference_execution"] =
            serde_json::to_value([gents_documents::InferenceExecution {
                node_did: node.to_owned(),
                execution_id: "native-live-replacement-budget".to_owned(),
                max_total_tokens: Some(8192),
                ..Default::default()
            }])?;
    }
    if goal {
        document["contexts"] = json!([{"node_did":node,"context_id":"native-live-goal-context","tools_id":"native-live-goal-tools","system_prompt":"Answer ordinary requests briefly. When the Goal is resumed, fulfill its objective and call update_goal with status complete. Do not create another Goal."}]);
        document["tools"] = json!([{"node_did":node,"tools_id":"native-live-goal-tools","built_ins":{"enable_goal_tools":true}}]);
    }
    if let Some(sampling) = target.sampling(node) {
        document["inference_sampling"] = json!([sampling]);
    }
    for gate in gates {
        gate.configure(&mut document, node);
    }
    if !gates.is_empty() {
        let instruction = if editor == Some("live-goal-steering") {
            "For the opening Goal request, run its given held command. Do not complete the Goal. For the first later automatic Goal continuation, run the continuation command appended below and acknowledge its completion without changing Goal status."
        } else if editor == Some("live-steering") {
            "Run the held tool from the opening request. If the user steers with additional input while it runs, acknowledge the additional input after the tool completes."
        } else if editor == Some("live-goal-delivery") {
            "For a Goal opening, run its held tool and then answer without completing the Goal. On a resumed Goal continuation after the opening completes, call update_goal with status complete."
        } else {
            "For a Goal opening, run its held tool and then answer without changing the Goal status. If the current user input is Complete the second goal from this follow-up, call update_goal with status complete. For other ordinary messages, answer briefly without changing the Goal."
        };
        for context in document["contexts"]
            .as_array_mut()
            .context("held tool contexts")?
        {
            let previous = context["system_prompt"].as_str().unwrap_or_default();
            let continuation = if editor == Some("live-goal-steering") {
                gates[1].prompt()
            } else {
                String::new()
            };
            context["system_prompt"] = json!(format!("{previous}\n{instruction}\n{continuation}"));
        }
    }
    if let Some(questions) = questions {
        questions.configure(&mut document, node)?;
    }
    if let Some(related_work) = related_work {
        related_work.configure(&mut document, node)?;
    }
    if let Some(inventory) = inventory {
        inventory.configure(&mut document, node)?;
    }
    if let Some(undo) = undo {
        undo.configure(&mut document, node)?;
    }
    // Decoded through the contract type, so a stale fixture field fails here
    // with the field's name rather than as the runtime's refusal.
    let document: gents_documents::PackConfig = serde_json::from_value(document)
        .context("live fixture configuration differs from the PackConfig contract")?;
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

fn active_generation(snapshot: &Value, node: &str) -> Option<u64> {
    snapshot["client"]["deployments"]
        .as_array()?
        .iter()
        .find(|row| row["nodeDid"] == node)?["nodeReadiness"]["activeGeneration"]
        .as_u64()
}

/// The runtime reconciles a committed configuration after its control
/// debounce, and router admission checks only that a generation's selected
/// documents are still visible, so input submitted before the next generation
/// is active runs with the previous tool surface. These journeys change their
/// Agent's resolved configuration, so the next generation always differs.
async fn wait_configuration_active(
    client: &mut Attachment,
    updates: &mut Attachment,
    id: &mut u32,
    node: &str,
    before: u64,
) -> Result<()> {
    wait_client(
        client,
        updates,
        id,
        "desktop_client_snapshot",
        json!({}),
        |snapshot| {
            let Some(row) = snapshot["client"]["deployments"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["nodeDid"] == node))
            else {
                return false;
            };
            let readiness = &row["nodeReadiness"];
            readiness["activeGeneration"]
                .as_u64()
                .is_some_and(|active| active > before)
                && readiness["routerGeneration"] == readiness["activeGeneration"]
                && row["runtime"]["reconcilePhase"] == "idle"
                && row["chatSafe"] == true
        },
    )
    .await
    .context("the runtime did not activate the applied configuration")?;
    Ok(())
}

pub(super) async fn wait_client(
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
