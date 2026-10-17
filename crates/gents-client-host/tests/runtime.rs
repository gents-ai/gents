#![cfg(any(unix, windows))]

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::identity::NodeIdentity;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use serde_json::{json, Value};
#[cfg(unix)]
use tokio::process::Child;
use tokio::process::Command;

#[path = "support/console.rs"]
#[cfg(windows)]
mod console;
#[cfg(windows)]
use console::ConsoleChild as Child;

#[path = "support/configured_actions.rs"]
mod configured_actions;
#[allow(dead_code)]
#[path = "../../gents-cli/tests/support/mocks/fake_llm.rs"]
mod fake_llm;
#[allow(dead_code)]
#[path = "../../gents-cli/tests/support/ports.rs"]
mod ports;
#[path = "support/speech.rs"]
mod speech;
mod support;

use fake_llm::{ChatAction, FakeLlm};
use support::Attachment;

#[derive(Clone, Copy)]
enum Journey {
    Conversation,
    #[cfg_attr(not(feature = "editor-e2e"), allow(dead_code))]
    Editor,
    Goal,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_client_submits_signed_input_and_observes_live_and_durable_output() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = exercise(temp.path(), Journey::Conversation).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_client_starts_pauses_and_resumes_the_current_goal_turn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    exercise(temp.path(), Journey::Goal).await.with_context(|| {
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
    exercise(temp.path(), Journey::Editor)
        .await
        .with_context(|| {
            std::fs::read_to_string(temp.path().join("editor.log")).unwrap_or_default()
        })
}

async fn exercise(root: &Path, journey: Journey) -> Result<()> {
    let cli = Path::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .parent()
        .context("native binary directory")?
        .join(format!("gents{}", std::env::consts::EXE_SUFFIX));
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
    let mut runtime_command = Command::new(&cli);
    runtime_command
        .current_dir(root)
        .env_remove("GENTS_SYSTEM_LOG")
        .args(["server", "--home"])
        .arg(&node_home)
        .arg("--http-port")
        .arg(port.to_string())
        .arg("--no-codex-shim");
    #[cfg(unix)]
    let mut runtime = runtime_command
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(
            root.join("runtime.log"),
        )?))
        .stderr(Stdio::from(std::fs::File::create(
            root.join("runtime.err"),
        )?))
        .kill_on_drop(true)
        .spawn()?;
    #[cfg(windows)]
    let mut runtime =
        console::ConsoleChild::spawn(runtime_command.as_std(), &root.join("runtime.log"))?;
    #[cfg(unix)]
    let ready = wait_ready(&mut runtime, &root.join("runtime.log")).await?;
    let status = wait_for_status(&mut runtime, &cli, &node_home, port).await?;
    #[cfg(unix)]
    ensure!(
        status["graphql"] == ready["graphql"] && status["node_did"] == ready["node_did"],
        "runtime readiness output and status command must identify the same runtime"
    );
    #[cfg(windows)]
    let ready = status;
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
    let mut host_command = Command::new(env!("CARGO_BIN_EXE_gents-client-host"));
    host_command.args(["--serve", "--home"]).arg(paths.root());
    #[cfg(unix)]
    let mut host = host_command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(
            root.join("client-host.log"),
        )?))
        .kill_on_drop(true)
        .spawn()?;
    #[cfg(windows)]
    let mut host =
        console::ConsoleChild::spawn(host_command.as_std(), &root.join("client-host.log"))?;
    wait_listener(&mut host, &paths).await?;
    let mut client = Attachment::start(&paths).await;
    let access = gents::ConfigAccess::Graphql(gents::config_client::GraphqlEndpoint::as_principal(
        graphql,
        identity.did(),
    ));
    if matches!(journey, Journey::Conversation) {
        client
            .call(1, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
    } else {
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
    }
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
    if matches!(journey, Journey::Conversation) {
        configure_inference(
            &mut client,
            &mut id,
            node_did,
            agent_id,
            &deployment,
            provider.endpoint(),
        )
        .await?;
    }
    let speech = if matches!(journey, Journey::Conversation) {
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        id += 1;
        let service = speech::SpeechService::start().await?;
        access
            .write(
                "seed_editor_speech_service",
                &format!(
                    r#"mutation {{ create_ToolServiceRegistry(input: {{
                node_did:"{}", service_id:"editor-speech", display_name:"Editor speech",
                hostname:"127.0.0.1", mcp_port:{}, mcp_path:"/mcp", enabled:true, send_node_did:true
            }}) {{ _docID }} }}"#,
                    gents::graphql::escape_graphql_string(node_did),
                    service.port,
                ),
            )
            .await?;
        let services = client
            .call(id, "client_speech_services", json!({"nodeDid":node_did}))
            .await;
        id += 1;
        ensure!(
            services.as_array().is_some_and(|services| services
                .iter()
                .any(|service| service["serviceId"] == "editor-speech")),
            "configured speech service was not discovered: {services}"
        );
        let tools = client
            .call(
                id,
                "client_speech_tools",
                json!({"nodeDid":node_did,"serviceId":"editor-speech"}),
            )
            .await;
        id += 1;
        ensure!(
            tools.as_array().is_some_and(|tools| tools.len() == 2),
            "speech adapter tools were not discovered: {tools}"
        );
        Some(service)
    } else {
        None
    };
    let content =
        if speech.is_some() {
            let transcript = client.call(id, "client_speech_recognize", json!({"request": {
            "tool":{"nodeDid":node_did,"serviceId":"editor-speech","toolName":"transcribe"},
            "clip":{"pcm":"AAAAAA==","sampleRate":48000},"language":"en"
        }})).await;
            id += 1;
            transcript
                .as_str()
                .context("configured speech transcript")?
                .to_owned()
        } else {
            "Return the fixture response.".to_owned()
        };
    let receipt = if matches!(journey, Journey::Goal) {
        client.call(id, "desktop_goal_control", json!({"request": {
            "nodeDid":node_did, "agentId":agent_id,
            "action":{"kind":"create", "objective":"Return the fixture response.", "tokenBudget":"1000"}
        }})).await
    } else {
        client
            .call(
                id,
                "desktop_chat_send",
                json!({"request":{
                    "nodeDid":node_did,"agentId":agent_id,"content":content,"delivery":"queue"
                }}),
            )
            .await
    };
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
            let projection = read_session(&mut client, &mut id, node_did, session).await?;
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
    if matches!(journey, Journey::Goal) {
        let mut view = wait_goal(&mut client, &mut id, node_did, session, "active").await?;
        ensure!(view["goal"]["goalId"] == receipt["goalId"]);
        for status in ["paused", "active", "paused"] {
            let result = client
                .call(
                    id,
                    "desktop_goal_control",
                    json!({"request": {
                        "nodeDid":node_did, "sessionId":session,
                        "sessionDocId":view["sessionDocId"], "expected":view["goal"]["revision"],
                        "action":{"kind":if status == "paused" { "pause" } else { "resume" }}
                    }}),
                )
                .await;
            id += 1;
            ensure!(
                result["goalId"] == receipt["goalId"] && result["requestId"].is_null(),
                "a goal control while the current turn is running must not publish another request"
            );
            view = wait_goal(&mut client, &mut id, node_did, session, status).await?;
            let requests = access.execute(&format!(r#"{{ AgentRequest(filter: {{ node_did: {{_eq: "{}"}},
                session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}} }}) {{ lifecycle_state interrupt_requested_at }} }}"#,
                gents::graphql::escape_graphql_string(node_did), gents::graphql::escape_graphql_string(session))).await?;
            let rows = requests["data"]["AgentRequest"]
                .as_array()
                .context("goal request rows")?;
            ensure!(
                rows.len() == 1
                    && rows[0]["lifecycle_state"] == "processing"
                    && rows[0]["interrupt_requested_at"].is_null(),
                "goal controls must preserve the current owned turn"
            );
        }
        let projection = read_session(&mut client, &mut id, node_did, session).await?;
        ensure!(
            projection["sessionId"] == session
                && projection["nodeDid"] == node_did
                && projection["goal"].is_null(),
            "ordinary client routes must not replicate raw Goal documents"
        );
    }
    if matches!(journey, Journey::Editor) {
        let completion_path = root.join("finish-provider");
        let completion_gate = Arc::clone(&gate);
        let _completion = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            loop {
                if completion_path.is_file() {
                    completion_gate.add_permits(1);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }));
        configured_actions::seed_editor_settings(&access, node_did, agent_id).await?;
        client
            .call(id, "client_connect_local", json!({"nodeHome":node_home}))
            .await;
        let approval_home = node_home.clone();
        let approval_path = root.join("approval-fixture.txt");
        std::fs::write(&approval_path, "fixture access")?;
        let approval_session = session.to_owned();
        let approval_wait = tokio::spawn(async move {
            let mark = gents::plugin::approval::queue_dir(&approval_home).join(".listening");
            tokio::time::timeout(Duration::from_secs(90), async {
                loop {
                    if std::fs::metadata(&mark)
                        .and_then(|metadata| metadata.modified())
                        .is_ok_and(|modified| modified > std::time::UNIX_EPOCH)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .context("the editor did not open an approval listener")?;
            let resolved = gents::plugin::allowed::Resolved {
                target: approval_path,
                is_dir: false,
            };
            let question = gents::plugin::approval::Request::new(
                "fixture/files",
                &resolved,
                gents::pack::BindAccess::Read,
                Some(approval_session),
            );
            gents::plugin::approval::ask(&approval_home, &question, gents::plugin::approval::WAIT)
                .await
        });
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
            .env("GENTS_VSCODE_INFERENCE_ENDPOINT", provider.endpoint())
            .arg(script)
            .arg(root)
            .arg(paths.root())
            .arg(env!("CARGO_BIN_EXE_gents-client-host"))
            .arg(session)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .status();
        let status = tokio::time::timeout(Duration::from_secs(120), child).await;
        if !matches!(&status, Ok(Ok(status)) if status.success()) {
            approval_wait.abort();
        }
        let status = status.context("editor journey timed out")??;
        ensure!(status.success(), "editor journey exited with {status}");
        configured_actions::verify_editor_settings(&access, node_did, agent_id).await?;
        ensure!(tokio::time::timeout(Duration::from_secs(5), approval_wait)
            .await
            .context("the editor's approval did not reach the waiting call")???);
        ensure!(
            gents::plugin::allowed::list(&node_home)?.is_empty(),
            "allow-once must not persist a file or folder grant"
        );
        id += 1;
    }
    gate.add_permits(1);
    let mut last_projection = Value::Null;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let projection = read_session(&mut client, &mut id, node_did, session).await?;
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
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let output = client
                .call(
                    id,
                    "desktop_request_output",
                    json!({
                        "nodeDid":node_did, "sessionId":session, "requestId":request,
                    }),
                )
                .await;
            id += 1;
            ensure!(
                output["nodeDid"] == node_did
                    && output["sessionId"] == session
                    && output["requestId"] == request,
                "request output crossed the accepted request scope"
            );
            match output["output"]["kind"].as_str() {
                Some("terminalMessage") => {
                    ensure!(
                        output["output"]["content"] == "First part and final part.",
                        "{output}"
                    );
                    break Ok::<_, anyhow::Error>(());
                }
                Some("loading") => tokio::time::sleep(Duration::from_millis(50)).await,
                _ => anyhow::bail!("unexpected canonical output: {output}"),
            }
        }
    })
    .await
    .context("the exact request's terminal answer was not observed")??;
    let wrong_scope = client
        .try_call(
            id,
            "desktop_request_output",
            json!({
                "nodeDid":node_did,"sessionId":"another-session","requestId":request,
            }),
        )
        .await;
    id += 1;
    ensure!(
        wrong_scope.is_err()
            || wrong_scope
                .as_ref()
                .is_ok_and(|value| value["output"]["kind"] == "loading"),
        "a request's answer escaped its session scope"
    );
    if let Some(speech) = speech.as_ref() {
        let clip = client
            .call(
                id,
                "client_speech_synthesize",
                json!({"request":{
                    "tool":{"nodeDid":node_did,"serviceId":"editor-speech","toolName":"synthesize"},
                    "text":"First part and final part.","sampleRate":48000
                }}),
            )
            .await;
        id += 1;
        let clip: gents_protocol::client_media::MediaPcmClip = serde_json::from_value(clip)?;
        ensure!(clip.sample_rate == 48000 && clip.samples()? == [i16::MIN, i16::MAX]);
        let calls = speech.calls.lock().unwrap();
        ensure!(
            calls.len() == 2 && calls.iter().all(|(actor, _)| actor == &client_did),
            "speech calls must use the actual client actor exactly once per operation"
        );
        ensure!(
            calls[0].1["arguments"]["audio"]
                == json!({"data":"AAAAAA==","format":"pcm_s16le","sampleRate":48000,"channels":1})
        );
        ensure!(calls[1].1["arguments"]["text"] == "First part and final part.");
        ensure!(
            calls[1].1["arguments"].get("language").is_none()
                && calls[1].1["arguments"].get("voice").is_none()
        );
    }
    if matches!(journey, Journey::Goal) {
        wait_goal(&mut client, &mut id, node_did, session, "paused").await?;
        let requests = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ node_did: {{_eq: "{}"}},
            session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}} }}) {{ request_id }} }}"#,
                gents::graphql::escape_graphql_string(node_did),
                gents::graphql::escape_graphql_string(session)
            ))
            .await?;
        ensure!(
            requests["data"]["AgentRequest"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1),
            "the paused goal must finish its current turn without a continuation"
        );
    }
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
    if matches!(journey, Journey::Conversation) {
        configured_actions::exercise(
            &mut client,
            &mut id,
            &access,
            node_did,
            agent_id,
            &client_did,
        )
        .await?;
    }
    client.close().await;
    stop(&mut host).await?;
    stop(&mut runtime).await?;
    Ok(())
}

async fn read_session(
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    session: &str,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = client
                .try_call(
                    *id,
                    "desktop_session_snapshot",
                    json!({
                        "nodeDid":node_did, "sessionId":session, "timelineLimit":40
                    }),
                )
                .await;
            *id += 1;
            match result {
                Ok(projection) => return Ok(projection),
                Err(error)
                    if error.code == gents_desktop_bridge::BridgeErrorCode::ProjectionPending =>
                {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => return Err(anyhow::Error::from(error)),
            }
        }
    })
    .await
    .context("session projection did not reach a current read fence")?
}

async fn wait_goal(
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    session: &str,
    status: &str,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let view = client
                .call(
                    *id,
                    "desktop_session_goal",
                    json!({"nodeDid":node_did,"sessionId":session}),
                )
                .await;
            *id += 1;
            if view["nodeDid"] == node_did
                && view["sessionId"] == session
                && view["goal"]["status"] == status
            {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("the signed goal observation did not reach the client")
}

async fn configure_inference(
    client: &mut Attachment,
    id: &mut u32,
    node_did: &str,
    agent_id: &str,
    deployment: &Value,
    endpoint: &str,
) -> Result<()> {
    let catalog = client
        .call(*id, "client_inference_setup_catalog", json!({}))
        .await;
    *id += 1;
    ensure!(
        catalog["providers"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == "local")),
        "the native setup catalog has no local provider"
    );
    let discovery = client.call(*id, "client_inference_models_discover", json!({"request": {
        "requestKey":"editor-setup", "nodeDid":node_did, "provider":"local",
        "authMethod":"optional_api_key", "endpoint":endpoint, "apiKey":null, "accountRef":null,
    }})).await;
    *id += 1;
    ensure!(
        discovery["reachable"] == true
            && discovery["models"].as_array().is_some_and(|models| models
                .iter()
                .any(|model| model["advertised"]["model_name"] == "editor-fixture")),
        "native model discovery did not observe the provider: {discovery}"
    );
    let mut agent = deployment["agentConfigs"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["agent_id"] == agent_id))
        .context("the setup default agent configuration")?
        .clone();
    agent["inference_profile_id"] = json!("editor-setup");
    client.call(*id, "client_config_apply", json!({"request":{"document":{
        "node":{"node_did":node_did},
        "inference_backends":[{"node_did":node_did,"backend_id":"editor-setup",
            "name":"Editor setup", "provider_kind":discovery["providerKind"],
            "openai_wire_api":discovery["openaiWireApi"], "endpoint":discovery["effectiveEndpoint"],
            "auth":{"kind":"unauthenticated"},"max_concurrent":1,"max_queue_depth":100,"enabled":true}],
        "inference_profiles":[{"node_did":node_did,"profile_id":"editor-setup",
            "backend_id":"editor-setup","model_name":"editor-fixture"}],
        "agents":[agent],
    }}})).await;
    *id += 1;
    let snapshot = client.snapshot(*id).await;
    *id += 1;
    let observed = snapshot["client"]["deployments"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["nodeDid"] == node_did))
        .context("the configured runtime observation")?;
    ensure!(
        observed["agentConfigs"].as_array().is_some_and(|rows| rows
            .iter()
            .any(
                |row| row["agent_id"] == agent_id && row["inference_profile_id"] == "editor-setup"
            )),
        "the selected model was not bound to the default agent: {observed}"
    );
    Ok(())
}

#[cfg(unix)]
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

async fn wait_for_status(child: &mut Child, cli: &Path, home: &Path, port: u16) -> Result<Value> {
    let graphql = ports::graphql_url(port);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            ensure!(
                child.try_wait()?.is_none(),
                "runtime exited before readiness"
            );
            let output = Command::new(cli)
                .args(["status", "--home"])
                .arg(home)
                .arg("--graphql")
                .arg(&graphql)
                .kill_on_drop(true)
                .output()
                .await?;
            if output.status.success() {
                let ready: Value = serde_json::from_slice(&output.stdout)?;
                if ready["readiness_status"] == "ready" {
                    return Ok(ready);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
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
    #[cfg(unix)]
    {
        let pid = child.id().context("live fixture child")?;
        // SAFETY: the PID belongs to the live child owned by this fixture.
        ensure!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) } == 0);
    }
    #[cfg(windows)]
    child.interrupt()?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    ensure!(status.success(), "fixture child exited with {status}");
    Ok(())
}
