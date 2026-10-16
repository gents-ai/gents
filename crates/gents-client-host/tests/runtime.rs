#![cfg(unix)]

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::identity::NodeIdentity;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use serde_json::{json, Value};
use tokio::process::{Child, Command};

#[allow(dead_code)]
#[path = "../../gents-cli/tests/support/mocks/fake_llm.rs"]
mod fake_llm;
#[allow(dead_code)]
#[path = "../../gents-cli/tests/support/ports.rs"]
mod ports;
mod support;

use fake_llm::{ChatAction, FakeLlm};
use support::Attachment;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_client_submits_signed_input_and_observes_live_and_durable_output() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = exercise(temp.path(), false).await;
    result.with_context(|| {
        ["runtime.log", "runtime.err", "client-host.log"]
            .map(|name| {
                format!(
                    "{name}:\n{}",
                    std::fs::read_to_string(temp.path().join(name)).unwrap_or_default()
                )
            })
            .join("\n")
    })
}

#[cfg(feature = "editor-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_preserves_live_session_and_draft_across_window_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), true).await.with_context(|| {
        std::fs::read_to_string(temp.path().join("editor.log")).unwrap_or_default()
    })
}

async fn exercise(root: &Path, editor: bool) -> Result<()> {
    let cli = Path::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .parent()
        .context("native binary directory")?
        .join("gents");
    ensure!(cli.is_file(), "build gents-cli before running runtime-e2e");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (emitted, _) = tokio::sync::watch::channel(None);
    let provider_gate = gate.clone();
    let provider = FakeLlm::start(
        "editor-fixture",
        None,
        Arc::new(move |_| {
            ChatAction::GatedSse(
                vec![(
                    Duration::ZERO,
                    format!(
                        "data: {}\n\n",
                        json!({"choices":[{"delta":{"content":"First part"},"finish_reason":null}],"usage":null})
                    ),
                )],
                provider_gate.clone(),
                fake_llm::completion_text_sse(" and final part."),
                emitted.clone(),
            )
        }),
    )?;
    let node_home = root.join("runtime");
    let init = Command::new(&cli)
        .current_dir(root)
        .args(["init", "--store-key-custody", "file", "--home"])
        .arg(&node_home)
        .args([
            "--node-name",
            "editor-fixture",
            "--model-name",
            "editor-fixture",
            "--inference-url",
        ])
        .arg(provider.endpoint())
        .kill_on_drop(true)
        .output();
    let init = tokio::time::timeout(Duration::from_secs(30), init)
        .await
        .context("initializing the fixture runtime timed out")??;
    ensure!(
        init.status.success(),
        "runtime initialization failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let init: Value = serde_json::from_slice(&init.stdout)?;
    let node_did = init["node_did"].as_str().context("initialized node DID")?;
    let identity = gents::KeyIdentity::load_or_create(
        init["key_path"]
            .as_str()
            .context("initialized identity path")?,
        None,
    )?;
    let port = ports::allocate_port()?;
    let mut runtime = Command::new(&cli)
        .current_dir(root)
        .env_remove("GENTS_SYSTEM_LOG")
        .args(["server", "--home"])
        .arg(&node_home)
        .arg("--http-port")
        .arg(port.to_string())
        .arg("--no-codex-shim")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(
            root.join("runtime.log"),
        )?))
        .stderr(Stdio::from(std::fs::File::create(
            root.join("runtime.err"),
        )?))
        .kill_on_drop(true)
        .spawn()?;
    let ready = wait_ready(&mut runtime, &root.join("runtime.log")).await?;
    ensure!(ready["readiness_status"] == "ready", "{ready}");
    let graphql = ready["graphql"]
        .as_str()
        .context("runtime GraphQL endpoint")?;
    let paths = DesktopPaths::from_root(root.join("client"));
    let seed = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await?,
    );
    let client_did = seed.node_identity().did().to_owned();
    seed.shutdown().await?;
    drop(seed);
    let mut host = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .args(["--serve", "--home"])
        .arg(paths.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(
            root.join("client-host.log"),
        )?))
        .kill_on_drop(true)
        .spawn()?;
    wait_listener(&mut host, &paths).await?;
    let mut client = Attachment::start(&paths).await;
    let access = gents::ConfigAccess::Graphql(gents::config_client::GraphqlEndpoint::as_principal(
        graphql,
        identity.did(),
    ));
    let pending = client
        .call(
            1,
            "desktop_peer_enroll_status",
            json!({
                "request": { "serverAddress": format!("http://127.0.0.1:{port}") }
            }),
        )
        .await;
    let pending_id = pending["requestId"]
        .as_str()
        .context("pending enrollment")?;
    let pending_query = format!(
        "{{ NetworkEnrollmentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ _docID }} }}",
        gents::graphql::escape_graphql_string(pending_id)
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = access.execute(&pending_query).await?;
            if response["data"]["NetworkEnrollmentRequest"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("runtime did not observe the enrollment request")??;
    let approved = Command::new(&cli)
        .current_dir(root)
        .args(["p2p", "enrollment", "approve", pending_id, "--home"])
        .arg(&node_home)
        .kill_on_drop(true)
        .output();
    let approved = tokio::time::timeout(Duration::from_secs(30), approved).await??;
    ensure!(
        approved.status.success(),
        "enrollment approval failed: {}",
        String::from_utf8_lossy(&approved.stderr)
    );
    let mut id = 2;
    let mut last_snapshot = Value::Null;
    let deployment = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let snapshot = client.snapshot(id).await;
            id += 1;
            last_snapshot = snapshot.clone();
            if let Some(deployment) = snapshot["client"]["deployments"]
                .as_array()
                .and_then(|values| values.iter().find(|value| value["nodeDid"] == node_did))
                .filter(|value| {
                    value["chatSafe"] == true
                        && value["nodeReadiness"]["source"]["state"] == "current"
                        && value["agents"].as_array().is_some_and(|agents| {
                            agents.iter().any(|agent| agent["isDefault"] == true)
                        })
                })
            {
                return deployment.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| {
        format!("client did not observe its authorized runtime route: {last_snapshot}")
    })?;
    let agent_id = deployment["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|agent| agent["isDefault"] == true))
        .and_then(|agent| agent["agentId"].as_str())
        .context("runtime default agent")?;
    let receipt = client.call(id, "desktop_chat_send", json!({"request":{
        "nodeDid":node_did,"agentId":agent_id,"content":"Return the fixture response.","delivery":"queue"
    }})).await;
    id += 1;
    let session = receipt["sessionId"].as_str().context("submitted session")?;
    let request = receipt["requestId"].as_str().context("submitted request")?;
    let signed_query = format!(
        "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
        gents::graphql::escape_graphql_string(request),
        gents::SIGNED_REQUEST_FIELDS,
    );
    let row: gents_protocol::row::AgentRequestRow =
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let response = access.execute(&signed_query).await?;
                if let Some(row) = response["data"]["AgentRequest"]
                    .as_array()
                    .and_then(|rows| rows.first())
                {
                    return serde_json::from_value(row.clone()).map_err(anyhow::Error::from);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("submitted request did not reach the runtime")??;
    gents::verify_request_receipt_signature(&row)?;
    ensure!(row.admission_signer_did.as_deref() == Some(client_did.as_str()));
    ensure!(client_did != node_did);
    ensure!(
        row.input
            .and_then(|input| input.queue)
            .context("signed delivery")?
            .delivery
            == gents_protocol::request_input::QueueDelivery::Queue
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let snapshot = client.snapshot(id).await;
            id += 1;
            if snapshot["client"]["deployments"]
                .as_array()
                .is_some_and(|nodes| {
                    nodes.iter().any(|node| {
                        node["nodeDid"] == node_did
                            && node["sessions"].as_array().is_some_and(|sessions| {
                                sessions.iter().any(|value| value["sessionId"] == session)
                            })
                    })
                })
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("the canonical session header was not observed")?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let projection = client
                .call(
                    id,
                    "desktop_session_snapshot",
                    json!({
                        "nodeDid":node_did,"sessionId":session,"timelineLimit":40
                    }),
                )
                .await;
            id += 1;
            let live = projection["timelineItems"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["kind"] == "liveAssistant"
                        && item["content"]
                            .as_str()
                            .is_some_and(|text| text.contains("First part"))
                })
            });
            if live {
                ensure!(
                    projection["turnState"] != "completed",
                    "stream bytes must precede completion"
                );
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("live output was not observed before provider completion")??;
    if editor {
        let extension_root = std::env::var_os("GENTS_VSCODE_ROOT")
            .context("Set GENTS_VSCODE_ROOT to the extension checkout before running editor-e2e")?;
        let extension_root = Path::new(&extension_root);
        ensure!(
            extension_root.is_absolute(),
            "GENTS_VSCODE_ROOT must be absolute"
        );
        let script = extension_root.join("tests/editor.mjs");
        ensure!(
            script.is_file(),
            "The extension checkout has no tests/editor.mjs"
        );
        let log = std::fs::File::create(root.join("editor.log"))?;
        let child = Command::new("node")
            .arg(script)
            .arg(root)
            .arg(paths.root())
            .arg(env!("CARGO_BIN_EXE_gents-client-host"))
            .arg(session)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .status();
        let status = tokio::time::timeout(Duration::from_secs(120), child)
            .await
            .context("editor journey timed out")??;
        ensure!(status.success(), "editor journey exited with {status}");
    }
    gate.add_permits(1);
    let mut last_projection = Value::Null;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let projection = client
                .call(
                    id,
                    "desktop_session_snapshot",
                    json!({
                        "nodeDid":node_did,"sessionId":session,"timelineLimit":40
                    }),
                )
                .await;
            id += 1;
            let durable = projection["timelineItems"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["kind"] == "assistantMessage"
                        && item["content"] == "First part and final part."
                })
            });
            if projection["turnState"] == "completed" && durable {
                return Ok::<_, anyhow::Error>(());
            }
            last_projection = projection;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| format!("canonical completed output was not observed: {last_projection}"))??;
    let requests = provider.captured_chat_requests();
    let user_rounds = requests
        .iter()
        .filter(|request| {
            request["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "user"
                        && match &message["content"] {
                            Value::String(text) => text == "Return the fixture response.",
                            Value::Array(parts) => parts
                                .iter()
                                .any(|part| part["text"] == "Return the fixture response."),
                            _ => false,
                        }
                })
            })
        })
        .count();
    ensure!(
        user_rounds == 1,
        "expected one submitted user round, observed {user_rounds}"
    );
    client.close().await;
    stop(&mut host).await?;
    stop(&mut runtime).await?;
    Ok(())
}

async fn wait_ready(child: &mut Child, path: &Path) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let text = tokio::fs::read_to_string(path).await?;
            for (index, _) in text.char_indices().filter(|(_, ch)| *ch == '{') {
                if let Some(Ok(value)) = serde_json::Deserializer::from_str(&text[index..])
                    .into_iter::<Value>()
                    .next()
                {
                    if value["status"] == "serving" {
                        return Ok(value);
                    }
                }
            }
            ensure!(
                child.try_wait()?.is_none(),
                "runtime exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("runtime readiness timed out")?
}

async fn wait_listener(child: &mut Child, paths: &DesktopPaths) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if gents_desktop_bridge::host::local::connect(paths)
                .await
                .is_ok()
            {
                return Ok(());
            }
            ensure!(
                child.try_wait()?.is_none(),
                "client host exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("native listener readiness timed out")?
}

async fn stop(child: &mut Child) -> Result<()> {
    let pid = child.id().context("live fixture child")?;
    // SAFETY: the PID belongs to the live child owned by this fixture.
    ensure!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) } == 0);
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    ensure!(status.success(), "fixture child exited with {status}");
    Ok(())
}
