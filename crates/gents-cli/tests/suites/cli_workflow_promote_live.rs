use crate::support::*;

use gents::support::snapshots::SnapshotReader;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::Instant;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[allow(dead_code)]
#[path = "../support/failure_artifacts.rs"]
mod failure_artifacts;
use failure_artifacts::FailureArtifacts;

const PONG_SCRIPT: &str = r#"module.exports = (input) => {
  if (!input.pin) return { effects: { spawn: [{ slot: "ask", attempt: 0, target: "wf-read-only", prompt: "Reply with the single word pong and nothing else.", title: "ask" }] } };
  const ask = input.children["ask"];
  if (!ask || ask.state === "live") return { effects: {} };
  if (ask.state !== "completed") return { fail: { code: "no_answer", message: "the worker did not answer" } };
  const text = input.job_results && input.job_results.text && input.job_results.text.result.slots.ask;
  if (!text) return { job_calls: { requests: [{ id: "text", child_output: { slots: ["ask"], offset: 0, limit: input.ceiling.result_bytes } }] } };
  return { result: { reply: text.text } };
};"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET; requires javy and GENTS_DYNAMIC_WORKFLOWS_PACK"]
async fn a_dynamic_run_is_promoted_and_the_promoted_tool_runs_installed() -> Result<()> {
    let root = std::env::var_os("GENTS_WORKFLOW_PROMOTE_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gents-workflow-promote-failures"));
    let retain = std::env::var_os("GENTS_WORKFLOW_PROMOTE_RETAIN_FAILURES").as_deref()
        != Some(std::ffi::OsStr::new("0"));
    let mut artifacts = FailureArtifacts::new(&root, retain)?;
    let result = promote_a_dynamic_run(&mut artifacts).await;
    artifacts.finish(result)
}

async fn promote_a_dynamic_run(artifacts: &mut FailureArtifacts) -> Result<()> {
    let pack_dir = std::env::var("GENTS_DYNAMIC_WORKFLOWS_PACK").context(
        "GENTS_DYNAMIC_WORKFLOWS_PACK must point at a built packs/gents/dynamic_workflows",
    )?;
    let target = gents::support::live_inference::live_target().await;
    let model_endpoint = target.endpoint().to_owned();
    let model_name = target.model().to_owned();
    // The fixture is deleted on success; set this to keep the evidence.
    let evidence = std::env::var_os("GENTS_WORKFLOW_PROMOTE_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| artifacts.path().join("evidence"));
    fs::create_dir_all(&evidence)?;
    let home_dir = artifacts.path().join("agent-home");
    fs::create_dir_all(&home_dir)?;
    let home = home_dir.to_str().context("home path is not UTF-8")?;

    let mut init_args = vec![
        "--home".to_string(),
        home.to_string(),
        "--node-name".to_string(),
        format!("workflow-promote-live-{}", Uuid::new_v4().simple()),
        "--model-name".to_string(),
        model_name,
        "--max-concurrent".to_string(),
        "4".to_string(),
        "--max-queue-depth".to_string(),
        "8".to_string(),
    ];
    init_args.push("--inference-url".to_string());
    init_args.push(model_endpoint);
    let init_refs = init_args.iter().map(String::as_str).collect::<Vec<_>>();
    let init = run_init_json(&home_dir, &init_refs)?;
    let node_did = node_did_from_init(&init)?;
    let field = |pointer: &str| -> Result<String> {
        init.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("init receipt is missing {pointer}: {init}"))
    };
    let profile_id = field("/inference_profile_id")?;
    let agent_did = field("/init/default_agent_did")?;
    let tools_id = field("/init/tools_id")?;

    let install = run_cli_json(
        &home_dir,
        &[
            "config",
            "pack",
            "install",
            &pack_dir,
            "--home",
            home,
            "--node-did",
            &node_did,
            "--grant-authority",
            "--inference-slot",
            &format!("worker={profile_id}"),
        ],
    )?;
    write_json_file(&evidence.join("install.json"), &install)?;

    let wire = run_cli_text(
        &home_dir,
        &[
            "config",
            "pack",
            "wire",
            "gents/workflow",
            "--agent",
            &agent_did,
            "--targets-from",
            "gents/dynamic_workflows",
            "--home",
            home,
            "--node-did",
            &node_did,
        ],
    )?;
    fs::write(evidence.join("wire.txt"), &wire)?;
    allow_dynamic_runs(&home_dir, &tools_id, &evidence)?;
    let graphql = start_runtime(artifacts, &home_dir, &node_did).await?;

    let script = serde_json::to_string(PONG_SCRIPT)?;
    let first_session = Uuid::new_v4().to_string();
    let reply = chat(
        &home_dir,
        &first_session,
        &format!(
            "Call the workflow tool exactly once with agent_budget 2, args {{\"handle\": \"pong_flow\"}} and this exact script (a JSON string): {script}. Then end your turn."
        ),
    )?;
    write_json_file(&evidence.join("chat-workflow.json"), &reply)?;
    let dynamic = finished_job(&home_dir, &graphql, &first_session, &evidence, "dynamic").await?;
    anyhow::ensure!(
        dynamic.view.pointer("/pin/origin").and_then(Value::as_str) == Some("dynamic"),
        "the first run is not dynamic: {}",
        dynamic.view
    );
    anyhow::ensure!(
        has_reply(&dynamic.result),
        "the dynamic run did not retain a child reply: {}",
        dynamic.result
    );

    let server = artifacts.take_server().context("runtime was not started")?;
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            let output = server.captured_output();
            drop(server);
            output
        }),
    )
    .await
    .context("runtime did not stop before promotion")???;
    fs::write(evidence.join("dynamic-runtime.stdout"), output.0)?;
    fs::write(evidence.join("dynamic-runtime.stderr"), output.1)?;
    let promote = run_cli_json(
        &home_dir,
        &[
            "automation",
            "job",
            "promote",
            &dynamic.address,
            "--name",
            "pong_flow",
            "--home",
            home,
            "--output",
            "json",
        ],
    )?;
    write_json_file(&evidence.join("promote.json"), &promote)?;
    let record = read_json_file(&home_dir.join("plugins/installed/gents/pong_flow.json"))?;
    write_json_file(&evidence.join("pong_flow-record.json"), &record)?;
    anyhow::ensure!(
        record.pointer("/declaration/job").and_then(Value::as_bool) == Some(true),
        "the installed pong_flow record does not declare a job: {record}"
    );
    let graphql = start_runtime(artifacts, &home_dir, &node_did).await?;

    let second_session = Uuid::new_v4().to_string();
    let reply = chat(
        &home_dir,
        &second_session,
        "Call the pong_flow tool exactly once with agent_budget 2. Then end your turn.",
    )?;
    write_json_file(&evidence.join("chat-pong_flow.json"), &reply)?;
    let installed =
        finished_job(&home_dir, &graphql, &second_session, &evidence, "installed").await?;
    anyhow::ensure!(
        installed
            .view
            .pointer("/pin/origin")
            .and_then(Value::as_str)
            == Some("installed"),
        "the promoted run is not installed: {}",
        installed.view
    );
    anyhow::ensure!(
        has_reply(&installed.result),
        "the promoted run did not retain a child reply: {}",
        installed.result
    );
    Ok(())
}

async fn start_runtime(
    artifacts: &mut FailureArtifacts,
    home: &Path,
    node_did: &str,
) -> Result<String> {
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let home = home.to_owned();
    let (serve, readiness) = tokio::task::spawn_blocking(move || {
        let home_arg = home.to_str().context("home path is not UTF-8")?;
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &[("RUST_LOG", "warn")])
    })
    .await??;
    artifacts.attach_server(serve);
    anyhow::ensure!(
        readiness["status"] == "serving",
        "server did not become ready: {readiness}"
    );
    wait_for_runtime_ready(&graphql, node_did, Duration::from_secs(30)).await?;
    Ok(graphql)
}

/// The dynamic_workflows pack ships `policy.dynamic = "check"`, which refuses
/// every dynamic spawn; this run needs `run`.
fn allow_dynamic_runs(home_dir: &Path, tools_id: &str, evidence: &Path) -> Result<()> {
    let home = home_dir.to_str().context("home path is not UTF-8")?;
    let mut tools = run_cli_json(
        home_dir,
        &[
            "config", "tools", "show", "--id", tools_id, "--home", home, "--output", "json",
        ],
    )?;
    let object = tools
        .as_object_mut()
        .context("tools document is not an object")?;
    object.remove("_docID");
    let block = object
        .get_mut("integrations")
        .and_then(|integrations| integrations.get_mut("plugins"))
        .and_then(Value::as_array_mut)
        .and_then(|plugins| {
            plugins.iter_mut().find(|plugin| {
                plugin.get("plugin").and_then(Value::as_str) == Some("gents/workflow")
            })
        })
        .and_then(|plugin| plugin.pointer_mut("/job/policy"))
        .and_then(Value::as_object_mut)
        .context("tools document has no gents/workflow job policy; see tools-show.json")?;
    block.insert("dynamic".into(), "run".into());
    let file = evidence.join("tools-run.json");
    write_json_file(&file, &tools)?;
    run_cli_text(
        home_dir,
        &[
            "config",
            "tools",
            "set",
            "--file",
            file.to_str().context("tools file path is not UTF-8")?,
            "--home",
            home,
        ],
    )?;
    Ok(())
}

fn chat(home_dir: &Path, session_id: &str, prompt: &str) -> Result<Value> {
    let home = home_dir.to_str().context("home path is not UTF-8")?;
    run_cli_json(
        home_dir,
        &[
            "utils",
            "debug",
            "request",
            "submit",
            "--home",
            home,
            "--session-id",
            session_id,
            "--valid-until",
            "none",
            "--timeout-secs",
            "300",
            "--poll-secs",
            "1",
            "--content",
            prompt,
        ],
    )
}

struct FinishedJob {
    address: String,
    view: Value,
    result: Value,
}

/// Waits for the session's one job to end completed and reads its result off
/// the completion notice the job posts to its session.
async fn finished_job(
    home_dir: &Path,
    graphql: &str,
    session_id: &str,
    evidence: &Path,
    label: &str,
) -> Result<FinishedJob> {
    let home = home_dir.to_str().context("home path is not UTF-8")?;
    let jobs = run_cli_json(
        home_dir,
        &[
            "automation",
            "job",
            "list",
            "--session",
            session_id,
            "--state",
            "all",
            "--home",
            home,
            "--graphql",
            graphql,
            "--output",
            "json",
        ],
    )?;
    let jobs = jobs.as_array().context("job list is not an array")?;
    let address = jobs
        .first()
        .context("the live model did not start a job")?
        .get("address")
        .and_then(Value::as_str)
        .context("job has no address")?
        .to_owned();
    let terminal = run_cli_text(
        home_dir,
        &[
            "automation",
            "job",
            "watch",
            &address,
            "--until-terminal",
            "--home",
            home,
            "--graphql",
            graphql,
        ],
    )?;
    fs::write(evidence.join(format!("{label}-watch.json")), &terminal)?;
    let view = run_cli_json(
        home_dir,
        &[
            "automation",
            "job",
            "show",
            &address,
            "--home",
            home,
            "--graphql",
            graphql,
            "--output",
            "json",
        ],
    )?;
    write_json_file(&evidence.join(format!("{label}-job.json")), &view)?;
    anyhow::ensure!(
        view.get("state").and_then(Value::as_str) == Some("completed"),
        "job {address} did not complete: {view}"
    );
    let tool_call_id = view
        .get("tool_call_id")
        .and_then(Value::as_str)
        .context("job has no tool_call_id")?;
    let text = completion_result(graphql, session_id, tool_call_id).await?;
    fs::write(evidence.join(format!("{label}-result.txt")), &text)?;
    let result = serde_json::from_str(&text)
        .with_context(|| format!("job {address} result is not JSON: {text}"))?;
    Ok(FinishedJob {
        address,
        view,
        result,
    })
}

/// A background job's result reaches its session as a `<tool-completion>`
/// message, not as the tool call's reply (that is the receipt).
async fn completion_result(graphql: &str, session_id: &str, tool_call_id: &str) -> Result<String> {
    let access = gents_store::config_client::ConfigAccess::Graphql(
        crate::support::graphql::served_endpoint(graphql),
    );
    let mut changes = access.subscribe_changes("AgentMessage").await;
    let marker = format!("<tool-completion tool_call_id=\"{tool_call_id}\"");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let rows = graphql_query(
            graphql,
            &format!(
                r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID node_did requester_did }} }}"#,
                escape_graphql_string(session_id)
            ),
        )
        .await?;
        for row in rows
            .pointer("/data/AgentMessage")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let field = |name: &str| row.get(name).and_then(Value::as_str);
            let (Some(doc_id), Some(node_did)) = (field("_docID"), field("node_did")) else {
                continue;
            };
            let Ok((_, message)) = gents_session::session::load_canonical_message(
                &access,
                doc_id,
                node_did,
                field("requester_did"),
            )
            .await
            else {
                continue;
            };
            let Some(text) = message.rag_text().filter(|text| text.contains(&marker)) else {
                continue;
            };
            let result = text
                .split_once("<result>")
                .and_then(|(_, rest)| rest.split_once("</result>"))
                .map(|(result, _)| result)
                .with_context(|| format!("completion notice has no result: {text}"))?;
            return Ok(result
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&amp;", "&"));
        }
        if Instant::now() >= deadline {
            anyhow::bail!("session {session_id} never received the completion of {tool_call_id}");
        }
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

fn has_reply(result: &Value) -> bool {
    result
        .pointer("/reply")
        .and_then(Value::as_str)
        .is_some_and(|reply| !reply.trim().is_empty())
}
